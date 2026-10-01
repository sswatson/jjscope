/*!
[Commander] member functions related to Git remotes.

A remote is described by its name and URL (`jj git remote list`) plus how its
bookmarks relate to the local ones, which `jj bookmark list --all-remotes`
reports through the `tracked` and `tracking_*_count` template keywords.

The counts are not always exact: jj computes ahead/behind counts lazily and
gives up on very long histories, reporting only a lower bound. [Count] keeps
that distinction so the UI can show `10+` rather than a wrong `10`.

It is mostly used in the [remotes_tab][crate::ui::remotes_tab] module.
*/
use std::collections::HashMap;
use std::fmt::Display;

use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::RemoveEndLine;
use crate::commander::gh_account::PushIdentity;

/// A commit count that may only be a lower bound.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Count {
    pub lower: usize,
    /// Whether `lower` is the true count, not just a lower bound.
    pub exact: bool,
}

impl Count {
    fn exactly(n: usize) -> Self {
        Self {
            lower: n,
            exact: true,
        }
    }

    /// Add another count. The sum is exact only if both terms are.
    fn add(self, other: Count) -> Count {
        Count {
            lower: self.lower + other.lower,
            exact: self.exact && other.exact,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.lower == 0 && self.exact
    }
}

impl Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.lower, if self.exact { "" } else { "+" })
    }
}

/// How a remote's bookmarks compare to the local repo's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteStats {
    /// Bookmarks present on the remote, as of the last fetch.
    pub bookmarks: usize,
    /// Of those, the ones a local bookmark tracks.
    pub tracked: usize,
    /// Tracked bookmarks whose local and remote targets differ.
    pub out_of_sync: usize,
    /// Commits the local bookmarks have that the remote's do not (what a push
    /// would send), summed over the tracked bookmarks.
    pub ahead: Count,
    /// Commits the remote's bookmarks have that the local ones do not (what a
    /// fetch already brought in but the local bookmarks have not taken), summed
    /// over the tracked bookmarks.
    pub behind: Count,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub stats: RemoteStats,
}

impl Remote {
    pub fn untracked(&self) -> usize {
        self.stats.bookmarks - self.stats.tracked
    }
}

// Template for one line per remote bookmark:
// `name TAB remote TAB tracked TAB ahead_lo,ahead_hi,behind_lo,behind_hi`
// with the counts replaced by `-` when nothing tracks the bookmark. An
// unknown upper bound renders as an empty field.
const REMOTE_BOOKMARK_TEMPLATE: &str = r#"if(remote && present, name ++ "\t" ++ remote ++ "\t" ++ tracked ++ "\t" ++ if(tracking_present, tracking_ahead_count.lower() ++ "," ++ tracking_ahead_count.upper() ++ "," ++ tracking_behind_count.lower() ++ "," ++ tracking_behind_count.upper(), "-") ++ "\n")"#;

/// One parsed line of [REMOTE_BOOKMARK_TEMPLATE] output.
#[derive(Debug, PartialEq)]
struct RemoteBookmark {
    remote: String,
    tracked: bool,
    /// `(ahead, behind)` of the *local* bookmark relative to this one, when a
    /// local bookmark tracks it. jj reports the counts from the remote ref's
    /// point of view (`@origin (behind by 2 commits)` means the local bookmark
    /// has two commits the remote lacks), so [parse_remote_bookmark] swaps them.
    tracking: Option<(Count, Count)>,
}

fn parse_count(lower: &str, upper: &str) -> Option<Count> {
    let lower = lower.parse::<usize>().ok()?;
    Some(Count {
        lower,
        exact: upper.parse::<usize>().ok() == Some(lower),
    })
}

fn parse_remote_bookmark(line: &str) -> Option<RemoteBookmark> {
    let mut fields = line.split('\t');
    let _name = fields.next()?;
    let remote = fields.next()?.to_owned();
    let tracked = fields.next()? == "true";
    let counts = fields.next()?;
    let tracking = if counts == "-" {
        None
    } else {
        let mut parts = counts.split(',');
        let remote_ahead = parse_count(parts.next()?, parts.next()?)?;
        let remote_behind = parse_count(parts.next()?, parts.next()?)?;
        Some((remote_behind, remote_ahead))
    };
    Some(RemoteBookmark {
        remote,
        tracked,
        tracking,
    })
}

