/*!
[Commander] member functions related to jj diff.

This module has features to parse the diff output.
It is mostly used in the [files_tab][crate::ui::files_tab] module.
*/
use std::sync::LazyLock;

use anyhow::Context;
use anyhow::Result;
use ratatui::style::Color;
use regex::Regex;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::ids::CommitId;
use crate::commander::log::Head;
use crate::env::DiffFormat;

#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub line: String,
    pub path: Option<String>,
    pub diff_type: Option<DiffType>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub path: String,
}

/// Which side of a conflict [Commander::run_resolve] keeps.
///
/// jj's built-in `:ours`/`:theirs` merge tools keep side #1 or side #2 of a
/// conflict. jj orders the sides by the roles in the operation that
/// introduced the conflict (rebase, squash, or the automatic rebase of
/// descendants when an ancestor is rewritten): side #1 is the operation's
/// destination and side #2 is the revision that was moved. These match the
/// labels jj prints in conflict markers, e.g. "rebase destination" vs
/// "rebased revision", or "squash destination" vs "squashed revision".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictSide {
    /// Keep the moved revision's content, i.e. the rebased or squashed
    /// revision (side #2, `:theirs`).
    Source,
    /// Keep the destination's content, e.g. the rebase or squash destination
    /// (side #1, `:ours`).
    Destination,
}

impl DiffType {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "A" => Some(DiffType::Added),
            "M" => Some(DiffType::Modified),
            "D" => Some(DiffType::Deleted),
            "R" => Some(DiffType::Renamed),
            _ => None,
        }
    }

    pub fn color(&self) -> Color {
        match self {
            DiffType::Added => Color::Green,
            DiffType::Modified => Color::Cyan,
            DiffType::Renamed => Color::Cyan,
            DiffType::Deleted => Color::Red,
        }
    }
}

// Example line: `A README.md`, `M src/main.rs`, `D Hello World`
static FILES_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.) (.*)").unwrap());
static RENAME_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{(.*?) => (.*?)\}").unwrap());
static CONFLICTS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.*)    .*").unwrap());

