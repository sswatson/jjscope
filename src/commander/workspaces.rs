/*!
[Commander] member functions related to jj workspaces.

Parses `jj workspace list` into [Workspace] records carrying the facts that
decide whether a workspace is safe to clean up, and wraps the commands the
[workspaces_tab][crate::ui::workspaces_tab] needs: snapshotting another
workspace, forgetting, `update-stale`, and deleting a workspace directory.

Two jj properties make cleanup tractable:

* Since 0.38 jj records each workspace's root directory, and the template's
  `root()` comes back empty once that directory is gone (or for workspaces
  created before the path was recorded). "The directory is missing" is a
  direct query, not a guess.
* `jj workspace forget` never loses tracked work. It abandons the working-copy
  commit only when that commit is empty and undescribed, and leaves any other
  one behind as an ordinary commit. Forgetting is also one operation, so a
  bulk forget is one `jj undo`.

The disk side is the only lossy part. Snapshotting a workspace before
forgetting it folds unsnapshotted edits into its commit, which leaves ignored
files as the only thing deleting the directory can destroy.
*/
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::RemoveEndLine;
use crate::commander::ids::ChangeId;
use crate::commander::ids::CommitId;
use crate::env::DiffFormat;

/// One jj workspace: its name, where it lives on disk, and the facts about its
/// working-copy commit that decide whether it is safe to clean up.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub name: String,
    /// Absolute root directory, if jj has one recorded and it still exists.
    /// jj only records roots for workspaces created since 0.38, and reports
    /// none once the directory is gone.
    pub root: Option<PathBuf>,
    pub change_id: ChangeId,
    pub commit_id: CommitId,
    /// The working-copy commit modifies no files.
    pub empty: bool,
    /// The working-copy commit has a description.
    pub described: bool,
    /// First line of the working-copy commit's description.
    pub description: String,
    /// Number of parents of the working-copy commit.
    pub parents: usize,
    /// How many of those parents are mutable. Zero means the workspace sits
    /// directly on immutable history, the textbook stray.
    pub mutable_parents: usize,
    /// This is the workspace jjscope is running in.
    pub current: bool,
    /// Committer timestamp of the working-copy commit. A poor "last activity"
    /// signal: jj bumps it whenever a parent is snapshotted or rebased.
    pub committer_timestamp: i64,
    /// Author timestamp of the working-copy commit: when the change was
    /// created, unaffected by rewrites.
    pub author_timestamp: i64,
    /// Modification time of the workspace's own on-disk state, i.e. the last
    /// time jj ran in that directory. `None` when there is no directory.
    pub last_touched: Option<i64>,
    /// The directory holds the repo store (`.jj/repo` is a directory), which
    /// makes it the main workspace. Deleting it would delete the repo.
    pub hosts_repo: bool,
    /// [Self::root] did not come from jj but from an earlier listing in this
    /// session, verified against the directory's own state. jj forgets a
    /// workspace's root for good when the workspace is forgotten, and an undo
    /// brings the workspace back without it.
    pub root_remembered: bool,
}

/// What cleaning up a workspace would amount to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkspaceState {
    /// No directory: deleted, or never recorded. Forgetting is all that is
    /// left to do.
    Ghost,
    /// Working copy empty and undescribed. Forgetting abandons it; nothing
    /// tracked is lost.
    Idle,
    /// Working copy has changes or a description. Forgetting leaves it in the
    /// log as an ordinary commit.
    HoldingWork,
}

impl WorkspaceState {
    /// Short badge text for list rows.
    pub fn label(self) -> &'static str {
        match self {
            WorkspaceState::Ghost => "ghost",
            WorkspaceState::Idle => "idle",
            WorkspaceState::HoldingWork => "work",
        }
    }
}

impl Workspace {
    pub fn state(&self) -> WorkspaceState {
        if self.root.is_none() {
            WorkspaceState::Ghost
        } else if self.discardable() {
            WorkspaceState::Idle
        } else {
            WorkspaceState::HoldingWork
        }
    }

    /// Whether jj would abandon the working-copy commit on forget: it is
    /// empty and has no description.
    pub fn discardable(&self) -> bool {
        self.empty && !self.described
    }