/// Fold the per-bookmark lines into per-remote statistics.
fn aggregate(lines: &str) -> HashMap<String, RemoteStats> {
    let mut stats: HashMap<String, RemoteStats> = HashMap::new();
    for bookmark in lines.lines().filter_map(parse_remote_bookmark) {
        let entry = stats.entry(bookmark.remote).or_insert_with(|| RemoteStats {
            ahead: Count::exactly(0),
            behind: Count::exactly(0),
            ..RemoteStats::default()
        });
        entry.bookmarks += 1;
        if bookmark.tracked {
            entry.tracked += 1;
        }
        if let Some((ahead, behind)) = bookmark.tracking {
            if !ahead.is_zero() || !behind.is_zero() {
                entry.out_of_sync += 1;
            }
            entry.ahead = entry.ahead.add(ahead);
            entry.behind = entry.behind.add(behind);
        }
    }
    stats
}

/// Parse `jj git remote list`, which prints `<name> <url>` per line.
pub(crate) fn parse_remote_list(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (name, url) = line.split_once(' ')?;
            Some((name.to_owned(), url.trim().to_owned()))
        })
        .collect()
}

impl Commander {
    /// Get the Git remotes with their bookmark statistics.
    /// Maps to `jj git remote list` and `jj bookmark list --all-remotes`
    #[instrument(level = "trace", skip(self))]
    pub fn get_remotes(&self) -> Result<Vec<Remote>, CommandError> {
        let remotes = parse_remote_list(&self.jj(["git", "remote", "list"]).run()?);
        if remotes.is_empty() {
            return Ok(vec![]);
        }

        let lines = self
            .jj([
                "bookmark",
                "list",
                "--all-remotes",
                "-T",
                REMOTE_BOOKMARK_TEMPLATE,
            ])
            .run()?;
        let mut stats = aggregate(&lines);

        Ok(remotes
            .into_iter()
            .map(|(name, url)| Remote {
                stats: stats.remove(&name).unwrap_or_else(|| RemoteStats {
                    ahead: Count::exactly(0),
                    behind: Count::exactly(0),
                    ..RemoteStats::default()
                }),
                name,
                url,
            })
            .collect())
    }

    /// The remote's bookmarks as jj prints them, with sync status.
    /// Maps to `jj bookmark list --remote <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_remote_bookmarks(&self, remote: &str) -> Result<String, CommandError> {
        // Rendered, never parsed -- see [Command::run_lossy].
        Ok(self
            .jj([
                "bookmark",
                "list",
                "--remote",
                // Match the name literally, never as a glob.
                &format!("exact:{}", quote_pattern(remote)),
                "--sort",
                "committer-date-",
            ])
            .color()
            .run_lossy()?
            .remove_end_line())
    }

    /// Add a remote. Maps to `jj git remote add <name> <url>`
    #[instrument(level = "trace", skip(self))]
    pub fn add_remote(&self, name: &str, url: &str) -> Result<(), CommandError> {
        self.jj(["git", "remote", "add", name, url]).run_void()
    }

    /// Remove a remote and forget its bookmarks.
    /// Maps to `jj git remote remove <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn remove_remote(&self, name: &str) -> Result<(), CommandError> {
        self.jj(["git", "remote", "remove", name]).run_void()
    }

    /// Rename a remote. Maps to `jj git remote rename <old> <new>`
    #[instrument(level = "trace", skip(self))]
    pub fn rename_remote(&self, old: &str, new: &str) -> Result<(), CommandError> {
        self.jj(["git", "remote", "rename", old, new]).run_void()
    }

    /// Change a remote's URL. Maps to `jj git remote set-url <name> <url>`
    #[instrument(level = "trace", skip(self))]
    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<(), CommandError> {
        self.jj(["git", "remote", "set-url", name, url]).run_void()
    }

    /// Fetch from one remote. Maps to `jj git fetch --remote <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn git_fetch_remote(&self, name: &str) -> Result<String, CommandError> {
        self.jj(["git", "fetch", "--remote", &exact_remote(name)])
            .color()
            .run()
    }

    /// Push every tracked bookmark of a remote.
    /// Maps to `jj git push --remote <name> --tracked`
    ///
    /// With `dry_run`, reports what would be pushed without pushing. The
    /// dry run needs no network access: jj compares against the state of the
    /// remote as of the last fetch.
    #[instrument(level = "trace", skip(self, identity))]
    pub fn git_push_remote(
        &self,
        name: &str,
        dry_run: bool,
        identity: &PushIdentity,
    ) -> Result<String, CommandError> {
        let mut args = vec!["git", "push", "--remote", name, "--tracked"];
        if dry_run {
            args.push("--dry-run");
        }
        // jj reports what a push did (or would do) on stderr, so keep both.
        let (stdout, stderr) = self
            .jj(args)
            .running_as(identity)
            .color()
            .verbose()
            .run_with_stderr()?;
        Ok(format!("{stdout}{stderr}").remove_end_line())
    }
}

/// The bookmarks a push dry run reports it would change, from jj's (uncolored)
/// `  bookmark: <name> [move forward from ...]` lines.
pub fn bookmarks_in_push_preview(preview: &str) -> Vec<String> {
    preview
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("bookmark: "))
        .filter_map(|rest| rest.rsplit_once(" [").map(|(name, _)| name.to_owned()))
        .collect()
}