impl Commander {
    /// Get list of changes files in a change. Parses the output.
    /// Maps to `jj diff --summary -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_files(&self, head: &Head) -> Result<Vec<File>, CommandError> {
        Ok(self
            .jj(["diff", "-r", head.commit_id.as_str(), "--summary"])
            .run()?
            .lines()
            .map(|line| {
                let captured = FILES_REGEX.captures(line);
                let diff_type = captured
                    .as_ref()
                    .and_then(|captured| captured.get(1))
                    .and_then(|inner_text| DiffType::parse(inner_text.as_str()));
                let path = captured
                    .as_ref()
                    .and_then(|captured| captured.get(2))
                    .map(|inner_text| inner_text.as_str().to_owned());

                File {
                    line: line.to_string(),
                    path,
                    diff_type,
                }
            })
            .collect())
    }

    /// Get list of changes files in a change. Parses the output.
    /// Maps to `jj diff --summary -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_conflicts(&self, commit_id: &CommitId) -> Result<Vec<Conflict>> {
        let output = self
            .jj(["resolve", "--list", "-r", commit_id.as_str()])
            .run();

        match output {
            Ok(output) => Ok(output
                .lines()
                .filter_map(|line| {
                    let captured = CONFLICTS_REGEX.captures(line);
                    captured
                        .as_ref()
                        .and_then(|captured| captured.get(1))
                        .map(|inner_text| Conflict {
                            path: inner_text.as_str().to_owned(),
                        })
                })
                .collect()),
            Err(CommandError::Status(_, Some(2))) => {
                // No conflicts
                Ok(vec![])
            }
            Err(err) => Err(err).context("Failed getting conflicts"),
        }
    }

    /// Resolve conflicts in a revision by keeping one side wholesale.
    /// Maps to `jj resolve -r <revision> --tool :ours|:theirs [<fileset>]`
    ///
    /// With no `path`, every conflicted file in the revision is resolved.
    /// Each conflicted file takes the chosen side's *entire* content — jj's
    /// built-in `:ours`/`:theirs` tools resolve whole files, so changes from
    /// the discarded side are dropped even where they would have merged
    /// cleanly.
    #[instrument(level = "trace", skip(self))]
    pub fn run_resolve(
        &self,
        revision: &str,
        path: Option<&str>,
        side: ConflictSide,
    ) -> Result<(), CommandError> {
        let tool = match side {
            ConflictSide::Source => ":theirs",
            ConflictSide::Destination => ":ours",
        };
        let mut args = vec![
            "resolve".to_owned(),
            "-r".to_owned(),
            revision.to_owned(),
            "--tool".to_owned(),
            tool.to_owned(),
        ];
        if let Some(path) = path {
            args.push(Self::get_file_revset(path));
        }

        self.jj(args).run_void()
    }

    /// Get diff for file change in a change.
    /// Maps to `jj diff -r <revision> <path>`
    #[instrument(level = "trace", skip(self))]
    pub fn get_file_diff(
        &self,
        head: &Head,
        current_file: &File,
        diff_format: &DiffFormat,
        ignore_working_copy: bool,
    ) -> Result<Option<String>, CommandError> {
        let Some(path) = current_file.path.as_ref() else {
            return Ok(None);
        };

        let path = if let (true, Some(captures)) = (
            current_file.diff_type == Some(DiffType::Renamed),
            RENAME_REGEX.captures(path),
        ) {
            match captures.get(2) {
                Some(path) => path.as_str(),
                None => return Ok(None),
            }
        } else {
            path
        };

        let fileset = Self::get_file_revset(path);
        let mut args = vec!["diff", "-r", head.commit_id.as_str(), &fileset];
        args.append(&mut diff_format.get_args());
        if ignore_working_copy {
            args.push("--ignore-working-copy");
        }

        self.jj(args).color().run().map(Some)
    }

    #[instrument(level = "trace", skip(self))]
    pub fn untrack_file(&self, current_file: &File) -> Result<Option<String>, CommandError> {
        let Some(path) = current_file.path.as_ref() else {
            return Ok(None);
        };

        let path = if let Some(DiffType::Renamed) = current_file.diff_type
            && let Some(captures) = RENAME_REGEX.captures(path)
        {
            match captures.get(2) {
                Some(path) => path.as_str(),
                None => return Ok(None),
            }
        } else {
            path
        };

        let fileset = Self::get_file_revset(path);
        Ok(Some(self.jj(["file", "untrack", &fileset]).run()?))
    }

    #[instrument(level = "trace", skip(self))]
    pub fn restore_file(&self, current_file: &File) -> Result<Option<String>, CommandError> {
        let Some(path) = current_file.path.as_ref() else {
            return Ok(None);
        };

        let path = if let Some(DiffType::Renamed) = current_file.diff_type
            && let Some(captures) = RENAME_REGEX.captures(path)
        {
            match captures.get(2) {
                Some(path) => path.as_str(),
                None => return Ok(None),
            }
        } else {
            path
        };

        let fileset = Self::get_file_revset(path);
        Ok(Some(self.jj(["restore", &fileset]).run()?))
    }

    fn get_file_revset(path: &str) -> String {
        format!(
            "file:\"{}\"",
            path.replace("\\", "\\\\").replace('"', "\\\"")
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use insta::assert_debug_snapshot;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn get_files() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let file_path = test_repo.directory.path().join("README");

        // Initial state
        {
            let head = test_repo.commander.get_current_head()?;
            let files = test_repo.commander.get_files(&head)?;
            assert_eq!(files, vec![]);
        }

        // Add file
        {
            fs::write(&file_path, b"AAA")?;

            let head = test_repo.commander.get_current_head()?;
            let files = test_repo.commander.get_files(&head)?;
            assert_eq!(
                files,
                vec![File {
                    line: "A README".to_owned(),
                    path: Some("README".to_owned(),),
                    diff_type: Some(DiffType::Added,),
                },]
            );
        }

        // Commit
        test_repo.commander.jj(["new"]).run_void()?;

        // Modify file
        {
            fs::write(&file_path, b"BBB")?;

            let head = test_repo.commander.get_current_head()?;
            let files = test_repo.commander.get_files(&head)?;
            assert_eq!(
                files,
                vec![File {
                    line: "M README".to_owned(),
                    path: Some("README".to_owned()),
                    diff_type: Some(DiffType::Modified)
                },]
            );
        }

        // Delete file
        {
            fs::remove_file(&file_path)?;

            let head = test_repo.commander.get_current_head()?;
            let files = test_repo.commander.get_files(&head)?;
            assert_eq!(
                files,
                vec![File {
                    line: "D README".to_owned(),
                    path: Some("README".to_owned()),
                    diff_type: Some(DiffType::Deleted)
                },]
            );
        }

        Ok(())
    }

    #[test]
    fn get_file_diff() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let mut file_path = test_repo.directory.path().join("README");

        // Add file
        {
            fs::write(&file_path, b"AAA")?;
            let file = File {
                path: Some("README".to_string()),
                diff_type: Some(DiffType::Added),
                line: "A README".to_string(),
            };

            let head = test_repo.commander.get_current_head()?;
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::ColorWords,
                false
            )?);
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::Git,
                false
            )?);
        }

        // Commit
        test_repo.commander.jj(["new"]).run_void()?;

        // Modify file
        {
            fs::write(&file_path, b"BBB")?;
            let file = File {
                path: Some("README".to_string()),
                diff_type: Some(DiffType::Modified),
                line: "M README".to_string(),
            };

            let head = test_repo.commander.get_current_head()?;
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::ColorWords,
                true
            )?);
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::Git,
                true
            )?);
        }

        // Commit
        test_repo.commander.jj(["new"]).run_void()?;

        // Rename file
        {
            let file_path_new = test_repo.directory.path().join("README2");
            fs::rename(file_path, &file_path_new)?;
            file_path = file_path_new;

            let file = File {
                path: Some("{README => README2}".to_string()),
                diff_type: Some(DiffType::Renamed),
                line: "R {README => README2}".to_string(),
            };

            let head = test_repo.commander.get_current_head()?;
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::ColorWords,
                true
            )?);
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::Git,
                true
            )?);
        }

        // Commit
        test_repo.commander.jj(["new"]).run_void()?;

        // Delete file
        {
            fs::remove_file(&file_path)?;
            let file = File {
                path: Some("README2".to_string()),
                diff_type: Some(DiffType::Deleted),
                line: "D README2".to_string(),
            };

            let head = test_repo.commander.get_current_head()?;
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::ColorWords,
                true
            )?);
            assert_debug_snapshot!(test_repo.commander.get_file_diff(
                &head,
                &file,
                &DiffFormat::Git,
                true
            )?);
        }

        Ok(())
    }

    // Build a repo where a rebase left the working copy conflicted: two
    // siblings both edit README's first line ("AAA" on the destination side,
    // "BBB" on the rebased side, from a common "base" version), then the
    // "BBB" change is rebased onto the "AAA" change. The rebased side also
    // appends an "extra" line that would merge cleanly on its own, to pin
    // down that resolution takes the whole file from the chosen side.
    fn make_conflicted_repo() -> Result<TestRepo> {
        let test_repo = TestRepo::new()?;
        let file_path = test_repo.directory.path().join("README");

        fs::write(&file_path, b"base\ncommon\n")?;
        let head0 = test_repo.commander.get_current_head()?;

        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        let head1 = test_repo.commander.get_current_head()?;
        fs::write(&file_path, b"AAA\ncommon\n")?;

        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        let head2 = test_repo.commander.get_current_head()?;
        fs::write(&file_path, b"BBB\ncommon\nextra\n")?;

        test_repo
            .commander
            .jj([
                "rebase",
                "-s",
                head2.change_id.as_str(),
                "-d",
                head1.change_id.as_str(),
            ])
            .run_void()?;

        Ok(test_repo)
    }

    fn readme_content(test_repo: &TestRepo, revision: &str) -> Result<String> {
        Ok(test_repo
            .commander
            .jj(["file", "show", "-r", revision, "README"])
            .run()?)
    }

    #[test]
    fn run_resolve_keep_source() -> Result<()> {
        let test_repo = make_conflicted_repo()?;
        let head = test_repo.commander.get_current_head()?;
        assert!(
            !test_repo
                .commander
                .get_conflicts(&head.commit_id)?
                .is_empty()
        );

        test_repo
            .commander
            .run_resolve(head.commit_id.as_str(), None, ConflictSide::Source)?;

        let head = test_repo.commander.get_current_head()?;
        assert_eq!(test_repo.commander.get_conflicts(&head.commit_id)?, []);
        // The rebased revision's own content wins
        assert_eq!(
            readme_content(&test_repo, head.commit_id.as_str())?,
            "BBB\ncommon\nextra\n"
        );

        Ok(())
    }

    #[test]
    fn run_resolve_keep_destination() -> Result<()> {
        let test_repo = make_conflicted_repo()?;
        let head = test_repo.commander.get_current_head()?;

        test_repo.commander.run_resolve(
            head.commit_id.as_str(),
            None,
            ConflictSide::Destination,
        )?;

        let head = test_repo.commander.get_current_head()?;
        assert_eq!(test_repo.commander.get_conflicts(&head.commit_id)?, []);
        // The rebase destination's whole file wins: the rebased side's
        // "extra" line is dropped even though it would have merged cleanly
        assert_eq!(
            readme_content(&test_repo, head.commit_id.as_str())?,
            "AAA\ncommon\n"
        );

        Ok(())
    }

    #[test]
    fn run_resolve_single_file() -> Result<()> {
        let test_repo = make_conflicted_repo()?;
        let head = test_repo.commander.get_current_head()?;

        test_repo.commander.run_resolve(
            head.commit_id.as_str(),
            Some("README"),
            ConflictSide::Source,
        )?;

        let head = test_repo.commander.get_current_head()?;
        assert_eq!(test_repo.commander.get_conflicts(&head.commit_id)?, []);
        assert_eq!(
            readme_content(&test_repo, head.commit_id.as_str())?,
            "BBB\ncommon\nextra\n"
        );

        Ok(())
    }

    #[test]
    fn run_resolve_squash_conflict_sides() -> Result<()> {
        // jj orders conflict sides by operation role, not by which revision
        // holds the conflict: in a squash-introduced conflict, side #1 is the
        // squash destination's old content (the conflicted revision itself!)
        // and side #2 is the squashed revision's. Pin that
        // [ConflictSide::Source] means "the moved revision" here too.
        let test_repo = TestRepo::new()?;
        let file_path = test_repo.directory.path().join("README");

        fs::write(&file_path, b"base")?;
        let head0 = test_repo.commander.get_current_head()?;

        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        fs::write(&file_path, b"Y-version")?;
        let dest = test_repo.commander.get_current_head()?;

        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        fs::write(&file_path, b"X-version")?;
        let source = test_repo.commander.get_current_head()?;

        test_repo.commander.run_squash_into(
            std::slice::from_ref(&source.commit_id),
            dest.commit_id.as_str(),
            false,
        )?;

        let dest = test_repo
            .commander
            .get_change_head(&dest.change_id)?
            .expect("squash destination should still exist");
        assert!(
            !test_repo
                .commander
                .get_conflicts(&dest.commit_id)?
                .is_empty()
        );

        test_repo
            .commander
            .run_resolve(dest.commit_id.as_str(), None, ConflictSide::Source)?;

        let dest = test_repo
            .commander
            .get_change_head(&dest.change_id)?
            .expect("squash destination should still exist");
        assert_eq!(test_repo.commander.get_conflicts(&dest.commit_id)?, []);
        // The squashed (moved) revision's version wins, NOT the conflicted
        // revision's own old content
        assert_eq!(
            readme_content(&test_repo, dest.commit_id.as_str())?,
            "X-version"
        );

        Ok(())
    }

    #[test]
    fn run_resolve_no_conflicts() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let head = test_repo.commander.get_current_head()?;

        let result =
            test_repo
                .commander
                .run_resolve(head.commit_id.as_str(), None, ConflictSide::Source);

        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn get_conflicts() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let file_path = test_repo.directory.path().join("README");

        let head0 = test_repo.commander.get_current_head()?;

        // First change
        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        let head1 = test_repo.commander.get_current_head()?;
        fs::write(&file_path, b"AAA")?;

        test_repo.commander.run_new([head0.commit_id.as_str()])?;
        let head2 = test_repo.commander.get_current_head()?;
        fs::write(&file_path, b"BBB")?;

        test_repo
            .commander
            .jj([
                "rebase",
                "-s",
                head2.change_id.as_str(),
                "-d",
                head1.change_id.as_str(),
            ])
            .run_void()?;

        let head = test_repo.commander.get_current_head()?;

        let conflicts = test_repo.commander.get_conflicts(&head.commit_id)?;

        assert_eq!(
            conflicts,
            [Conflict {
                path: "README".to_owned()
            }]
        );

        Ok(())
    }
}
