/*!
[Commander] member functions related to jj diff.

This module has features to parse the diff output.
It is mostly used in the [files_tab][crate::ui::files_tab] module.
*/
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::Context;
use anyhow::Result;
use ratatui::style::Color;
use regex::Regex;
use tempfile::Builder;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::EditorCleanup;
use crate::commander::EditorCommand;
use crate::commander::InteractiveCommand;
use crate::commander::RemoveEndLine;
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

/// A working-copy file jj declined to snapshot, so it belongs to no revision.
#[derive(Clone, Debug, PartialEq)]
pub struct UntrackedFile {
    pub path: String,
    /// Why jj refused it, as reported by `jj status`, e.g.
    /// `2.9MiB (3000000 bytes); the maximum size allowed is 1.0MiB (...)`.
    /// `None` if jj listed the path without an accompanying warning.
    pub reason: Option<String>,
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

/// How `jj diff --stat` marks a binary file in the change column of its
/// per-file row, e.g. `blob.bin | (binary) +200 bytes`.
const BINARY_STAT_MARKER: &str = "(binary)";

/// Shown in the diff pane in place of a binary file's diff. Mirrors the wording
/// jj's own `--color-words` format uses, so the two formats read the same.
const BINARY_PLACEHOLDER: &str = "    (binary)";

// Example line: `A README.md`, `M src/main.rs`, `D Hello World`
static FILES_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.) (.*)").unwrap());
static RENAME_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{(.*?) => (.*?)\}").unwrap());
static CONFLICTS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(.*)    .*").unwrap());