    /// The working copy sits directly on immutable history.
    pub fn on_immutable_base(&self) -> bool {
        self.parents > 0 && self.mutable_parents == 0
    }

    /// Whether the tab's "cleanup candidates" filter should show this
    /// workspace: a ghost or an idle one, and not the one jjscope runs in.
    pub fn is_candidate(&self) -> bool {
        !self.current && matches!(self.state(), WorkspaceState::Ghost | WorkspaceState::Idle)
    }

    /// Why this workspace must not be forgotten or deleted from the tab, if
    /// there is such a reason.
    pub fn protected_reason(&self) -> Option<&'static str> {
        if self.current {
            Some("it is the workspace jjscope is running in")
        } else if self.hosts_repo {
            Some("it holds the repo store")
        } else {
            None
        }
    }

    /// What `jj workspace forget` does to this workspace's commit.
    pub fn forget_effect(&self) -> String {
        if self.discardable() {
            "abandons its empty working-copy commit; nothing is lost".to_owned()
        } else {
            format!("keeps commit {} in the log", self.short_change_id())
        }
    }

    /// Best available "last activity" time: when jj last ran in the
    /// directory, falling back to the commit's rewrite time for ghosts.
    pub fn activity_timestamp(&self) -> i64 {
        self.last_touched.unwrap_or(self.committer_timestamp)
    }

    pub fn short_change_id(&self) -> &str {
        short_id(self.change_id.as_str())
    }

    pub fn short_commit_id(&self) -> &str {
        short_id(self.commit_id.as_str())
    }

    /// Fill in the facts that come from the filesystem rather than from jj.
    fn fill_disk_facts(&mut self) {
        let Some(root) = self.root.as_deref() else {
            return;
        };
        let jj_dir = root.join(".jj");
        self.hosts_repo = jj_dir.join("repo").is_dir();
        // `checkout` is rewritten whenever the workspace's working copy is
        // updated or snapshotted; the directory itself is the fallback for
        // an older layout.
        self.last_touched = [
            jj_dir.join("working_copy").join("checkout"),
            jj_dir.join("working_copy"),
        ]
        .iter()
        .find_map(|path| mtime_secs(path));
    }

    fn with_disk_facts(mut self) -> Self {
        self.fill_disk_facts();
        self
    }

    /// Use `root` as this workspace's directory when jj has lost the record,
    /// provided the directory's own jj state says it belongs to this
    /// workspace. Returns whether the root was adopted.
    pub fn adopt_remembered_root(&mut self, root: &Path) -> bool {
        if self.root.is_some() || checkout_workspace_name(root).as_deref() != Some(&self.name) {
            return false;
        }
        self.root = Some(root.to_owned());
        self.root_remembered = true;
        self.fill_disk_facts();
        true
    }
}

/// Restore roots jj has lost from `known` (name to root, as seen in earlier
/// listings). A remembered root is used only if no listed workspace claims it
/// and the directory's own state names the workspace; see
/// [Workspace::adopt_remembered_root].
pub fn recover_roots(workspaces: &mut [Workspace], known: &HashMap<String, PathBuf>) {
    let claimed: Vec<PathBuf> = workspaces.iter().filter_map(|ws| ws.root.clone()).collect();
    for ws in workspaces.iter_mut().filter(|ws| ws.root.is_none()) {
        if let Some(root) = known.get(&ws.name)
            && !claimed.contains(root)
        {
            ws.adopt_remembered_root(root);
        }
    }
}

/// The workspace name recorded in `<root>/.jj/working_copy/checkout`, jj's
/// own note of which workspace a directory belongs to.
///
/// The file is a protobuf message (`Checkout` in jj's `working_copy.proto`):
/// field 2 is the operation id and field 3 the workspace name. Only the
/// length-delimited field 3 is wanted, so this walks the wire format rather
/// than pulling in a protobuf crate.
pub fn checkout_workspace_name(root: &Path) -> Option<String> {
    let bytes = fs::read(root.join(".jj").join("working_copy").join("checkout")).ok()?;
    let mut i = 0;
    while i < bytes.len() {
        let (key, n) = read_varint(&bytes[i..])?;
        i += n;
        let field = key >> 3;
        match key & 7 {
            0 => {
                let (_, n) = read_varint(&bytes[i..])?;
                i += n;
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let (len, n) = read_varint(&bytes[i..])?;
                i += n;
                let end = i.checked_add(usize::try_from(len).ok()?)?;
                if end > bytes.len() {
                    return None;
                }
                if field == 3 {
                    return String::from_utf8(bytes[i..end].to_vec()).ok();
                }
                i = end;
            }
            _ => return None,
        }
    }
    None
}

