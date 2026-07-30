/*!
[Commander] members for materializing a whole revision's file tree into a
temporary directory, so the user can open the repo *as of that revision* in
their editor.

The tree is extracted with `git archive` against jj's underlying git object
store. jj's git backend writes every commit to that store as it goes, so even
a just-created working-copy commit is archivable without asking jj to export
anything.

Finding the store is the fiddly part, because the working directory may be a
[workspace][jj workspaces] rather than the main repo, and the repo may not be
colocated (no `.git` next to the working copy). Both cases are handled by
following jj's own pointers — see [Commander::resolve_git_dir].

[jj workspaces]: https://docs.jj-vcs.dev/latest/glossary/#workspace
*/

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use tempfile::Builder;
use tracing::instrument;

use crate::commander::Commander;
use crate::commander::EditorCleanup;
use crate::commander::EditorCommand;
use crate::commander::log::Head;

impl Commander {
    /// Locate jj's underlying git object store for this repo.
    ///
    /// The chain, starting from the workspace root:
    ///
    /// * `.jj/repo` is either the repo directory itself, or (in a secondary
    ///   workspace) a *file* whose contents are a path to the main repo's
    ///   `.jj/repo`, relative to the workspace's own `.jj/`.
    /// * `<repo>/store/git_target` is a file whose contents are a path to the
    ///   git directory, relative to `<repo>/store/`.
    ///
    /// Returns an error if the repo isn't backed by git (`git_target` absent),
    /// since there is then no object store to archive from.
    #[instrument(level = "trace", skip(self))]
    pub fn resolve_git_dir(&self) -> Result<PathBuf> {
        let jj_dir = Path::new(&self.env.root).join(".jj");
        let repo_pointer = jj_dir.join("repo");

        // In a secondary workspace `.jj/repo` is a file pointing at the main
        // repo; in the main workspace it is the repo directory itself.
        let repo_dir = if repo_pointer.is_dir() {
            repo_pointer
        } else {
            let target = fs::read_to_string(&repo_pointer)
                .with_context(|| format!("Reading {}", repo_pointer.display()))?;
            let target = target.trim();
            // Relative to `.jj/`, per jj's layout.
            canonicalize_relative(&jj_dir, target)
                .with_context(|| format!("Resolving repo pointer {target:?}"))?
        };

        let git_target_file = repo_dir.join("store").join("git_target");
        if !git_target_file.exists() {
            bail!(
                "This repo is not backed by git (no {}), so its file tree cannot be extracted",
                git_target_file.display()
            );
        }
        let git_target = fs::read_to_string(&git_target_file)
            .with_context(|| format!("Reading {}", git_target_file.display()))?;
        let git_target = git_target.trim();

        // Relative to `<repo>/store/`.
        canonicalize_relative(&repo_dir.join("store"), git_target)
            .with_context(|| format!("Resolving git target {git_target:?}"))
    }

    /// Extract the complete file tree of `head` into a fresh temporary
    /// directory and return it. The directory is deleted when the returned
    /// handle is dropped.
    ///
    /// Uses `git archive <commit> | tar -x`, which reproduces file modes
    /// (including the executable bit) and handles binary files, so the result
    /// is a faithful copy of the revision rather than a text-only
    /// approximation.
    #[instrument(level = "trace", skip(self))]
    pub fn extract_revision_tree(&self, head: &Head) -> Result<tempfile::TempDir> {
        let git_dir = self.resolve_git_dir()?;
        let temp_dir = Builder::new()
            .prefix(&format!("jjscope-{}-", head.change_id.as_str()))
            .tempdir()
            .context("Creating a temporary directory for the revision tree")?;

        let mut archive = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .arg("archive")
            .arg("--format=tar")
            .arg(head.commit_id.as_str())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Running git archive")?;

        // Hand git's stdout pipe straight to tar, so the kernel streams the
        // archive between them: no copy loop to deadlock or break, and a large
        // tree is never held in memory.
        let archive_stdout = archive.stdout.take().expect("git archive stdout was piped");
        let extract = Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(temp_dir.path())
            .stdin(Stdio::from(archive_stdout))
            .stderr(Stdio::piped())
            .spawn()
            .context("Running tar to extract the revision tree")?;

        let tar_output = extract
            .wait_with_output()
            .context("Waiting for tar to finish")?;
        // Check git too: a revision git cannot archive (e.g. one with
        // conflicts) makes git fail while tar happily extracts nothing, so
        // without this the user would get a silently empty tree.
        let archive_output = archive
            .wait_with_output()
            .context("Waiting for git archive to finish")?;

        if !archive_output.status.success() {
            bail!(
                "Failed reading the revision's file tree: {}",
                String::from_utf8_lossy(&archive_output.stderr).trim()
            );
        }
        if !tar_output.status.success() {
            bail!(
                "Failed extracting the revision tree: {}",
                String::from_utf8_lossy(&tar_output.stderr).trim()
            );
        }

        Ok(temp_dir)
    }