/// jj's fileset pattern kinds, as accepted in a `kind:pattern` prefix. Used by
/// [Commander::quote_fileset] to tell an explicit pattern kind apart from a
/// plain path that merely contains a colon.
///
/// From `jj help -k filesets` (jj 0.44). Note there is no `regex:` kind -- an
/// unrecognized prefix here is quoted as part of the path instead, which makes
/// jj match it literally rather than fail, so a wrong entry degrades to "no
/// matches" rather than an error.
const FILESET_PATTERN_KINDS: &[&str] = &[
    "cwd",
    "cwd-file",
    "cwd-glob",
    "cwd-prefix-glob",
    "file",
    "glob",
    "prefix-glob",
    "root",
    "root-file",
    "root-glob",
    "root-prefix-glob",
];

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

    /// Files present in the working copy that jj did not snapshot, with the
    /// reason it refused each one. Maps to `jj status`.
    ///
    /// jj tracks everything that is not ignored, so in practice a file lands
    /// here by exceeding `snapshot.max-new-file-size`. These files are in no
    /// revision at all, which is why `jj diff` cannot show them and the files
    /// tab would otherwise not mention them.
    ///
    /// The paths come from the `Untracked paths:` section on stdout and the
    /// per-file reasons from the `Refused to snapshot some files:` warning on
    /// stderr. A path with no matching warning still appears, with no reason.
    #[instrument(level = "trace", skip(self))]
    pub fn get_untracked_files(&self) -> Result<Vec<UntrackedFile>> {
        let (stdout, stderr) = self
            .jj(["status"])
            .run_with_stderr()
            .context("Failed getting untracked files")?;

        let reasons = Self::parse_snapshot_refusals(&stderr);

        Ok(stdout
            .lines()
            .skip_while(|line| !line.starts_with("Untracked paths:"))
            .skip(1)
            // The section runs until the next unindented, non-`?` line
            .map_while(|line| line.strip_prefix("? "))
            .map(|path| UntrackedFile {
                path: path.to_owned(),
                reason: reasons.get(path).cloned(),
            })
            .collect())
    }

    /// Pull the per-file reasons out of jj's "Refused to snapshot some files:"
    /// warning, whose entries look like
    /// `  big.bin: 2.9MiB (3000000 bytes); the maximum size allowed is 1.0MiB (1048576 bytes)`.
    fn parse_snapshot_refusals(stderr: &str) -> HashMap<String, String> {
        stderr
            .lines()
            .skip_while(|line| !line.contains("Refused to snapshot some files:"))
            .skip(1)
            .map_while(|line| line.strip_prefix("  "))
            // Split on ": " rather than ':' so a path containing a colon is
            // not cut in half at the wrong place.
            .filter_map(|line| line.split_once(": "))
            .map(|(path, reason)| (path.to_owned(), reason.to_owned()))
            .collect()
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

    /// Build the invocation for resolving a revision's conflicts in the
    /// configured merge editor (`ui.merge-editor`), one conflicted file at
    /// a time. Maps to `jj resolve -r <revision> [<fileset>]`; with no
    /// `path`, jj walks every conflicted file in the revision.
    ///
    /// Returns the command for the main loop to run with the terminal
    /// handed over ([crate::commander::JjCommand::run_interactive]), since
    /// merge editors are interactive programs.
    pub fn resolve_interactive_command(revision: &str, path: Option<&str>) -> InteractiveCommand {
        let mut args = vec!["resolve".to_owned(), "-r".to_owned(), revision.to_owned()];
        if let Some(path) = path {
            args.push(Self::get_file_revset(path));
        }

        InteractiveCommand {
            args,
            name: "Interactive resolve".to_owned(),
        }
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

        // jj's own diff formats print a placeholder for binary files, but a
        // `--tool` format pipes the external tool's stdout through untouched,
        // and such a tool may dump raw file contents. Ask jj whether the file
        // is binary first and render our own placeholder if so, rather than
        // filling the pane with mojibake.
        if matches!(diff_format, DiffFormat::DiffTool(_))
            && self.is_file_binary(head, &fileset, ignore_working_copy)?
        {
            return Ok(Some(BINARY_PLACEHOLDER.to_owned()));
        }

        let mut args = vec!["diff", "-r", head.commit_id.as_str(), &fileset];
        args.append(&mut diff_format.get_args());
        if ignore_working_copy {
            args.push("--ignore-working-copy");
        }

        // Rendered, never parsed -- see [Command::run_lossy]. A diff tool that
        // emits undecodable bytes garbles its own output instead of taking the
        // whole preview pane down with it.
        self.jj(args).color().run_lossy().map(Some)
    }

    /// Whether jj considers the single file selected by `fileset` to be binary.
    ///
    /// Asks `jj diff --stat`, whose per-file row reads `<path> | (binary) +N
    /// bytes` for a binary file and `<path> | N +-` for a text one. The fileset
    /// selects one file, so the marker is looked for anywhere in the rows
    /// rather than matched against a path: `--stat` elides the front of a long
    /// path with `...`, which would defeat matching, and a *file* named
    /// `(binary) trap.txt` cannot produce a false positive because its row is
    /// the only one and its marker sits left of the `|` column separator.
    ///
    /// The trailing summary line (`1 file changed, ...`) never contains the
    /// marker, so it is skipped implicitly.
    #[instrument(level = "trace", skip(self))]
    fn is_file_binary(
        &self,
        head: &Head,
        fileset: &str,
        ignore_working_copy: bool,
    ) -> Result<bool, CommandError> {
        let mut args = vec!["diff", "-r", head.commit_id.as_str(), fileset, "--stat"];
        if ignore_working_copy {
            args.push("--ignore-working-copy");
        }

        // No .color(): the markers are matched literally, so styling would only
        // interleave escape sequences into the text being searched.
        let stat = self.jj(args).run()?;
        Ok(stat
            .lines()
            .filter_map(|line| line.split_once('|'))
            .any(|(_path, change)| change.contains(BINARY_STAT_MARKER)))
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

    /// Add the file to the repo-root `.gitignore` and then untrack it.
    ///
    /// Both steps are needed together: `jj file untrack` refuses to untrack a
    /// file that is not ignored, since the next command would just add it back.
    /// The file itself is left on disk — only version control forgets it.
    ///
    /// Returns whether a new `.gitignore` line was added; an already-ignored
    /// file is untracked without touching `.gitignore`.
    #[instrument(level = "trace", skip(self))]
    pub fn ignore_and_untrack_file(&self, current_file: &File) -> Result<bool> {
        let Some(path) = Self::destination_path(current_file) else {
            return Ok(false);
        };

        let added = self.append_to_gitignore(&Self::gitignore_pattern(path))?;
        // Untrack after ignoring, so jj sees the file as ignored and accepts it.
        self.untrack_file(current_file)
            .context("Failed untracking the file after adding it to .gitignore")?;
        Ok(added)
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

    /// The `.gitignore` pattern that ignores exactly `path`.
    ///
    /// Anchored with a leading slash so it matches that one file relative to the
    /// repo root, rather than every similarly named file in any directory.
    /// Separators are normalized to `/`, which is what gitignore expects even on
    /// Windows, and the gitignore metacharacters are escaped so a path
    /// containing them still matches literally.
    pub fn gitignore_pattern(path: &str) -> String {
        let escaped: String = path
            .replace('\\', "/")
            .chars()
            .flat_map(|c| {
                let escape = matches!(c, '*' | '?' | '[' | ']' | '!' | '#' | ' ' | '\\');
                escape.then_some('\\').into_iter().chain(std::iter::once(c))
            })
            .collect();
        format!("/{}", escaped.trim_start_matches('/'))
    }

    /// Append `pattern` to the repo-root `.gitignore`, and report whether it was
    /// added. A pattern already present is left alone, so repeating the action
    /// does not accumulate duplicate lines.
    ///
    /// A `.gitignore` whose last line lacks a newline gets one first, so the
    /// appended pattern does not run onto the end of the previous entry.
    #[instrument(level = "trace", skip(self))]
    pub fn append_to_gitignore(&self, pattern: &str) -> Result<bool> {
        let gitignore = Path::new(&self.env.root).join(".gitignore");

        let existing = match std::fs::read_to_string(&gitignore) {
            Ok(existing) => existing,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed reading {}", gitignore.to_string_lossy()));
            }
        };

        if existing.lines().any(|line| line.trim() == pattern) {
            return Ok(false);
        }

        let mut contents = existing;
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(pattern);
        contents.push('\n');

        std::fs::write(&gitignore, contents)
            .with_context(|| format!("Failed writing {}", gitignore.to_string_lossy()))?;
        Ok(true)
    }

    fn get_file_revset(path: &str) -> String {
        format!(
            "file:\"{}\"",
            path.replace("\\", "\\\\").replace('"', "\\\"")
        )
    }

    /// The revset selecting every revision that modifies exactly `path`.
    ///
    /// Used by the files tab's "mark the revisions touching this file" handoff,
    /// where the path came from jj's diff summary rather than from the user, so
    /// matching it exactly is unambiguous. Typed input goes through
    /// [Self::files_revset] instead, whose default `prefix-glob:` kind lets a
    /// directory or a glob work.
    pub(crate) fn exact_file_fileset_revset(path: &str) -> String {
        format!("files({})", Self::get_file_revset(path))
    }

    /// Quote a user-typed fileset for interpolation into a revset expression.
    ///
    /// A bare value is quoted whole, which gives it jj's *default* pattern kind
    /// (`prefix-glob:`) -- so `src/ui` matches everything under that directory
    /// and `src/*.rs` honours the glob, which is what "touching this path" means
    /// to a user. Quoting the whole thing would break an explicit pattern kind,
    /// though: jj reads `files("glob:src/**")` as a file literally *named*
    /// `glob:src/**` and matches nothing, where `files(glob:"src/**")` matches
    /// as intended. So a recognized `kind:` prefix is kept outside the quotes.
    ///
    /// Only a prefix from [FILESET_PATTERN_KINDS] is split off, so a plain path
    /// that happens to contain a colon (`weird:name.txt`, `C:\repo\f.txt`) is
    /// still quoted whole rather than being read as an unknown pattern kind --
    /// which jj would reject outright.
    fn quote_fileset(fileset: &str) -> String {
        fn quote(value: &str) -> String {
            format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
        }

        if let Some((kind, rest)) = fileset.split_once(':')
            && FILESET_PATTERN_KINDS.contains(&kind)
        {
            return format!("{kind}:{}", quote(rest));
        }

        quote(fileset)
    }

    /// The revset that selects every revision modifying `fileset`.
    ///
    /// Kept separate from running it so the same expression can be handed to
    /// [Commander::get_changes_in][crate::commander::Commander::get_changes_in]
    /// to mark revisions, or dropped straight into the log's revset field to
    /// filter by it.
    pub(crate) fn files_revset(fileset: &str) -> String {
        format!("files({})", Self::quote_fileset(fileset))
    }

    /// The post-change path of a file, resolving a rename to its new name.
    /// Returns `None` if there is no path (e.g. a blank line).
    pub fn destination_path(current_file: &File) -> Option<&str> {
        let path = current_file.path.as_deref()?;
        if current_file.diff_type == Some(DiffType::Renamed)
            && let Some(captures) = RENAME_REGEX.captures(path)
        {
            return captures.get(2).map(|m| m.as_str());
        }
        Some(path)
    }

    /// The editor command, resolved the way jj resolves it: the `ui.editor`
    /// config, then `$VISUAL`, then `$EDITOR`, then a plain `vi` fallback.
    /// Split on whitespace so a configured value like `"code --wait"` keeps
    /// its arguments.
    pub(crate) fn editor_argv(&self) -> Vec<String> {
        let raw = self
            .jj(["config", "get", "ui.editor"])
            .run()
            .ok()
            .map(|value| value.remove_end_line())
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("VISUAL").ok().filter(|v| !v.is_empty()))
            .or_else(|| std::env::var("EDITOR").ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "vi".to_owned());

        raw.split_whitespace().map(String::from).collect()
    }

    /// Build the command that opens the selected file in the user's editor.
    ///
    /// When `is_current_head` is true the file lives on disk in the working
    /// copy, so it is opened directly and edits are saved in place. For any
    /// other revision the content is not on disk, so it is materialized from
    /// `jj file show -r <rev> <path>` into a temp file and opened read-only
    /// (`-R` for vi-family editors) — editing history in place is a separate
    /// gesture (`jj edit`/`diffedit`), not what "open this file" means.
    ///
    /// Returns `None` for a row with no openable path (e.g. a blank line).
    #[instrument(level = "trace", skip(self))]
    pub fn open_file_command(
        &self,
        head: &Head,
        current_file: &File,
        is_current_head: bool,
    ) -> Result<Option<EditorCommand>, CommandError> {
        let Some(path) = Self::destination_path(current_file) else {
            return Ok(None);
        };

        let mut argv = self.editor_argv();
        let editor_name = argv.first().cloned().unwrap_or_default();

        if is_current_head {
            // The working-copy file on disk. Open it live; never delete it.
            let full_path = Path::new(&self.env.root).join(path);
            argv.push(full_path.to_string_lossy().into_owned());
            return Ok(Some(EditorCommand {
                argv,
                name: format!("Open {path}"),
                cleanup: None,
                working_dir: None,
            }));
        }

        // Not the working copy: materialize the revision's content to a temp
        // file and open it read-only, so the editor shows exactly what the
        // diff panel shows without pretending it is editable.
        let contents = self
            .jj(["file", "show", "-r", head.commit_id.as_str(), path])
            .run()?;

        let suffix = Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{ext}"))
            .unwrap_or_default();
        let mut temp = Builder::new()
            .prefix("jjscope-")
            .suffix(&suffix)
            .tempfile()?;
        temp.write_all(contents.as_bytes())?;
        let temp_path = temp.into_temp_path();

        if is_read_only_capable(&editor_name) {
            argv.push("-R".to_owned());
        }
        argv.push(temp_path.to_string_lossy().into_owned());

        Ok(Some(EditorCommand {
            argv,
            name: format!("View {path} @ {}", head.change_id),
            cleanup: Some(EditorCleanup::File(temp_path)),
            working_dir: None,
        }))
    }
}

