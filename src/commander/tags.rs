/*!
[Commander] member functions related to jj tag.

Tags gained first-class support in jj 0.44: they can be set, deleted, and
tracked against remotes much like bookmarks. The templating surface is the same
as for bookmarks (`name`, `remote`, `present`, `tracked`, `normal_target()`), so
the parsing here mirrors [bookmarks][super::bookmarks].

It is mostly used in the [tags_tab][crate::ui::tags_tab] module.
*/
use std::fmt::Display;
use std::sync::LazyLock;

use ansi_to_tui::IntoText;
use itertools::Itertools;
use ratatui::text::Text;
use regex::Regex;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::RemoveEndLine;
use crate::env::DiffFormat;

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub remote: Option<String>,
    pub present: bool,
    /// Whether a remote tag is tracked by a local tag of the same name. Always
    /// false for a local tag, which has nothing to track.
    pub tracked: bool,
    pub timestamp: i64,
}

impl Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut text = self.name.clone();
        if let Some(remote) = self.remote.as_ref() {
            text.push('@');
            text.push_str(remote);
        }
        write!(f, "{text}")
    }
}

impl Tag {
    /// The argument that names this tag to `jj tag track`/`untrack`, which
    /// take `name@remote` rather than a bare name.
    pub fn remote_ref(&self) -> Option<String> {
        let remote = self.remote.as_ref()?;
        Some(format!("{}@{}", self.name, remote))
    }
}

// Template which outputs `[name@remote|present|tracked|timestamp]`.
const TAG_TEMPLATE: &str = r#""[" ++ name ++ "@" ++ remote ++ "|" ++ present ++ "|" ++ tracked ++ "|" ++ self.normal_target().committer().timestamp().format("%s") ++ "]""#;
static TAG_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(.*)@(.*)\|(true|false)\|(true|false)\|(\d+)\]$").unwrap());

fn parse_tag(text: &str) -> Option<Tag> {
    let captured = TAG_REGEX.captures(text)?;
    let name = captured.get(1)?.as_str().to_owned();
    let remote = captured.get(2)?.as_str().to_owned();
    let present = captured.get(3)?.as_str() == "true";
    let tracked = captured.get(4)?.as_str() == "true";
    let timestamp = captured.get(5)?.as_str().parse::<i64>().unwrap_or(0);

    Some(Tag {
        name,
        remote: if remote.is_empty() {
            None
        } else {
            Some(remote)
        },
        present,
        tracked,
        timestamp,
    })
}

#[derive(Clone, Debug)]
pub enum TagLine {
    Unparsable(String),
    Parsed { text: String, tag: Tag },
}

impl TagLine {
    pub fn to_text(&self) -> Result<Text<'_>, ansi_to_tui::Error> {
        match self {
            TagLine::Unparsable(text) => text.to_text(),
            TagLine::Parsed { text, .. } => text.to_text(),
        }
    }
}

