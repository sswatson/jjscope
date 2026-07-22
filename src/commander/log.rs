/*!
[Commander] member functions related to jj log.

This module has features to parse the log output to extract change id and commit id.
It is mostly used in the [log_tab][crate::ui::log_tab] module.
*/

use std::fmt::Display;
use std::sync::LazyLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use itertools::Itertools;
use regex::Regex;
use thiserror::Error;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::RemoveEndLine;
use crate::commander::bookmarks::Bookmark;
use crate::commander::ids::ChangeId;
use crate::commander::ids::CommitId;
use crate::env::DiffFormat;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Head {
    pub change_id: ChangeId,
    pub commit_id: CommitId,
    pub divergent: bool,
    pub immutable: bool,
}

#[derive(Clone, Debug)]
pub struct LogOutput {
    pub graph: String,
    // Maps graph line -> heads
    pub graph_heads: Vec<Option<Head>>,
    pub heads: Vec<Head>,
}

impl LogOutput {
    pub fn head_at(&self, line: usize) -> Option<&Head> {
        self.graph_heads.get(line).and_then(Option::as_ref)
    }
}

#[derive(Error, Debug)]
pub struct HeadParseError(String);

impl Display for HeadParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Head parse error: {}", self.0)
    }
}

// Template which outputs `[change_id|commit_id|divergent]`. Used to parse data from log and other
// commands which supports templating.
const HEAD_TEMPLATE: &str =
    r#""[" ++ change_id ++ "|" ++ commit_id ++ "|" ++ divergent ++ "|" ++ immutable ++ "]""#;
const HEAD_TEMPLATE_NL: &str = r#""[" ++ change_id ++ "|" ++ commit_id ++ "|" ++ divergent ++ "|" ++ immutable ++ "]" ++ "\n""#;
// Regex to parse HEAD_TEMPLATE
static HEAD_TEMPLATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(.*)\|(.*)\|(.*)\|(.*)\]").unwrap());

// Parse a head with HEAD_TEMPLATE.
fn parse_head(text: &str) -> Result<Head> {
    let captured = HEAD_TEMPLATE_REGEX.captures(text);
    captured
        .as_ref()
        .map_or(Err(anyhow!(HeadParseError(text.to_owned()))), |captured| {
            if let (Some(change_id), Some(commit_id), Some(divergent), Some(immutable)) = (
                captured.get(1),
                captured.get(2),
                captured.get(3),
                captured.get(4),
            ) {
                Ok(Head {
                    change_id: ChangeId(change_id.as_str().to_string()),
                    commit_id: CommitId(commit_id.as_str().to_string()),
                    divergent: divergent.as_str() == "true",
                    immutable: immutable.as_str() == "true",
                })
            } else {
                bail!(HeadParseError(text.to_owned()))
            }
        })
}