/// Decode a protobuf varint, returning the value and the bytes consumed.
fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, byte) in bytes.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

fn short_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
    &id[..end]
}

fn mtime_secs(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// Template for `jj workspace list`, one tab-separated line per workspace.
///
/// The name goes through `json()` because a `RefSymbol` renders quoted when it
/// contains characters that are not valid in a bare revset symbol (a space,
/// say), and the tab needs the exact name to hand back to `workspace forget`.
/// The root path and the description first line are rendered raw; the
/// description comes last so it may contain tabs without upsetting the split.
const WORKSPACE_TEMPLATE: &str = r#"json(name) ++ "\t" ++ if(root, root.absolute(), "") ++ "\t" ++ target.change_id() ++ "\t" ++ target.commit_id() ++ "\t" ++ target.empty() ++ "\t" ++ if(target.description(), "true", "false") ++ "\t" ++ target.parents().len() ++ "\t" ++ target.parents().filter(|p| !p.immutable()).len() ++ "\t" ++ target.current_working_copy() ++ "\t" ++ target.committer().timestamp().format("%s") ++ "\t" ++ target.author().timestamp().format("%s") ++ "\t" ++ target.description().first_line() ++ "\n""#;

/// Number of tab-separated fields [WORKSPACE_TEMPLATE] produces. The last one
/// is the description, which may itself contain tabs.
const WORKSPACE_FIELDS: usize = 12;

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Parse one line of [WORKSPACE_TEMPLATE] output. The filesystem-derived
/// fields are left empty; see [Workspace::with_disk_facts].
fn parse_workspace_line(line: &str) -> Option<Workspace> {
    let mut fields = line.splitn(WORKSPACE_FIELDS, '\t');
    let name: String = serde_json::from_str(fields.next()?).ok()?;
    let root = fields.next()?;
    let root = if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    };
    let change_id = ChangeId(fields.next()?.to_owned());
    let commit_id = CommitId(fields.next()?.to_owned());
    let empty = parse_bool(fields.next()?)?;
    let described = parse_bool(fields.next()?)?;
    let parents = fields.next()?.parse().ok()?;
    let mutable_parents = fields.next()?.parse().ok()?;
    let current = parse_bool(fields.next()?)?;
    let committer_timestamp = fields.next()?.parse().ok()?;
    let author_timestamp = fields.next()?.parse().ok()?;
    let description = fields.next().unwrap_or("").to_owned();

    Some(Workspace {
        name,
        root,
        change_id,
        commit_id,
        empty,
        described,
        description,
        parents,
        mutable_parents,
        current,
        committer_timestamp,
        author_timestamp,
        last_touched: None,
        hosts_repo: false,
        root_remembered: false,
    })
}

impl Commander {
    /// List every workspace of the repo with its cleanup-relevant facts.
    /// Maps to `jj workspace list -T <template>`
    ///
    /// Fails rather than skipping a line it cannot parse: a workspace that
    /// silently vanished from a cleanup tab is worse than an error.
    #[instrument(level = "trace", skip(self))]
    pub fn get_workspaces(&self) -> Result<Vec<Workspace>, CommandError> {
        self.jj(["workspace", "list", "-T", WORKSPACE_TEMPLATE])
            .run()?
            .lines()
            .map(|line| {
                parse_workspace_line(line)
                    .map(Workspace::with_disk_facts)
                    .ok_or_else(|| {
                        CommandError::Status(
                            format!("Unexpected line in the workspace listing: {line:?}"),
                            None,
                        )
                    })
            })
            .collect()
    }