/// `--remote` takes a string pattern, glob by default; `exact:` makes it a
/// literal match. Only `jj git fetch` and `jj bookmark list` accept patterns;
/// `jj git push --remote` takes a plain name.
fn exact_remote(name: &str) -> String {
    format!("exact:{}", quote_pattern(name))
}

/// Quote a pattern body if it contains characters the pattern parser treats
/// specially. jj string patterns accept a double-quoted string after the kind
/// prefix.
fn quote_pattern(name: &str) -> String {
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn reads_bookmarks_from_push_preview() {
        let preview = "Changes to push to origin:
  bookmark: main [move forward from c1d99e305ab3 to ffdbe6a1faa1]
  bookmark: new-one [add to ffdbe6a1faa1]
Dry-run requested, not pushing.";
        assert_eq!(bookmarks_in_push_preview(preview), ["main", "new-one"]);
        assert!(bookmarks_in_push_preview("Nothing changed.").is_empty());
    }

    #[test]
    fn parses_remote_list() {
        let remotes =
            parse_remote_list("origin https://example.com/a.git\nfork git@host:x/y.git\n");
        assert_eq!(
            remotes,
            vec![
                ("origin".to_owned(), "https://example.com/a.git".to_owned()),
                ("fork".to_owned(), "git@host:x/y.git".to_owned()),
            ]
        );
    }

    #[test]
    fn parses_tracking_counts() {
        let exact = parse_remote_bookmark("main\torigin\ttrue\t0,0,0,0").unwrap();
        assert_eq!(exact.remote, "origin");
        assert!(exact.tracked);
        assert_eq!(exact.tracking, Some((Count::exactly(0), Count::exactly(0))));

        // An unknown upper bound leaves the field empty. The remote being
        // behind by 10 means the local bookmark is 10 commits ahead.
        let inexact = parse_remote_bookmark("main\tupstream\ttrue\t0,0,10,").unwrap();
        assert_eq!(
            inexact.tracking,
            Some((
                Count {
                    lower: 10,
                    exact: false
                },
                Count::exactly(0),
            ))
        );

        let untracked = parse_remote_bookmark("dev\tupstream\tfalse\t-").unwrap();
        assert!(!untracked.tracked);
        assert_eq!(untracked.tracking, None);
    }

    #[test]
    fn aggregates_per_remote() {
        let stats = aggregate(
            "main\torigin\ttrue\t0,0,0,0\n\
             dev\torigin\ttrue\t2,2,1,1\n\
             old\torigin\tfalse\t-\n\
             main\tupstream\ttrue\t0,0,10,\n",
        );
        let origin = &stats["origin"];
        assert_eq!(origin.bookmarks, 3);
        assert_eq!(origin.tracked, 2);
        assert_eq!(origin.out_of_sync, 1);
        assert_eq!(origin.ahead, Count::exactly(1));
        assert_eq!(origin.behind, Count::exactly(2));

        let upstream = &stats["upstream"];
        assert_eq!(upstream.out_of_sync, 1);
        assert_eq!(upstream.ahead.to_string(), "10+");
        assert_eq!(upstream.behind.to_string(), "0");
    }

    #[test]
    fn quotes_awkward_remote_names() {
        assert_eq!(quote_pattern("origin"), "origin");
        assert_eq!(quote_pattern("my+fork"), "\"my+fork\"");
        assert_eq!(quote_pattern("we\"ird"), "\"we\\\"ird\"");
    }

    #[test]
    fn manages_remotes() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let commander = &test_repo.commander;
        assert!(commander.get_remotes()?.is_empty());

        commander.add_remote("origin", "https://example.com/a.git")?;
        commander.add_remote("my+fork", "https://example.com/b.git")?;
        let remotes = commander.get_remotes()?;
        assert_eq!(remotes.len(), 2);
        let origin = remotes.iter().find(|r| r.name == "origin").unwrap();
        assert_eq!(origin.url, "https://example.com/a.git");
        assert_eq!(origin.stats.bookmarks, 0);
        assert!(remotes.iter().any(|r| r.name == "my+fork"));

        commander.set_remote_url("origin", "https://example.com/c.git")?;
        commander.rename_remote("origin", "upstream")?;
        let remotes = commander.get_remotes()?;
        assert!(
            remotes
                .iter()
                .any(|r| r.name == "upstream" && r.url == "https://example.com/c.git")
        );

        // The pattern quoting must reach jj intact for a glob-like name.
        commander.get_remote_bookmarks("my+fork")?;

        commander.remove_remote("upstream")?;
        assert_eq!(commander.get_remotes()?.len(), 1);
        Ok(())
    }
}