/// Whether `editor` accepts vi's `-R` read-only flag. Covers the vi family
/// (vi/vim/nvim/view); anything else is opened without the flag rather than
/// risk passing an argument it would treat as a filename.
pub(crate) fn is_read_only_capable(editor: &str) -> bool {
    let name = Path::new(editor)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(editor);
    matches!(name, "vi" | "vim" | "nvim" | "view" | "vimr")
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

    /// Register a real external diff tool on `test_repo` and return the format
    /// that selects it.
    ///
    /// jj invokes a diff tool with two *directory* paths, so the stand-in is
    /// `diff -r`, which is present anywhere the rest of this suite already
    /// assumes a POSIX toolchain. `--tool` accepts a program name only, so the
    /// arguments have to come from a `merge-tools` entry.
    fn register_diff_tool(test_repo: &mut TestRepo) -> DiffFormat {
        let config = test_repo
            .commander
            .jj_config_toml
            .get_or_insert_with(Vec::new);
        config.push(r#"merge-tools.dirdiff.program="diff""#.to_owned());
        config.push(r#"merge-tools.dirdiff.diff-args=["-r","$left","$right"]"#.to_owned());
        DiffFormat::DiffTool(Some("dirdiff".to_owned()))
    }

    /// A binary file gets the placeholder instead of the diff tool's raw bytes.
    ///
    /// jj's own formats already print a placeholder, so the regression this
    /// pins down is the `--tool` path, where jj passes the tool's stdout
    /// through verbatim. `cat` stands in for a real tool that dumps file
    /// contents; without the binary check its output is undecodable bytes.
    #[test]
    fn get_file_diff_binary_with_diff_tool() -> Result<()> {
        let mut test_repo = TestRepo::new()?;
        let diff_format = register_diff_tool(&mut test_repo);

        // Invalid UTF-8 (a lone continuation byte) plus a NUL, so the content
        // is both binary to jj and undecodable to Rust.
        fs::write(
            test_repo.directory.path().join("blob.bin"),
            b"\x00\x01\x02\xff\xfe binary \x80\x00",
        )?;
        let file = File {
            path: Some("blob.bin".to_string()),
            diff_type: Some(DiffType::Added),
            line: "A blob.bin".to_string(),
        };

        let head = test_repo.commander.get_current_head()?;
        let diff = test_repo
            .commander
            .get_file_diff(&head, &file, &diff_format, false)?;

        assert_eq!(diff.as_deref(), Some(BINARY_PLACEHOLDER));

        Ok(())
    }

    /// A text file still gets a real diff from the same diff-tool path, so the
    /// binary check does not swallow ordinary previews.
    ///
    #[test]
    fn get_file_diff_text_with_diff_tool() -> Result<()> {
        let mut test_repo = TestRepo::new()?;
        let diff_format = register_diff_tool(&mut test_repo);

        // Modify rather than add: `diff -r` reports an added file as "Only in
        // right: ...", so a modification is what puts content in the output.
        let readme = test_repo.directory.path().join("README");
        fs::write(&readme, b"hello\n")?;
        test_repo.commander.jj(["new"]).run_void()?;
        fs::write(&readme, b"goodbye\n")?;

        let file = File {
            path: Some("README".to_string()),
            diff_type: Some(DiffType::Modified),
            line: "M README".to_string(),
        };

        let head = test_repo.commander.get_current_head()?;
        let diff = test_repo
            .commander
            .get_file_diff(&head, &file, &diff_format, false)?
            .expect("a text file has a diff");

        assert_ne!(diff, BINARY_PLACEHOLDER);
        assert!(diff.contains("goodbye"), "unexpected diff: {diff:?}");

        Ok(())
    }

    /// A file whose *name* contains the stat marker is not mistaken for binary.
    #[test]
    fn get_file_diff_path_containing_binary_marker() -> Result<()> {
        let mut test_repo = TestRepo::new()?;
        let diff_format = register_diff_tool(&mut test_repo);

        fs::write(
            test_repo.directory.path().join("(binary) trap.txt"),
            b"hello\n",
        )?;
        let file = File {
            path: Some("(binary) trap.txt".to_string()),
            diff_type: Some(DiffType::Added),
            line: "A (binary) trap.txt".to_string(),
        };

        let head = test_repo.commander.get_current_head()?;
        let diff = test_repo
            .commander
            .get_file_diff(&head, &file, &diff_format, false)?
            .expect("a text file has a diff");

        assert_ne!(diff, BINARY_PLACEHOLDER);

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

    /// Pin `ui.editor` so [Commander::editor_argv] is deterministic in tests
    /// regardless of the developer's `$EDITOR`/`ui.editor`.
    fn with_editor(mut test_repo: TestRepo, editor: &str) -> TestRepo {
        let mut cfg = test_repo
            .commander
            .jj_config_toml
            .take()
            .unwrap_or_default();
        cfg.push(format!(r#"ui.editor="{editor}""#));
        test_repo.commander.jj_config_toml = Some(cfg);
        test_repo
    }

    #[test]
    fn open_file_command_current_head_opens_disk_path() -> Result<()> {
        let test_repo = with_editor(TestRepo::new()?, "nvim");
        let file_path = test_repo.directory.path().join("README");
        fs::write(&file_path, b"AAA")?;

        let head = test_repo.commander.get_current_head()?;
        let file = File {
            line: "A README".to_owned(),
            path: Some("README".to_owned()),
            diff_type: Some(DiffType::Added),
        };

        let command = test_repo
            .commander
            .open_file_command(&head, &file, true)?
            .expect("a path should yield a command");

        // Editing @: open the live on-disk file, no read-only flag, no cleanup
        let disk_path = file_path.to_string_lossy().into_owned();
        assert_eq!(command.argv, vec!["nvim".to_owned(), disk_path]);
        assert!(command.cleanup.is_none());

        Ok(())
    }

    #[test]
    fn open_file_command_other_revision_is_read_only_temp() -> Result<()> {
        let test_repo = with_editor(TestRepo::new()?, "nvim");
        let file_path = test_repo.directory.path().join("README");

        // Commit a version, then move on so the head we open is not @
        fs::write(&file_path, b"first\n")?;
        let committed = test_repo.commander.get_current_head()?;
        test_repo.commander.jj(["new"]).run_void()?;
        fs::write(&file_path, b"second\n")?;

        let file = File {
            line: "A README".to_owned(),
            path: Some("README".to_owned()),
            diff_type: Some(DiffType::Added),
        };

        let command = test_repo
            .commander
            .open_file_command(&committed, &file, false)?
            .expect("a path should yield a command");

        // Viewing another revision: read-only flag, a temp file (not the
        // on-disk path), and a cleanup handle
        assert_eq!(command.argv[0], "nvim");
        assert_eq!(command.argv[1], "-R");
        let opened = &command.argv[2];
        assert!(opened.contains("jjscope-"), "unexpected path: {opened}");
        assert!(opened.ends_with(".README") || opened.contains("jjscope-"));
        assert!(command.cleanup.is_some());

        // The temp file holds the *committed* revision's content, not @'s
        let contents = fs::read_to_string(opened)?;
        assert_eq!(contents, "first\n");

        Ok(())
    }

    #[test]
    fn open_file_command_no_path_is_none() -> Result<()> {
        let test_repo = with_editor(TestRepo::new()?, "nvim");
        let head = test_repo.commander.get_current_head()?;
        let file = File {
            line: String::new(),
            path: None,
            diff_type: None,
        };
        assert!(
            test_repo
                .commander
                .open_file_command(&head, &file, true)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn open_file_command_non_vi_editor_has_no_read_only_flag() -> Result<()> {
        let test_repo = with_editor(TestRepo::new()?, "code --wait");
        let file_path = test_repo.directory.path().join("README");
        fs::write(&file_path, b"first\n")?;
        let committed = test_repo.commander.get_current_head()?;
        test_repo.commander.jj(["new"]).run_void()?;

        let file = File {
            line: "A README".to_owned(),
            path: Some("README".to_owned()),
            diff_type: Some(DiffType::Added),
        };

        let command = test_repo
            .commander
            .open_file_command(&committed, &file, false)?
            .expect("a path should yield a command");

        // "code --wait" splits into two argv entries; no -R is injected for a
        // non-vi editor, and the temp path is appended last.
        assert_eq!(command.argv[0], "code");
        assert_eq!(command.argv[1], "--wait");
        assert!(!command.argv.iter().any(|a| a == "-R"));
        assert!(command.argv.last().unwrap().contains("jjscope-"));

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

#[cfg(test)]
mod gitignore_tests {
    use std::fs;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn gitignore_pattern_anchors_to_repo_root() {
        // Anchored, so it matches this one file and not a like-named file
        // elsewhere in the tree.
        assert_eq!(Commander::gitignore_pattern("secret.env"), "/secret.env");
        assert_eq!(
            Commander::gitignore_pattern("sub/nested.tmp"),
            "/sub/nested.tmp"
        );
        // An already-rooted path does not gain a second slash.
        assert_eq!(Commander::gitignore_pattern("/rooted.txt"), "/rooted.txt");
        // Windows separators normalize; gitignore always uses forward slashes.
        assert_eq!(Commander::gitignore_pattern(r"sub\win.txt"), "/sub/win.txt");
    }

    #[test]
    fn gitignore_pattern_escapes_metacharacters() {
        // A literal `*` in a filename must not become a wildcard.
        assert_eq!(Commander::gitignore_pattern("wei*rd.txt"), "/wei\\*rd.txt");
        assert_eq!(Commander::gitignore_pattern("a?b.txt"), "/a\\?b.txt");
        assert_eq!(Commander::gitignore_pattern("[x].txt"), "/\\[x\\].txt");
        // A leading `!` would otherwise negate, and `#` would comment out.
        assert_eq!(Commander::gitignore_pattern("!bang.txt"), "/\\!bang.txt");
        assert_eq!(Commander::gitignore_pattern("#hash.txt"), "/\\#hash.txt");
        // A space would otherwise be trimmed by git.
        assert_eq!(Commander::gitignore_pattern("two words"), "/two\\ words");
    }

    #[test]
    fn append_to_gitignore_creates_file() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let gitignore = test_repo.directory.path().join(".gitignore");

        assert!(test_repo.commander.append_to_gitignore("/a.txt")?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/a.txt\n");

        Ok(())
    }

    #[test]
    fn append_to_gitignore_is_idempotent() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let gitignore = test_repo.directory.path().join(".gitignore");

        assert!(test_repo.commander.append_to_gitignore("/a.txt")?);
        // Repeating the action reports "not added" and does not duplicate.
        assert!(!test_repo.commander.append_to_gitignore("/a.txt")?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/a.txt\n");

        // A different pattern still appends.
        assert!(test_repo.commander.append_to_gitignore("/b.txt")?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/a.txt\n/b.txt\n");

        Ok(())
    }

    #[test]
    fn append_to_gitignore_fixes_missing_trailing_newline() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let gitignore = test_repo.directory.path().join(".gitignore");

        // A hand-edited .gitignore whose last line has no newline: appending
        // naively would corrupt that entry.
        fs::write(&gitignore, "/existing.txt")?;
        assert!(test_repo.commander.append_to_gitignore("/new.txt")?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/existing.txt\n/new.txt\n");

        Ok(())
    }

    #[test]
    fn append_to_gitignore_matches_existing_entry_with_whitespace() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let gitignore = test_repo.directory.path().join(".gitignore");

        // An entry with trailing whitespace still counts as present.
        fs::write(&gitignore, "/a.txt  \n")?;
        assert!(!test_repo.commander.append_to_gitignore("/a.txt")?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/a.txt  \n");

        Ok(())
    }

    #[test]
    fn ignore_and_untrack_file_stops_tracking_but_keeps_file() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let file_path = test_repo.directory.path().join("secret.env");
        fs::write(&file_path, b"token")?;

        // The file starts out tracked as an addition.
        let head = test_repo.commander.get_current_head()?;
        let files = test_repo.commander.get_files(&head)?;
        assert!(
            files
                .iter()
                .any(|f| f.path.as_deref() == Some("secret.env")),
            "expected secret.env to be tracked, got {files:?}"
        );

        let current_file = files
            .iter()
            .find(|f| f.path.as_deref() == Some("secret.env"))
            .unwrap()
            .clone();
        assert!(test_repo.commander.ignore_and_untrack_file(&current_file)?);

        // It is ignored, no longer tracked, and still on disk.
        let gitignore = test_repo.directory.path().join(".gitignore");
        assert_eq!(fs::read_to_string(&gitignore)?, "/secret.env\n");

        let head = test_repo.commander.get_current_head()?;
        let files = test_repo.commander.get_files(&head)?;
        assert!(
            !files
                .iter()
                .any(|f| f.path.as_deref() == Some("secret.env")),
            "expected secret.env to be untracked, got {files:?}"
        );
        assert_eq!(fs::read_to_string(&file_path)?, "token");

        Ok(())
    }

    #[test]
    fn ignore_and_untrack_file_already_ignored() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let gitignore = test_repo.directory.path().join(".gitignore");
        let file_path = test_repo.directory.path().join("app.log");

        // Committed first, then ignored: still tracked despite the ignore rule.
        fs::write(&file_path, b"log")?;
        let head = test_repo.commander.get_current_head()?;
        let files = test_repo.commander.get_files(&head)?;
        let current_file = files
            .iter()
            .find(|f| f.path.as_deref() == Some("app.log"))
            .unwrap()
            .clone();
        fs::write(&gitignore, "/app.log\n")?;

        // Reports that no line was added, but still untracks.
        assert!(!test_repo.commander.ignore_and_untrack_file(&current_file)?);
        assert_eq!(fs::read_to_string(&gitignore)?, "/app.log\n");

        let head = test_repo.commander.get_current_head()?;
        let files = test_repo.commander.get_files(&head)?;
        assert!(
            !files.iter().any(|f| f.path.as_deref() == Some("app.log")),
            "expected app.log to be untracked, got {files:?}"
        );

        Ok(())
    }
}

#[cfg(test)]
mod untracked_tests {
    use std::fs;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn parse_snapshot_refusals_extracts_reasons() {
        let stderr = "\
Warning: Refused to snapshot some files:
  bigtext.log: 7.3MiB (7688890 bytes); the maximum size allowed is 64.0B (64 bytes)
  blob.bin: 2.9MiB (3000000 bytes); the maximum size allowed is 64.0B (64 bytes)
Hint: This is to prevent large files from being added by accident. To fix this:
  * Add the file(s) to `.gitignore`
";
        let reasons = Commander::parse_snapshot_refusals(stderr);

        assert_eq!(reasons.len(), 2);
        assert_eq!(
            reasons.get("bigtext.log").map(String::as_str),
            Some("7.3MiB (7688890 bytes); the maximum size allowed is 64.0B (64 bytes)")
        );
        // The `Hint:` block ends the section, so its bullets are not entries.
        assert!(!reasons.contains_key("* Add the file(s) to `.gitignore`"));
    }

    #[test]
    fn parse_snapshot_refusals_handles_colon_in_path() {
        let stderr = "\
Warning: Refused to snapshot some files:
  weird:name.bin: 2.0MiB (2097152 bytes); the maximum size allowed is 1.0B (1 bytes)
";
        let reasons = Commander::parse_snapshot_refusals(stderr);
        assert_eq!(
            reasons.get("weird:name.bin").map(String::as_str),
            Some("2.0MiB (2097152 bytes); the maximum size allowed is 1.0B (1 bytes)")
        );
    }

    #[test]
    fn parse_snapshot_refusals_empty_when_absent() {
        assert!(Commander::parse_snapshot_refusals("").is_empty());
        assert!(Commander::parse_snapshot_refusals("Working copy  (@) : abc\n").is_empty());
    }

    #[test]
    fn get_untracked_files_reports_refused_files() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // Nothing refused yet.
        assert_eq!(test_repo.commander.get_untracked_files()?, vec![]);

        // A file over the snapshot limit is refused, so it lands in no revision.
        let refused = test_repo.directory.path().join("big.bin");
        fs::write(&refused, vec![b'x'; 4096])?;
        let mut commander = test_repo.commander.clone();
        commander
            .jj_config_toml
            .get_or_insert_with(Vec::new)
            .push("snapshot.max-new-file-size=64".to_owned());

        let untracked = commander.get_untracked_files()?;
        assert_eq!(untracked.len(), 1, "got {untracked:?}");
        assert_eq!(untracked[0].path, "big.bin");
        let reason = untracked[0].reason.as_deref().unwrap_or_default();
        assert!(
            reason.contains("maximum size allowed"),
            "expected a size reason, got {reason:?}"
        );

        // It is genuinely absent from the revision's file list.
        let head = commander.get_current_head()?;
        let files = commander.get_files(&head)?;
        assert!(
            !files.iter().any(|f| f.path.as_deref() == Some("big.bin")),
            "refused file should not be in the diff summary, got {files:?}"
        );

        Ok(())
    }
}

#[cfg(test)]
mod files_revset_tests {
    use std::fs;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn files_revset_selects_only_the_touching_change() -> Result<()> {
        let test_repo = TestRepo::new()?;

        fs::write(test_repo.directory.path().join("a.txt"), b"A")?;
        let touching = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([touching.commit_id.as_str()])?;
        fs::write(test_repo.directory.path().join("b.txt"), b"B")?;
        let other = test_repo.commander.get_current_head()?;

        let changes = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("a.txt"))?;

        assert!(changes.contains(&touching.change_id), "got {changes:?}");
        assert!(!changes.contains(&other.change_id), "got {changes:?}");

        Ok(())
    }

    #[test]
    fn files_revset_matches_a_whole_directory() -> Result<()> {
        // A bare path uses jj's default pattern kind (`prefix-glob:`), so naming
        // a directory matches everything under it. Regression test: quoting the
        // fileset as `file:"dir"` instead would make this exact-match and find
        // nothing, silently breaking every directory and glob query.
        let test_repo = TestRepo::new()?;

        fs::create_dir(test_repo.directory.path().join("dir"))?;
        fs::write(test_repo.directory.path().join("dir/f.txt"), b"A")?;
        let head = test_repo.commander.get_current_head()?;

        let changes = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("dir"))?;

        assert!(changes.contains(&head.change_id), "got {changes:?}");

        Ok(())
    }

    #[test]
    fn files_revset_honours_an_explicit_pattern_kind() -> Result<()> {
        // A recognized `kind:` prefix stays outside the quotes, so jj applies the
        // kind instead of reading it as part of the filename.
        let test_repo = TestRepo::new()?;

        fs::write(test_repo.directory.path().join("f.rs"), b"A")?;
        let head = test_repo.commander.get_current_head()?;

        let changes = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("glob:*.rs"))?;
        assert!(changes.contains(&head.change_id), "got {changes:?}");

        // `file:` is exact, so the directory-style prefix match must NOT apply.
        let exact = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("file:f.rs"))?;
        assert!(exact.contains(&head.change_id), "got {exact:?}");

        Ok(())
    }

    #[test]
    fn files_revset_nonexistent_path_is_empty_not_an_error() -> Result<()> {
        // An empty result and an error are different outcomes: the UI says "no
        // revisions touch this" for one and shows jj's diagnostic for the other.
        let test_repo = TestRepo::new()?;

        fs::write(test_repo.directory.path().join("a.txt"), b"A")?;

        let changes = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("no/such/path"))?;

        assert!(changes.is_empty(), "got {changes:?}");

        Ok(())
    }

    #[test]
    fn files_revset_malformed_pattern_errors() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let result = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("glob:["));

        assert!(result.is_err(), "expected an error, got {result:?}");

        Ok(())
    }

    #[test]
    fn files_revset_survives_a_rewrite() -> Result<()> {
        // The set is keyed by CHANGE id so it stays valid across the commit-id
        // rewrite that every jj operation performs. Keying by commit id would
        // drop the mark from a revision that still touches the file.
        let test_repo = TestRepo::new()?;

        fs::write(test_repo.directory.path().join("a.txt"), b"A")?;
        let before = test_repo.commander.get_current_head()?;

        test_repo
            .commander
            .run_describe(before.commit_id.as_str(), "rewritten")?;

        let after = test_repo
            .commander
            .get_change_head(&before.change_id)?
            .expect("change should still exist after describe");

        // The rewrite really did change the commit id, so this test is
        // meaningful rather than vacuous.
        assert_ne!(after.commit_id, before.commit_id);
        assert_eq!(after.change_id, before.change_id);

        let changes = test_repo
            .commander
            .get_changes_in(&Commander::files_revset("a.txt"))?;
        assert!(changes.contains(&before.change_id), "got {changes:?}");

        Ok(())
    }

    #[test]
    fn quote_fileset_quotes_a_bare_path() {
        assert_eq!(Commander::quote_fileset("src/app.rs"), r#""src/app.rs""#);
    }

    #[test]
    fn quote_fileset_escapes_quotes_and_backslashes() {
        assert_eq!(Commander::quote_fileset(r#"a"b"#), r#""a\"b""#);
        assert_eq!(Commander::quote_fileset(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn quote_fileset_keeps_a_known_pattern_kind_outside_the_quotes() {
        assert_eq!(
            Commander::quote_fileset("glob:src/**/*.rs"),
            r#"glob:"src/**/*.rs""#
        );
        assert_eq!(
            Commander::quote_fileset("root-file:src/app.rs"),
            r#"root-file:"src/app.rs""#
        );
    }

    #[test]
    fn quote_fileset_treats_an_unknown_prefix_as_part_of_the_path() {
        // A colon in a plain filename must not be mistaken for a pattern kind --
        // jj rejects an unknown kind outright, so splitting here would turn a
        // valid (if unusual) path into a hard error.
        assert_eq!(
            Commander::quote_fileset("weird:name.txt"),
            r#""weird:name.txt""#
        );
    }

    #[test]
    fn exact_file_fileset_revset_is_an_exact_match() {
        // The files-tab handoff names a concrete file, so it matches exactly
        // rather than as a directory prefix.
        assert_eq!(
            Commander::exact_file_fileset_revset("src/app.rs"),
            r#"files(file:"src/app.rs")"#
        );
    }

    #[test]
    fn files_revset_uses_the_default_pattern_kind() {
        // No `file:` prefix, so jj applies `prefix-glob:` -- a directory matches
        // everything under it.
        assert_eq!(Commander::files_revset("src/ui"), r#"files("src/ui")"#);
    }
}