    /// Show a workspace's working-copy commit.
    /// Maps to `jj show <commit> --ignore-working-copy`
    ///
    /// Addressed by commit id rather than `<name>@`, which would need revset
    /// quoting for unusual names.
    #[instrument(level = "trace", skip(self))]
    pub fn get_workspace_show(
        &self,
        workspace: &Workspace,
        diff_format: &DiffFormat,
    ) -> Result<String, CommandError> {
        let mut args = vec!["show", workspace.commit_id.as_str()];
        args.append(&mut diff_format.get_args());
        args.push("--ignore-working-copy");

        // Rendered, never parsed -- see [JjCommand::run_lossy].
        Ok(self.jj(args).color().run_lossy()?.remove_end_line())
    }

    /// Snapshot another workspace's working copy, so edits made there since
    /// jj last ran in it land in its working-copy commit.
    /// Maps to `jj -R <root> status`
    ///
    /// Run before forgetting: it is what makes forgetting lossless for
    /// everything but ignored files.
    #[instrument(level = "trace", skip(self))]
    pub fn snapshot_workspace(&self, root: &Path) -> Result<(), CommandError> {
        self.jj([OsStr::new("-R"), root.as_os_str(), OsStr::new("status")])
            .run_void()
    }

    /// Stop tracking workspaces in the repo. Their directories are left alone.
    /// Maps to `jj workspace forget <names>...`
    ///
    /// A no-op for an empty list: bare `jj workspace forget` would forget the
    /// *current* workspace, which is never what a bulk action means.
    #[instrument(level = "trace", skip(self))]
    pub fn forget_workspaces(&self, names: &[String]) -> Result<(), CommandError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut args = vec!["workspace".to_owned(), "forget".to_owned()];
        args.extend(names.iter().cloned());
        self.jj(args).run_void()
    }

    /// Bring a workspace whose working-copy commit was rewritten from
    /// elsewhere back in sync with the repo. Returns jj's report.
    /// Maps to `jj -R <root> workspace update-stale`
    #[instrument(level = "trace", skip(self))]
    pub fn update_stale_workspace(&self, root: &Path) -> Result<String, CommandError> {
        let (stdout, stderr) = self
            .jj([
                OsStr::new("-R"),
                root.as_os_str(),
                OsStr::new("workspace"),
                OsStr::new("update-stale"),
            ])
            .verbose()
            .run_with_stderr()?;
        Ok(format!("{stdout}{stderr}").trim().to_owned())
    }

    /// Delete a workspace's directory. Returns the path that was removed.
    ///
    /// Refuses anything that is not plainly a disposable secondary workspace:
    /// the current workspace, the one holding the repo store, a directory
    /// without a `.jj` in it, or any directory that contains either the
    /// current workspace or the repo store. Forget the workspace first; this
    /// only touches the disk.
    #[instrument(level = "trace", skip(self))]
    pub fn remove_workspace_dir(&self, workspace: &Workspace) -> Result<PathBuf> {
        let name = &workspace.name;
        let Some(root) = workspace.root.as_deref() else {
            bail!("Workspace {name} has no directory to delete");
        };
        if let Some(reason) = workspace.protected_reason() {
            bail!("Refusing to delete workspace {name}: {reason}");
        }
        if !root.is_absolute() {
            bail!(
                "Refusing to delete workspace {name}: its path is not absolute ({})",
                root.display()
            );
        }
        let jj_dir = root.join(".jj");
        if !jj_dir.is_dir() {
            bail!(
                "Refusing to delete {}: it has no .jj directory, so it is not a workspace root",
                root.display()
            );
        }
        if jj_dir.join("repo").is_dir() {
            bail!(
                "Refusing to delete {}: it holds the repo store",
                root.display()
            );
        }

        let root = fs::canonicalize(root)
            .with_context(|| format!("Resolving workspace directory {}", root.display()))?;
        let here = fs::canonicalize(&self.env.root)
            .with_context(|| format!("Resolving the current workspace {}", self.env.root))?;
        if here.starts_with(&root) {
            bail!(
                "Refusing to delete {}: it contains the workspace jjscope is running in",
                root.display()
            );
        }
        let repo_dir = self.resolve_repo_dir()?;
        if repo_dir.starts_with(&root) {
            bail!(
                "Refusing to delete {}: it contains the repo store",
                root.display()
            );
        }

        fs::remove_dir_all(&root)
            .with_context(|| format!("Deleting workspace directory {}", root.display()))?;
        Ok(root)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use tempfile::TempDir;

    use super::*;
    use crate::commander::tests::TestRepo;

    /// A test repo with a second workspace in a sibling temp directory.
    struct TwoWorkspaces {
        repo: TestRepo,
        /// Owns the directory the second workspace was created in.
        _dir: TempDir,
        two_root: PathBuf,
    }

    impl TwoWorkspaces {
        fn new() -> Result<Self> {
            let repo = TestRepo::new()?;
            let dir = TempDir::with_prefix("jjscope-ws")?;
            let two_root = dir.path().join("two");
            repo.commander
                .jj([
                    OsStr::new("workspace"),
                    OsStr::new("add"),
                    OsStr::new("--name"),
                    OsStr::new("two"),
                    two_root.as_os_str(),
                ])
                .run_void()?;
            Ok(Self {
                repo,
                _dir: dir,
                two_root,
            })
        }

        fn workspaces(&self) -> Result<Vec<Workspace>> {
            Ok(self.repo.commander.get_workspaces()?)
        }

        fn find(&self, name: &str) -> Result<Workspace> {
            self.workspaces()?
                .into_iter()
                .find(|ws| ws.name == name)
                .ok_or_else(|| anyhow::anyhow!("no workspace named {name}"))
        }

        fn names(&self) -> Result<Vec<String>> {
            Ok(self.workspaces()?.into_iter().map(|ws| ws.name).collect())
        }

        /// Whether a commit is still visible in the repo.
        fn is_visible(&self, commit_id: &CommitId) -> Result<bool> {
            let visible = self
                .repo
                .commander
                .jj([
                    "log",
                    "-r",
                    "all()",
                    "--no-graph",
                    "-T",
                    r#"commit_id ++ "\n""#,
                ])
                .run()?;
            Ok(visible.lines().any(|line| line == commit_id.as_str()))
        }
    }

    fn sample(name: &str) -> Workspace {
        Workspace {
            name: name.to_owned(),
            root: Some(PathBuf::from("/tmp/ws")),
            change_id: ChangeId("kkmpptxzrspxrzommnulwmwkkqwworpl".to_owned()),
            commit_id: CommitId("0123456789abcdef0123456789abcdef01234567".to_owned()),
            empty: true,
            described: false,
            description: String::new(),
            parents: 1,
            mutable_parents: 0,
            current: false,
            committer_timestamp: 1_000,
            author_timestamp: 500,
            last_touched: Some(2_000),
            hosts_repo: false,
            root_remembered: false,
        }
    }

    #[test]
    fn parse_line_with_root() {
        let line = "\"two\"\t/tmp/ws/two\tkkmpptxzrspxrzommnulwmwkkqwworpl\t0123456789abcdef0123456789abcdef01234567\ttrue\tfalse\t1\t0\tfalse\t1700000000\t1690000000\t";
        let ws = parse_workspace_line(line).expect("line should parse");
        assert_eq!(ws.name, "two");
        assert_eq!(ws.root.as_deref(), Some(Path::new("/tmp/ws/two")));
        assert_eq!(ws.short_change_id(), "kkmpptxz");
        assert_eq!(ws.short_commit_id(), "01234567");
        assert!(ws.empty);
        assert!(!ws.described);
        assert_eq!(ws.description, "");
        assert_eq!(ws.parents, 1);
        assert_eq!(ws.mutable_parents, 0);
        assert!(!ws.current);
        assert_eq!(ws.committer_timestamp, 1_700_000_000);
        assert_eq!(ws.author_timestamp, 1_690_000_000);
        assert!(ws.on_immutable_base());
        assert_eq!(ws.state(), WorkspaceState::Idle);
        assert!(ws.is_candidate());
    }

    #[test]
    fn parse_line_without_root_and_with_tabbed_description() {
        // A quoted name with a space, no root, and a description containing a
        // tab: the name must round-trip exactly and the description must keep
        // everything after the fixed fields.
        let line = "\"my ws\"\t\tkkmpptxzrspxrzommnulwmwkkqwworpl\t0123456789abcdef0123456789abcdef01234567\tfalse\ttrue\t2\t1\tfalse\t1700000000\t1690000000\tfix\tthe thing";
        let ws = parse_workspace_line(line).expect("line should parse");
        assert_eq!(ws.name, "my ws");
        assert_eq!(ws.root, None);
        assert!(!ws.empty);
        assert!(ws.described);
        assert_eq!(ws.description, "fix\tthe thing");
        assert_eq!(ws.parents, 2);
        assert_eq!(ws.mutable_parents, 1);
        assert!(!ws.on_immutable_base());
        assert_eq!(ws.state(), WorkspaceState::Ghost);
        assert!(
            ws.is_candidate(),
            "a ghost is a candidate even if it holds work"
        );
        assert_eq!(ws.forget_effect(), "keeps commit kkmpptxz in the log");
    }

    #[test]
    fn parse_line_rejects_garbage() {
        assert!(parse_workspace_line("").is_none());
        assert!(parse_workspace_line("default: . abcd1234 (empty) (no description set)").is_none());
        assert!(
            parse_workspace_line("\"two\"\t/tmp\tid\tid\tmaybe\tfalse\t1\t0\tfalse\t1\t1\t")
                .is_none()
        );
    }

    #[test]
    fn state_and_protection_rules() {
        let idle = sample("idle");
        assert_eq!(idle.state(), WorkspaceState::Idle);
        assert!(idle.discardable());
        assert_eq!(idle.protected_reason(), None);
        assert_eq!(idle.activity_timestamp(), 2_000, "prefers the on-disk time");

        let described = Workspace {
            described: true,
            ..sample("described")
        };
        assert_eq!(described.state(), WorkspaceState::HoldingWork);
        assert!(!described.is_candidate(), "a description is worth keeping");

        let dirty = Workspace {
            empty: false,
            ..sample("dirty")
        };
        assert_eq!(dirty.state(), WorkspaceState::HoldingWork);

        let ghost = Workspace {
            root: None,
            last_touched: None,
            ..sample("ghost")
        };
        assert_eq!(ghost.state(), WorkspaceState::Ghost);
        assert_eq!(
            ghost.activity_timestamp(),
            1_000,
            "falls back to the commit"
        );

        let current = Workspace {
            current: true,
            ..sample("current")
        };
        assert!(!current.is_candidate());
        assert!(current.protected_reason().is_some());

        let main = Workspace {
            hosts_repo: true,
            ..sample("main")
        };
        assert!(main.protected_reason().is_some());
    }

    #[test]
    fn list_workspaces_reports_roots_and_current() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let workspaces = two.workspaces()?;
        assert_eq!(two.names()?, vec!["default", "two"], "got {workspaces:?}");

        let default = two.find("default")?;
        assert!(default.current);
        assert!(default.hosts_repo, "the main workspace holds .jj/repo");
        assert!(default.protected_reason().is_some());
        assert_eq!(
            default.root.as_deref().map(fs::canonicalize).transpose()?,
            Some(fs::canonicalize(two.repo.directory.path())?)
        );
        assert!(default.last_touched.is_some());

        let second = two.find("two")?;
        assert!(!second.current);
        assert!(
            !second.hosts_repo,
            "a secondary workspace's .jj/repo is a pointer file"
        );
        assert_eq!(
            second.root.as_deref().map(fs::canonicalize).transpose()?,
            Some(fs::canonicalize(&two.two_root)?)
        );
        assert!(second.empty);
        assert!(!second.described);
        assert_eq!(second.state(), WorkspaceState::Idle);
        assert!(second.last_touched.is_some());
        Ok(())
    }

    #[test]
    fn forget_abandons_a_discardable_working_copy() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let second = two.find("two")?;
        assert!(two.is_visible(&second.commit_id)?);

        two.repo.commander.forget_workspaces(&["two".to_owned()])?;

        assert_eq!(two.names()?, vec!["default"]);
        assert!(
            !two.is_visible(&second.commit_id)?,
            "an empty, undescribed working copy is abandoned on forget"
        );
        // The directory is untouched.
        assert!(two.two_root.join(".jj").is_dir());
        Ok(())
    }

    #[test]
    fn snapshot_then_forget_keeps_the_work() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        fs::write(two.two_root.join("notes.txt"), b"unsnapshotted edit\n")?;

        // The repo does not know about the edit until the workspace is
        // snapshotted from its own directory.
        assert!(two.find("two")?.empty);
        two.repo.commander.snapshot_workspace(&two.two_root)?;
        let second = two.find("two")?;
        assert!(!second.empty);
        assert_eq!(second.state(), WorkspaceState::HoldingWork);

        two.repo.commander.forget_workspaces(&["two".to_owned()])?;
        assert_eq!(two.names()?, vec!["default"]);
        assert!(
            two.is_visible(&second.commit_id)?,
            "a working copy with changes survives forget as a plain commit"
        );
        Ok(())
    }

    #[test]
    fn forget_with_no_names_is_a_no_op() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        two.repo.commander.forget_workspaces(&[])?;
        assert_eq!(two.names()?, vec!["default", "two"]);
        Ok(())
    }

    #[test]
    fn deleted_directory_makes_a_ghost_that_can_be_forgotten() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        fs::remove_dir_all(&two.two_root)?;

        let second = two.find("two")?;
        assert_eq!(
            second.root, None,
            "jj drops the root once the directory is gone"
        );
        assert_eq!(second.state(), WorkspaceState::Ghost);
        assert_eq!(second.last_touched, None);

        two.repo.commander.forget_workspaces(&["two".to_owned()])?;
        assert_eq!(two.names()?, vec!["default"]);
        Ok(())
    }

    #[test]
    fn remove_workspace_dir_deletes_a_secondary_workspace() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let second = two.find("two")?;

        two.repo.commander.forget_workspaces(&["two".to_owned()])?;
        let removed = two.repo.commander.remove_workspace_dir(&second)?;
        assert_eq!(
            removed,
            fs::canonicalize(&two.two_root).unwrap_or(removed.clone())
        );
        assert!(!two.two_root.exists());
        Ok(())
    }

    #[test]
    fn remove_workspace_dir_refuses_the_main_and_current_workspace() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let default = two.find("default")?;

        let err = two
            .repo
            .commander
            .remove_workspace_dir(&default)
            .expect_err("must not delete the main workspace");
        assert!(err.to_string().contains("Refusing"), "got: {err}");
        assert!(two.repo.directory.path().join(".jj").is_dir());

        // Even with the flags cleared, the directory itself gives it away.
        let disguised = Workspace {
            current: false,
            hosts_repo: false,
            ..default
        };
        let err = two
            .repo
            .commander
            .remove_workspace_dir(&disguised)
            .expect_err("must not delete the directory holding .jj/repo");
        assert!(err.to_string().contains("repo store"), "got: {err}");
        assert!(two.repo.directory.path().join(".jj").is_dir());
        Ok(())
    }

    #[test]
    fn remove_workspace_dir_refuses_non_workspace_directories() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let plain = TempDir::with_prefix("jjscope-plain")?;
        fs::write(plain.path().join("keep.txt"), b"not a workspace\n")?;

        let bogus = Workspace {
            root: Some(plain.path().to_owned()),
            ..two.find("two")?
        };
        let err = two
            .repo
            .commander
            .remove_workspace_dir(&bogus)
            .expect_err("must not delete a directory without .jj");
        assert!(err.to_string().contains(".jj"), "got: {err}");
        assert!(plain.path().join("keep.txt").exists());

        let ghost = Workspace {
            root: None,
            ..two.find("two")?
        };
        assert!(two.repo.commander.remove_workspace_dir(&ghost).is_err());
        Ok(())
    }

    #[test]
    fn checkout_file_names_its_workspace() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        assert_eq!(
            checkout_workspace_name(&two.two_root).as_deref(),
            Some("two")
        );
        assert_eq!(
            checkout_workspace_name(two.repo.directory.path()).as_deref(),
            Some("default")
        );
        assert_eq!(checkout_workspace_name(Path::new("/nonexistent")), None);
        Ok(())
    }

    #[test]
    fn checkout_parser_walks_the_wire_format() -> Result<()> {
        // Field 2 (operation id, 3 bytes), an unrelated varint field 4, then
        // field 3 with the name.
        let dir = TempDir::with_prefix("jjscope-checkout")?;
        let wc = dir.path().join(".jj").join("working_copy");
        fs::create_dir_all(&wc)?;
        let mut bytes = vec![0x12, 0x03, 0xaa, 0xbb, 0xcc, 0x20, 0x81, 0x01];
        bytes.extend([0x1a, 0x05]);
        bytes.extend(b"my ws");
        fs::write(wc.join("checkout"), &bytes)?;
        assert_eq!(
            checkout_workspace_name(dir.path()).as_deref(),
            Some("my ws")
        );

        // Truncated length: no name rather than a panic.
        fs::write(wc.join("checkout"), [0x1a, 0x09, b'x'])?;
        assert_eq!(checkout_workspace_name(dir.path()), None);
        Ok(())
    }

    #[test]
    fn undo_of_forget_loses_the_root_and_recovery_restores_it() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let before = two.find("two")?;
        let known: HashMap<String, PathBuf> = [("two".to_owned(), before.root.clone().unwrap())]
            .into_iter()
            .collect();

        two.repo.commander.forget_workspaces(&["two".to_owned()])?;
        two.repo.commander.run_undo()?;

        // jj brings the workspace back, but not its recorded root.
        let mut workspaces = two.workspaces()?;
        let restored = workspaces.iter().find(|ws| ws.name == "two").unwrap();
        assert_eq!(
            restored.root, None,
            "jj is expected to lose the root; if this fails, jj now keeps it and the recovery is unnecessary"
        );
        assert_eq!(restored.state(), WorkspaceState::Ghost);

        recover_roots(&mut workspaces, &known);
        let recovered = workspaces.iter().find(|ws| ws.name == "two").unwrap();
        assert_eq!(recovered.root, before.root);
        assert!(recovered.root_remembered);
        assert_eq!(recovered.state(), WorkspaceState::Idle);
        assert!(recovered.last_touched.is_some(), "disk facts are filled in");

        // With the directory renamed the remembered path is stale: stay a ghost.
        let moved = two.two_root.with_file_name("elsewhere");
        fs::rename(&two.two_root, &moved)?;
        let mut workspaces = two.workspaces()?;
        recover_roots(&mut workspaces, &known);
        assert_eq!(
            workspaces.iter().find(|ws| ws.name == "two").unwrap().root,
            None
        );
        Ok(())
    }

    #[test]
    fn recovery_refuses_a_directory_that_belongs_to_another_workspace() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let ghost = Workspace {
            root: None,
            ..sample("impostor")
        };
        let known: HashMap<String, PathBuf> = [("impostor".to_owned(), two.two_root.clone())]
            .into_iter()
            .collect();

        // The directory's state says "two", so "impostor" cannot adopt it...
        let mut workspaces = vec![ghost.clone()];
        recover_roots(&mut workspaces, &known);
        assert_eq!(workspaces[0].root, None);

        // ...and even a matching name is refused while a listed workspace
        // claims the same root.
        let known: HashMap<String, PathBuf> = [("two".to_owned(), two.two_root.clone())]
            .into_iter()
            .collect();
        let mut workspaces = vec![
            Workspace {
                root: None,
                ..sample("two")
            },
            Workspace {
                root: Some(two.two_root.clone()),
                ..sample("claimant")
            },
        ];
        recover_roots(&mut workspaces, &known);
        assert_eq!(workspaces[0].root, None);
        Ok(())
    }

    #[test]
    fn update_stale_reports_on_a_fresh_workspace() -> Result<()> {
        let two = TwoWorkspaces::new()?;
        let report = two.repo.commander.update_stale_workspace(&two.two_root)?;
        assert!(
            report.contains("not stale"),
            "expected jj to say the workspace is not stale, got: {report:?}"
        );
        Ok(())
    }
}