impl Commander {
    /// Get tags, as colored display lines paired with their parsed data.
    /// Maps to `jj tag list`
    #[instrument(level = "trace", skip(self))]
    pub fn get_tags(&self, show_all: bool) -> Result<Vec<TagLine>, CommandError> {
        let mut args = vec![];
        if show_all {
            args.push("--all-remotes");
        }

        let tags_colored = self
            .jj([vec!["tag", "list"], args.clone()].concat())
            .color()
            .run()?;

        let tags: Vec<TagLine> = self
            .jj([
                vec!["tag", "list", "-T", &format!(r#"{TAG_TEMPLATE} ++ "\n""#)],
                args,
            ]
            .concat())
            .run()?
            .lines()
            .zip(tags_colored.lines())
            .map(|(line, line_colored)| match parse_tag(line) {
                Some(tag) => TagLine::Parsed {
                    text: line_colored.to_owned(),
                    tag,
                },
                None => TagLine::Unparsable(line_colored.to_owned()),
            })
            .collect();

        Ok(tags)
    }

    /// Get tags as data only, newest target first.
    #[instrument(level = "trace", skip(self))]
    pub fn get_tags_list(&self, show_all: bool) -> Result<Vec<Tag>, CommandError> {
        let mut args = vec![
            "tag".to_owned(),
            "list".to_owned(),
            "-T".to_owned(),
            format!(r#"if(present, {TAG_TEMPLATE} ++ "\n", "")"#),
        ];
        if show_all {
            args.push("--all-remotes".to_owned());
        }

        Ok(self
            .jj(args)
            .run()?
            .lines()
            .filter_map(parse_tag)
            .sorted_by(|a, b| b.timestamp.cmp(&a.timestamp))
            .collect())
    }

    /// Get tags pointing at a revision.
    /// Maps to `jj tag list -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_tags_at(&self, revision: &str) -> Result<Vec<Tag>, CommandError> {
        Ok(self
            .jj([
                "tag",
                "list",
                "-r",
                revision,
                "-T",
                &format!(r#"if(present, {TAG_TEMPLATE} ++ "\n", "")"#),
            ])
            .run()?
            .lines()
            .filter_map(parse_tag)
            .collect())
    }

    /// Show the revision a tag points at.
    /// Maps to `jj show <tag>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_tag_show(
        &self,
        tag: &Tag,
        diff_format: &DiffFormat,
        ignore_working_copy: bool,
    ) -> Result<String, CommandError> {
        let tag_arg = &tag.to_string();
        let mut args = vec!["show", tag_arg];
        args.append(&mut diff_format.get_args());
        if ignore_working_copy {
            args.push("--ignore-working-copy");
        }

        Ok(self.jj(args).color().run()?.remove_end_line())
    }

    /// Create or move a tag onto a revision.
    /// Maps to `jj tag set <name> -r <revision> [--allow-move]`
    ///
    /// `allow_move` is required by jj to repoint a tag that already exists;
    /// without it jj refuses rather than silently moving a release marker.
    #[instrument(level = "trace", skip(self))]
    pub fn set_tag(
        &self,
        name: &str,
        revision: &str,
        allow_move: bool,
    ) -> Result<(), CommandError> {
        let mut args = vec!["tag", "set", name, "-r", revision];
        if allow_move {
            args.push("--allow-move");
        }
        self.jj(args).run_void()
    }

    /// Delete a tag. The tagged revision itself is left alone.
    /// Maps to `jj tag delete <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn delete_tag(&self, name: &str) -> Result<(), CommandError> {
        self.jj(["tag", "delete", name]).run_void()
    }

    /// Start tracking a remote tag with a local tag of the same name.
    /// Maps to `jj tag track <name>@<remote>`
    #[instrument(level = "trace", skip(self))]
    pub fn track_tag(&self, remote_ref: &str) -> Result<String, CommandError> {
        self.jj(["tag", "track", remote_ref]).color().run()
    }

    /// Stop tracking a remote tag.
    /// Maps to `jj tag untrack <name>@<remote>`
    #[instrument(level = "trace", skip(self))]
    pub fn untrack_tag(&self, remote_ref: &str) -> Result<String, CommandError> {
        self.jj(["tag", "untrack", remote_ref]).color().run()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn parse_tag_local_and_remote() {
        let local = parse_tag("[v1.0@|true|false|1786376005]").unwrap();
        assert_eq!(local.name, "v1.0");
        assert_eq!(local.remote, None);
        assert!(local.present);
        assert!(!local.tracked);
        assert_eq!(local.timestamp, 1786376005);
        assert_eq!(local.to_string(), "v1.0");
        assert_eq!(local.remote_ref(), None);

        let remote = parse_tag("[v2.0@origin|true|true|1786376006]").unwrap();
        assert_eq!(remote.name, "v2.0");
        assert_eq!(remote.remote.as_deref(), Some("origin"));
        assert!(remote.tracked);
        assert_eq!(remote.to_string(), "v2.0@origin");
        assert_eq!(remote.remote_ref().as_deref(), Some("v2.0@origin"));
    }

    #[test]
    fn parse_tag_rejects_other_lines() {
        // The colored/human listing must not be mistaken for template output.
        assert!(parse_tag("v1.0: knyyswnu 4e976878 first").is_none());
        assert!(parse_tag("").is_none());
        assert!(parse_tag("[malformed]").is_none());
    }

    #[test]
    fn set_list_and_delete_tag() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let head = test_repo.commander.get_current_head()?;

        test_repo
            .commander
            .set_tag("v1.0", head.commit_id.as_str(), false)?;

        let tags = test_repo.commander.get_tags_list(false)?;
        assert_eq!(tags.len(), 1, "got {tags:?}");
        assert_eq!(tags[0].name, "v1.0");
        assert_eq!(tags[0].remote, None);

        // Tags at a revision.
        let at = test_repo.commander.get_tags_at(head.commit_id.as_str())?;
        assert_eq!(at.len(), 1);
        assert_eq!(at[0].name, "v1.0");

        test_repo.commander.delete_tag("v1.0")?;
        assert!(test_repo.commander.get_tags_list(false)?.is_empty());

        Ok(())
    }

    #[test]
    fn set_tag_requires_allow_move_to_repoint() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let first = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .set_tag("v1.0", first.commit_id.as_str(), false)?;

        test_repo.commander.jj(["new"]).run_void()?;
        let second = test_repo.commander.get_current_head()?;

        // jj refuses to move an existing tag without --allow-move, so a
        // careless keystroke cannot silently relocate a release marker.
        assert!(
            test_repo
                .commander
                .set_tag("v1.0", second.commit_id.as_str(), false)
                .is_err(),
            "expected jj to refuse moving an existing tag"
        );

        // With it, the tag moves.
        test_repo
            .commander
            .set_tag("v1.0", second.commit_id.as_str(), true)?;
        let at = test_repo.commander.get_tags_at(second.commit_id.as_str())?;
        assert_eq!(at.len(), 1, "got {at:?}");
        assert_eq!(at[0].name, "v1.0");

        Ok(())
    }

    #[test]
    fn get_tags_pairs_display_lines_with_data() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let head = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .set_tag("v1.0", head.commit_id.as_str(), false)?;

        let lines = test_repo.commander.get_tags(false)?;
        assert_eq!(lines.len(), 1, "got {lines:?}");
        match &lines[0] {
            TagLine::Parsed { text, tag } => {
                assert_eq!(tag.name, "v1.0");
                // The display line is the human listing, not the template.
                assert!(text.contains("v1.0"), "unexpected display line: {text:?}");
                assert!(!text.starts_with('['), "template leaked into display");
            }
            other => panic!("expected a parsed tag line, got {other:?}"),
        }

        Ok(())
    }
}