/// Build a `templates.log_node=...` config override string that replaces the
/// graph node glyph with `glyph` for each of `ids`, falling back to jj's
/// usual node otherwise. `ids` are change or commit IDs in hex (both work
/// unquoted as revset symbols, so callers can mix either). Returns `None` if
/// there's nothing to override, so callers can skip passing `--config`.
///
/// Earlier entries in `overrides` take priority when a commit matches more
/// than one, since the `if` chain short-circuits on the first match.
fn build_log_node_config(overrides: &[(char, &[&str])]) -> Option<String> {
    let fallback = "builtin_log_node".to_owned();
    let template = overrides
        .iter()
        .filter(|(_, ids)| !ids.is_empty())
        .rev()
        .fold(fallback, |rest, (glyph, ids)| {
            // Bare hex change/commit IDs are valid revset symbols on their
            // own, so this avoids nesting string literals (e.g. via
            // `commit_id("...")`) inside the outer `contained_in("...")`
            // string, which would need careful escaping.
            let revset = ids.iter().join("|");
            format!(r#"if(self.contained_in("{revset}"), "{glyph}", {rest})"#)
        });

    if template == "builtin_log_node" {
        return None;
    }

    // Elided-revision placeholders (shown as "(elided revisions)") aren't
    // real commits, so `self` is falsy for them and `self.contained_in(...)`
    // errors out instead of just returning false. Guard the same way
    // `builtin_log_node` itself does, or the elided node renders as
    // "<Error: No Commit available>" instead of "~".
    Some(format!(
        "templates.log_node=if(!self, builtin_log_node, {template})"
    ))
}

impl Commander {
    fn execute_jj_log(&self, revset: &str, template: &str) -> Result<String, CommandError> {
        self.jj(["log", "--no-graph", "--template", template, "-r", revset])
            .run()
    }

    fn execute_jj_log_one(&self, revset: &str, template: &str) -> Result<String, CommandError> {
        self.jj([
            "log",
            "--no-graph",
            "--template",
            template,
            "-r",
            revset,
            "--limit",
            "1",
        ])
        .run()
    }

    /// Get log. Returns human readable log and mapping to log line to head.
    /// Maps to `jj log`
    ///
    /// `node_overrides` replaces the graph node glyph (`@`/`○`/`◆`/...) for
    /// specific commits, e.g. to show marked or just-absorbed-into commits
    /// with a distinct symbol instead of jj's usual node. Each entry is a
    /// glyph and the change/commit IDs (in hex) that should show it; earlier
    /// entries take priority if a commit matches more than one.
    #[instrument(level = "trace", skip(self, node_overrides))]
    pub fn get_log(
        &self,
        revset: &Option<String>,
        node_overrides: &[(char, &[&str])],
    ) -> Result<LogOutput, CommandError> {
        let mut args = vec![];

        if let Some(revset) = revset {
            args.push("-r");
            args.push(revset);
        }

        let log_node_config = build_log_node_config(node_overrides);
        let mut config_args = vec![];
        if let Some(log_node_config) = &log_node_config {
            config_args.push("--config");
            config_args.push(log_node_config.as_str());
        }

        // Force builtin_log_compact which uses 2 lines per change
        let graph = self
            .jj([
                vec!["log", "--template", "builtin_log_compact"],
                config_args.clone(),
                args.clone(),
            ]
            .concat())
            .color()
            .run()?;

        // Extract the log one more time, but this time use a template
        // where each line begins with Head information. Since jj has
        // 2 lines per change, there will also be two lines with head info.
        // The number of lines in graph and the number of items in graph_heads
        // should be identical.
        let graph_heads: Vec<Option<Head>> = self
            .jj([
                vec![
                    "log",
                    "--template",
                    // Match builtin_log_compact with 2 lines per change
                    &format!(r#"{HEAD_TEMPLATE} ++ " " ++ bookmarks ++"\n" ++ {HEAD_TEMPLATE}"#),
                ],
                args,
            ]
            .concat())
            .run()?
            .lines()
            .map(|line| parse_head(line).ok())
            .collect();

        let heads = graph_heads.clone().into_iter().flatten().unique().collect();

        Ok(LogOutput {
            graph,
            graph_heads,
            heads,
        })
    }

    /// Get commit details.
    /// Maps to `jj show <commit>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_commit_show(
        &self,
        commit_id: &CommitId,
        diff_format: &DiffFormat,
        ignore_working_copy: bool,
    ) -> Result<String, CommandError> {
        let mut args = vec!["show", commit_id.as_str()];
        args.append(&mut diff_format.get_args());
        if ignore_working_copy {
            args.push("--ignore-working-copy");
        }

        Ok(self.jj(args).color().run()?.remove_end_line())
    }

    /// Get the current head.
    /// Maps to `jj log -r @`
    #[instrument(level = "trace", skip(self))]
    pub fn get_current_head(&self) -> Result<Head> {
        parse_head(
            &self
                .execute_jj_log_one("@", HEAD_TEMPLATE_NL)
                .context("Failed getting current head")?
                .remove_end_line(),
        )
    }

    /// Get the latest version of a head. Can detect evolution of divergent head.
    #[instrument(level = "trace", skip(self))]
    pub fn get_head_latest(&self, head: &Head) -> Result<Head> {
        // Get all heads which point to the same change ID
        let latest_heads_res = self.execute_jj_log(
            &format!(r#"change_id({})"#, head.change_id.as_str()),
            HEAD_TEMPLATE_NL,
        );
        let Ok(latest_heads_res) = latest_heads_res else {
            return self.get_head_latest(&self.get_current_head()?);
        };
        if latest_heads_res.is_empty() {
            return self.get_head_latest(&self.get_current_head()?);
        }
        let latest_heads: Vec<Head> = latest_heads_res
            .lines()
            .map(parse_head)
            .collect::<Result<Vec<Head>>>()?;

        // If the current head exist, that means it wasn't updated
        if let Some(head) = latest_heads.iter().find(|latest_head| latest_head == &head) {
            return Ok(head.to_owned());
        }

        // Check obslog for each head. If the obslog contains the head's commit, it means
        // there's a new commit for the head
        for latest_head in latest_heads.iter() {
            let parent_commits: Vec<ChangeId> = self
                .jj([
                    "obslog",
                    "--no-graph",
                    "--template",
                    r#"commit.change_id() ++ "\n""#,
                    "-r",
                    latest_head.commit_id.as_str(),
                ])
                .run()
                .context("Failed getting latest head parent commits")?
                .lines()
                .map(|line| ChangeId(line.to_owned()))
                .collect();

            if parent_commits
                .iter()
                .any(|parent_commit| parent_commit == &head.change_id)
            {
                return Ok(latest_head.to_owned());
            }
        }

        bail!(
            "Could not find head latest: {} {} {:?}",
            head.change_id,
            head.commit_id,
            latest_heads
        );
    }

    /// Get the head for a revision.
    /// Maps to `jj log -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_head(&self, revision: &str) -> Result<Head> {
        parse_head(
            &self
                .execute_jj_log_one(revision, HEAD_TEMPLATE_NL)
                .with_context(|| format!("Failed getting head: {revision}"))?
                .remove_end_line(),
        )
    }

    /// Get all of a commit's parents, in parent order.
    /// Maps to `jj log -r <revision> -T 'parents.map(|p| p.commit_id())...'`
    ///
    /// The template goes through the commit's own parent list rather than the
    /// `<revision>-` revset, whose output order is topological, not parent
    /// order.
    pub(crate) fn get_commit_parents(&self, commit_id: &CommitId) -> Result<Vec<CommitId>> {
        Ok(self
            .execute_jj_log_one(
                commit_id.as_str(),
                r#"parents.map(|p| p.commit_id()).join("\n") ++ "\n""#,
            )
            .with_context(|| format!("Failed getting commit parents: {commit_id}"))?
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| CommitId(line.to_owned()))
            .collect())
    }

    /// Get a commit's parent.
    /// Maps to `jj log -r <revision>-`
    #[instrument(level = "trace", skip(self))]
    pub fn get_commit_parent(&self, commit_id: &CommitId) -> Result<Head> {
        parse_head(
            &self
                .execute_jj_log_one(&format!("{commit_id}-"), HEAD_TEMPLATE_NL)
                .with_context(|| format!("Failed getting commit parent: {commit_id}"))?
                .remove_end_line(),
        )
    }

    /// Get commit's description.
    /// Maps to `jj log -r <revision> -T description`
    #[instrument(level = "trace", skip(self))]
    pub fn get_commit_description(&self, commit_id: &CommitId) -> Result<String> {
        Ok(self
            .execute_jj_log_one(commit_id.as_str(), "description")
            .with_context(|| format!("Failed getting commit description: {commit_id}"))?
            .remove_end_line())
    }

    /// Check if a revision is immutable
    /// Maps to `jj log -r <revision> -T immutable`
    #[instrument(level = "trace", skip(self))]
    pub fn check_revision_immutable(&self, revision: &str) -> Result<bool> {
        Ok(self
            .execute_jj_log_one(revision, "immutable")
            .with_context(|| format!("Failed checking if revision is immutable: {revision}"))?
            .remove_end_line()
            == "true")
    }

    /// Get bookmark head
    /// Maps to `jj log -r <bookmark>[@<remote>]`
    #[instrument(level = "trace", skip(self))]
    pub fn get_bookmark_head(&self, bookmark: &Bookmark) -> Result<Head> {
        parse_head(
            &self
                .execute_jj_log_one(&bookmark.to_string(), HEAD_TEMPLATE_NL)
                .context("Failed getting bookmark head")?
                .remove_end_line(),
        )
    }

    /// Get the head(s) among the given revisions: those that are not an
    /// ancestor of any other. Maps to `jj log -r 'heads(<rev>|<rev>|...)'`
    ///
    /// A single result for a pair of revisions means the pair is comparable
    /// (one is an ancestor of the other), and the result is the descendant.
    pub(crate) fn get_heads_among(&self, commit_ids: &[CommitId]) -> Result<Vec<CommitId>> {
        let revset = commit_ids.iter().map(CommitId::as_str).join("|");
        Ok(self
            .execute_jj_log(&format!("heads({revset})"), "commit_id ++ \"\\n\"")
            .context("Failed getting heads among revisions")?
            .lines()
            .map(|line| CommitId(line.to_owned()))
            .collect())
    }

    /// Get all mutable revisions. Maps to `jj log -r 'mutable()'`
    ///
    /// Used to snapshot the candidate set before an operation like `jj absorb`
    /// that rewrites revisions, so the rewritten set can be found afterwards
    /// by comparing commit IDs for each change ID (see
    /// [Self::run_absorb][crate::commander::Commander::run_absorb]). The set
    /// covers the whole mutable graph rather than just ancestors, since
    /// rebase propagation also rewrites descendants on sibling branches.
    pub(crate) fn get_mutable_heads(&self) -> Result<Vec<Head>> {
        self.execute_jj_log("mutable()", HEAD_TEMPLATE_NL)
            .context("Failed getting mutable revisions")?
            .lines()
            .map(parse_head)
            .collect()
    }

    /// Get the current head of a change, if it still exists.
    /// Maps to `jj log -r 'change_id(<id>)'`
    #[cfg_attr(not(test), expect(dead_code, reason = "currently only used by tests"))]
    pub(crate) fn get_change_head(&self, change_id: &ChangeId) -> Result<Option<Head>> {
        let result = self.execute_jj_log(&format!("change_id({change_id})"), HEAD_TEMPLATE_NL)?;
        result.lines().map(parse_head).next().transpose()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use insta::assert_debug_snapshot;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn get_log() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let log = test_repo.commander.get_log(&None, &[])?;

        let mut settings = insta::Settings::clone_current();
        settings.add_filter(r"[k-z]{8} .*? [0-9a-fA-F]{8}", "[LINE]");
        let _bound = settings.bind_to_scope();

        assert_debug_snapshot!(log.graph);

        assert!(log.graph_heads.iter().all(|graph_head| {
            graph_head
                .as_ref()
                .is_none_or(|graph_head| log.heads.contains(graph_head))
        }));

        Ok(())
    }

    #[test]
    fn get_log_node_override() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        let commit_id = head.commit_id.as_str();

        let log = test_repo.commander.get_log(&None, &[('X', &[commit_id])])?;

        assert!(log.graph.lines().next().unwrap().starts_with('X'));

        Ok(())
    }

    #[test]
    fn get_log_node_override_with_elided_revisions() -> Result<()> {
        // A node override must not break rendering of the synthetic
        // "(elided revisions)" placeholder node, which isn't a real commit
        // (regression test: this used to render as
        // "<Error: No Commit available>" instead of "~").
        let test_repo = TestRepo::new()?;

        let root_head = test_repo.commander.get_current_head()?;
        fs::write(test_repo.directory.path().join("f.txt"), b"A")?;
        test_repo
            .commander
            .run_new([root_head.commit_id.as_str()])?;
        let middle_head = test_repo.commander.get_current_head()?;
        fs::write(test_repo.directory.path().join("f.txt"), b"B")?;
        test_repo
            .commander
            .run_new([middle_head.commit_id.as_str()])?;
        let head = test_repo.commander.get_current_head()?;

        // A revset naming only the root and current head, skipping
        // middle_head in between, elides that middle commit.
        let revset = format!(
            "{}|{}",
            root_head.commit_id.as_str(),
            head.commit_id.as_str()
        );
        let log = test_repo
            .commander
            .get_log(&Some(revset), &[('X', &[head.commit_id.as_str()])])?;

        assert!(log.graph.contains("(elided revisions)"));
        assert!(!log.graph.contains("Error"));

        Ok(())
    }

    #[test]
    fn get_commit_show() -> Result<()> {
        let test_repo = TestRepo::new()?;

        fs::write(test_repo.directory.path().join("README"), b"AAA")?;

        let head = test_repo.commander.get_current_head()?;
        let show =
            test_repo
                .commander
                .get_commit_show(&head.commit_id, &DiffFormat::ColorWords, false)?;

        let mut settings = insta::Settings::clone_current();
        settings.add_filter(r"Commit ID: [0-9a-fA-F]{40}", "Commit ID: [COMMIT_ID]");
        settings.add_filter(r"Change ID: [k-z]{32}", "Change ID: [Change ID]");
        settings.add_filter(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})", "([DATE_TIME])");
        let _bound = settings.bind_to_scope();

        assert_debug_snapshot!(show);

        Ok(())
    }

    #[test]
    fn get_commit_parent() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;

        assert_eq!(
            test_repo.commander.get_commit_parent(&head.commit_id)?,
            Head {
                commit_id: CommitId("0000000000000000000000000000000000000000".to_owned()),
                change_id: ChangeId("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".to_owned()),
                divergent: false,
                immutable: true,
            }
        );

        Ok(())
    }

    #[test]
    fn get_head_latest() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let old_head = test_repo.commander.get_current_head()?;

        fs::write(test_repo.directory.path().join("README"), b"AAA")?;

        let new_head = test_repo.commander.get_current_head()?;

        assert_ne!(old_head, new_head);

        assert_eq!(new_head, test_repo.commander.get_head_latest(&old_head)?);

        Ok(())
    }

    #[test]
    fn get_commit_parents() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let left = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let right = test_repo.commander.get_current_head()?;

        // Merge with (left, right) as parents, in that order
        test_repo
            .commander
            .run_new([left.commit_id.as_str(), right.commit_id.as_str()])?;
        let merge = test_repo.commander.get_current_head()?;

        let parents = test_repo.commander.get_commit_parents(&merge.commit_id)?;
        assert_eq!(parents, [left.commit_id, right.commit_id]);

        Ok(())
    }

    #[test]
    fn get_heads_among() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let child = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let sibling = test_repo.commander.get_current_head()?;

        // Comparable pair: one head, the descendant
        let heads = test_repo
            .commander
            .get_heads_among(&[base.commit_id.clone(), child.commit_id.clone()])?;
        assert_eq!(heads, [child.commit_id.clone()]);

        // Incomparable pair: both are heads
        let mut heads = test_repo
            .commander
            .get_heads_among(&[child.commit_id.clone(), sibling.commit_id.clone()])?;
        heads.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut expected = [child.commit_id.clone(), sibling.commit_id.clone()];
        expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(heads, expected);

        Ok(())
    }

    #[test]
    fn check_revision_immutable() -> Result<()> {
        let test_repo = TestRepo::new()?;

        assert!(!(test_repo.commander.check_revision_immutable("@")?));

        Ok(())
    }

    #[test]
    fn get_bookmark_head() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        // Git doesn't support bookmark pointing to root commit, so it will advance
        let bookmark = test_repo.commander.create_bookmark("main")?;

        assert_eq!(test_repo.commander.get_bookmark_head(&bookmark)?, head);

        Ok(())
    }
}