    /// Build the command that opens `head`'s whole file tree in the user's
    /// editor, materialized into a temp directory that is removed when the
    /// editor exits.
    ///
    /// The editor is launched *inside* the extracted tree (see
    /// [EditorCommand::working_dir]) so file pickers, `:grep`, and relative
    /// paths all stay within the revision instead of leaking into the live
    /// working copy. The tree is opened read-only where the editor supports it
    /// (`-R` for the vi family): edits would be silently discarded with the
    /// temp directory, so inviting them would be a trap.
    #[instrument(level = "trace", skip(self))]
    pub fn open_revision_tree_command(&self, head: &Head) -> Result<EditorCommand> {
        let temp_dir = self.extract_revision_tree(head)?;

        let mut argv = self.editor_argv();
        let editor_name = argv.first().cloned().unwrap_or_default();
        if crate::commander::files::is_read_only_capable(&editor_name) {
            argv.push("-R".to_owned());
        }
        // Open the tree's root. Editors given a directory show a file browser
        // (netrw/oil for vim, the folder view for VS Code), which is the
        // "browse the repo at this revision" experience.
        argv.push(temp_dir.path().to_string_lossy().into_owned());

        Ok(EditorCommand {
            argv,
            name: format!("Browse tree @ {}", head.change_id),
            working_dir: Some(temp_dir.path().to_owned()),
            cleanup: Some(EditorCleanup::Dir(temp_dir)),
        })
    }
}

/// Join `relative` onto `base` and canonicalize, so the `../..`-style paths
/// jj stores in its pointer files resolve to real absolute locations.
fn canonicalize_relative(base: &Path, relative: &str) -> Result<PathBuf> {
    let joined = base.join(relative);
    fs::canonicalize(&joined).with_context(|| format!("Canonicalizing {}", joined.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn resolve_git_dir_finds_the_store() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let git_dir = test_repo.commander.resolve_git_dir()?;
        // TestRepo colocates, so the store is the `.git` beside the working copy.
        assert!(git_dir.ends_with(".git"), "unexpected git dir: {git_dir:?}");
        assert!(git_dir.is_dir());
        Ok(())
    }

    #[test]
    fn extract_revision_tree_reproduces_the_revision() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let dir = test_repo.directory.path();

        fs::write(dir.join("README"), b"first version\n")?;
        fs::create_dir_all(dir.join("src"))?;
        fs::write(dir.join("src/main.rs"), b"fn main() {}\n")?;
        let first = test_repo.commander.get_current_head()?;

        // Move on, changing and adding files, so the extracted tree must come
        // from the commit rather than the current working copy.
        test_repo.commander.jj(["new"]).run_void()?;
        fs::write(dir.join("README"), b"second version\n")?;
        fs::write(dir.join("later.txt"), b"only in the newer commit\n")?;

        let tree = test_repo.commander.extract_revision_tree(&first)?;

        // The older revision's content, including a nested path.
        assert_eq!(
            fs::read_to_string(tree.path().join("README"))?,
            "first version\n"
        );
        assert_eq!(
            fs::read_to_string(tree.path().join("src/main.rs"))?,
            "fn main() {}\n"
        );
        // A file added after `first` must not be present.
        assert!(!tree.path().join("later.txt").exists());

        Ok(())
    }

    #[test]
    fn extract_revision_tree_preserves_executable_bit() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let dir = test_repo.directory.path();

        let script = dir.join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hi\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        }
        let head = test_repo.commander.get_current_head()?;

        let tree = test_repo.commander.extract_revision_tree(&head)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(tree.path().join("run.sh"))?
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "expected executable, got mode {mode:o}");
        }
        Ok(())
    }

    #[test]
    fn extract_revision_tree_handles_trees_larger_than_a_pipe_buffer() -> Result<()> {
        // A tree bigger than the OS pipe buffer (~64 KiB) must stream through
        // to tar. An implementation that copies git's output by hand can
        // deadlock or die with EPIPE here, while small trees pass by fitting
        // entirely in the buffer.
        let test_repo = TestRepo::new()?;
        let dir = test_repo.directory.path();

        let filler = "x".repeat(4096);
        for i in 0..64 {
            fs::write(dir.join(format!("file{i:03}.txt")), &filler)?;
        }
        let head = test_repo.commander.get_current_head()?;

        let tree = test_repo.commander.extract_revision_tree(&head)?;

        for i in 0..64 {
            let path = tree.path().join(format!("file{i:03}.txt"));
            assert!(path.exists(), "missing {path:?}");
            assert_eq!(fs::read_to_string(&path)?.len(), filler.len());
        }
        Ok(())
    }

    #[test]
    fn extract_revision_tree_cleans_up_on_drop() -> Result<()> {
        let test_repo = TestRepo::new()?;
        fs::write(test_repo.directory.path().join("README"), b"content\n")?;
        let head = test_repo.commander.get_current_head()?;

        let path = {
            let tree = test_repo.commander.extract_revision_tree(&head)?;
            let path = tree.path().to_owned();
            assert!(path.exists());
            path
        };
        // Dropping the handle removes the directory, so a browse leaves nothing
        // behind once the editor exits.
        assert!(!path.exists());
        Ok(())
    }
}
