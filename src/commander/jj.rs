/*!
[Commander] member functions related to various simpler jj commands.

The module implementes a number of jj commands.
Surprisingly, this module also contains jj bookmark commands.
These functions are used everywhere (bookmark tab, log tab).
*/
use anyhow::Context;
use anyhow::Result;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::bookmarks::Bookmark;
use crate::commander::ids::CommitId;

impl Commander {
    /// Create a new change after revisions. Maps to `jj new <revision>...`
    #[instrument(level = "trace", skip(self, revisions))]
    pub fn run_new<'a, T: IntoIterator<Item = &'a str>>(&self, revisions: T) -> Result<()> {
        let args = ["new"].into_iter().chain::<T>(revisions);
        self.jj(args).run_void().context("Failed executing jj new")
    }

    /// Create a new change inserted after and/or before revisions.
    /// Maps to `jj new -A <revision>... -B <revision>...`
    #[instrument(level = "trace", skip(self, after, before))]
    pub fn run_new_insert(&self, after: &[CommitId], before: &[CommitId]) -> Result<()> {
        let mut args = vec!["new".to_owned()];
        for commit_id in after {
            args.push("-A".to_owned());
            args.push(commit_id.as_str().to_owned());
        }
        for commit_id in before {
            args.push("-B".to_owned());
            args.push(commit_id.as_str().to_owned());
        }

        self.jj(args).run_void().context("Failed executing jj new")
    }

    /// Move a change so that it's inserted after and/or before revisions.
    /// Maps to `jj rebase -r <revision> -A <revision>... -B <revision>...`
    #[instrument(level = "trace", skip(self, after, before))]
    pub fn run_rebase_insert(
        &self,
        revision: &str,
        after: &[CommitId],
        before: &[CommitId],
    ) -> Result<()> {
        let mut args = vec!["rebase".to_owned(), "-r".to_owned(), revision.to_owned()];
        for commit_id in after {
            args.push("-A".to_owned());
            args.push(commit_id.as_str().to_owned());
        }
        for commit_id in before {
            args.push("-B".to_owned());
            args.push(commit_id.as_str().to_owned());
        }

        self.jj(args)
            .run_void()
            .context("Failed executing jj rebase")
    }

    /// Duplicate a change. Maps to `jj duplicate`
    pub fn run_duplicate(&self, revision: &str) -> Result<()> {
        self.jj(["duplicate", revision])
            .run_void()
            .context("Failed executing jj duplicate")
    }

    /// Edit change. Maps to `jj edit <commit>`
    #[instrument(level = "trace", skip(self))]
    pub fn run_edit(&self, revision: &str, ignore_immutable: bool) -> Result<()> {
        let mut args = vec!["edit", revision];
        if ignore_immutable {
            args.push("--ignore-immutable");
        }

        self.jj(args).run_void().context("Failed executing jj edit")
    }

    /// Abandon change. Maps to `jj abandon <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn run_abandon(&self, commit_ids: &[CommitId]) -> Result<()> {
        let args = ["abandon"]
            .into_iter()
            .chain(commit_ids.iter().map(CommitId::as_str));
        self.jj(args)
            .run_void()
            .context("Failed executing jj abandon")
    }

    /// Describe change. Maps to `jj describe <revision> --stdin`
    ///
    /// The message is passed on stdin rather than via `-m`, since jj would
    /// otherwise mistake a message starting with a dash for a flag.
    #[instrument(level = "trace", skip(self))]
    pub fn run_describe(&self, revision: &str, message: &str) -> Result<()> {
        self.jj(["describe", revision, "--stdin"])
            .stdin(message)
            .run_void()
            .context("Failed executing jj describe")
    }

    /// Rebase changes. Maps to `jj rebase -s <rev> -d <rev>` or similar
    #[instrument(level = "trace", skip(self))]
    pub fn run_rebase(
        &self,
        src_mode: &str,
        src_rev: &str,
        tgt_mode: &str,
        tgt_rev: &str,
    ) -> Result<()> {
        Ok(self
            .jj(["rebase", src_mode, src_rev, tgt_mode, tgt_rev])
            .run_void()?)
    }

    /// Squash changes. Maps to `jj squash -u --into <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn run_squash(&self, revision: &str, ignore_immutable: bool) -> Result<()> {
        let mut args = vec!["squash", "-u", "--into", revision];
        if ignore_immutable {
            args.push("--ignore-immutable");
        }

        self.jj(args)
            .run_void()
            .context("Failed executing jj squash")
    }

    /// Absorb a change's diff into its mutable ancestors. Maps to `jj absorb --from <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn run_absorb(&self, revision: &str) -> Result<()> {
        self.jj(["absorb", "--from", revision])
            .run_void()
            .context("Failed executing jj absorb")
    }

    /// Undo the last operation. Maps to `jj undo`
    #[instrument(level = "trace", skip(self))]
    pub fn run_undo(&self) -> Result<()> {
        self.jj(["undo"])
            .run_void()
            .context("Failed executing jj undo")
    }

    /// Redo the most recently undone operation. Maps to `jj redo`
    #[instrument(level = "trace", skip(self))]
    pub fn run_redo(&self) -> Result<()> {
        self.jj(["redo"])
            .run_void()
            .context("Failed executing jj redo")
    }

    /// Generate a new change id for a revision. Maps to `jj metaedit --update-change-id <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn run_metaedit_update_change_id(
        &self,
        revision: &str,
        ignore_immutable: bool,
    ) -> Result<()> {
        let mut args = vec!["metaedit", "--update-change-id", revision];
        if ignore_immutable {
            args.push("--ignore-immutable");
        }

        self.jj(args)
            .run_void()
            .context("Failed executing jj metaedit --update-change-id")
    }

    /// Create bookmark. Maps to `jj bookmark create <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn create_bookmark(&self, name: &str) -> Result<Bookmark, CommandError> {
        self.jj(["bookmark", "create", name]).run_void()?;
        // jj only creates local bookmarks
        Ok(Bookmark {
            name: name.to_owned(),
            remote: None,
            present: true,
            timestamp: chrono::Utc::now().timestamp(),
        })
    }

    /// Create bookmark pointing to commit. Maps to `jj bookmark create <name> -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn create_bookmark_commit(
        &self,
        name: &str,
        commit_id: &CommitId,
    ) -> Result<Bookmark, CommandError> {
        self.jj(["bookmark", "create", name, "-r", commit_id.as_str()])
            .run_void()?;
        // jj only creates local bookmarks
        Ok(Bookmark {
            name: name.to_owned(),
            remote: None,
            present: true,
            timestamp: chrono::Utc::now().timestamp(),
        })
    }

    /// Set bookmark pointing to commit. Maps to `jj bookmark set <name> -r <revision>`
    #[instrument(level = "trace", skip(self))]
    pub fn set_bookmark_commit(
        &self,
        name: &str,
        commit_id: &CommitId,
    ) -> Result<(), CommandError> {
        // TODO: Maybe don't do --allow-backwards by default?
        self.jj([
            "bookmark",
            "set",
            name,
            "-r",
            commit_id.as_str(),
            "--allow-backwards",
        ])
        .run_void()
    }

    /// Rename bookmark. Maps to `jj bookmark rename <old> <new>`
    #[instrument(level = "trace", skip(self))]
    pub fn rename_bookmark(&self, old: &str, new: &str) -> Result<(), CommandError> {
        self.jj(["bookmark", "rename", old, new]).run_void()
    }

    /// Delete bookmark. Maps to `jj bookmark delete <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn delete_bookmark(&self, name: &str) -> Result<(), CommandError> {
        self.jj(["bookmark", "delete", name]).run_void()
    }

    /// Forget bookmark. Maps to `jj bookmark forget <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn forget_bookmark(&self, name: &str) -> Result<(), CommandError> {
        self.jj(["bookmark", "forget", name]).run_void()
    }

    /// Track bookmark. Maps to `jj bookmark track <bookmark>@<remote>`
    #[instrument(level = "trace", skip(self))]
    pub fn track_bookmark(&self, bookmark: &Bookmark) -> Result<(), CommandError> {
        self.jj(["bookmark", "track", &bookmark.to_string()])
            .run_void()
    }

    /// Untrack bookmark. Maps to `jj bookmark untrack <bookmark>@<remote>`
    #[instrument(level = "trace", skip(self))]
    pub fn untrack_bookmark(&self, bookmark: &Bookmark) -> Result<(), CommandError> {
        self.jj(["bookmark", "untrack", &bookmark.to_string()])
            .run_void()
    }

    /// Git push. Maps to `jj git push`
    ///
    /// When pushing a single revision, bookmarks pointing at it are pushed by name
    /// (`-b`) rather than by revision (`-r`), since `-r` refuses to create brand-new
    /// remote bookmarks (jj prints a warning and exits 0, so the push silently does
    /// nothing). Revisions with no bookmark fall back to `-r <commit_id>`.
    #[instrument(level = "trace", skip(self))]
    pub fn git_push(
        &self,
        all_bookmarks: bool,
        commit_id: &CommitId,
    ) -> Result<String, CommandError> {
        let mut args = vec!["git".to_owned(), "push".to_owned()];
        if all_bookmarks {
            args.push("--all".to_owned());
        } else {
            let bookmarks = self.get_bookmarks_at(commit_id.as_str())?;
            if bookmarks.is_empty() {
                args.push("-r".to_owned());
                args.push(commit_id.as_str().to_owned());
            } else {
                for bookmark in bookmarks {
                    args.push("-b".to_owned());
                    args.push(bookmark.name);
                }
            }
        }

        self.jj(args).color().run()
    }

    /// Git push a single named bookmark. Maps to `jj git push -b <name>`
    #[instrument(level = "trace", skip(self))]
    pub fn git_push_bookmark(&self, name: &str) -> Result<String, CommandError> {
        self.jj(["git", "push", "-b", name]).color().run()
    }

    /// Git fetch. Maps to `jj git fetch`
    #[instrument(level = "trace", skip(self))]
    pub fn git_fetch(&self, all_remotes: bool) -> Result<String, CommandError> {
        let mut args = vec!["git", "fetch"];
        if all_remotes {
            args.push("--all-remotes");
        }

        self.jj(args).color().run()
    }
}

#[cfg(test)]
mod tests {
    use core::slice;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn run_new() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([head.commit_id.as_str()])?;
        assert_ne!(head, test_repo.commander.get_current_head()?);

        Ok(())
    }

    #[test]
    fn run_edit() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([head.commit_id.as_str()])?;
        assert_ne!(head, test_repo.commander.get_current_head()?);
        test_repo
            .commander
            .run_edit(head.commit_id.as_str(), false)?;
        assert_eq!(head, test_repo.commander.get_current_head()?);

        Ok(())
    }

    #[test]
    fn run_undo_redo() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([head.commit_id.as_str()])?;
        let new_head = test_repo.commander.get_current_head()?;
        assert_ne!(head, new_head);

        test_repo.commander.run_undo()?;
        assert_eq!(head, test_repo.commander.get_current_head()?);

        test_repo.commander.run_redo()?;
        assert_eq!(new_head, test_repo.commander.get_current_head()?);

        Ok(())
    }

    #[test]
    fn run_abandon() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_abandon(slice::from_ref(&head.commit_id))?;
        assert_ne!(head, test_repo.commander.get_current_head()?);

        Ok(())
    }

    #[test]
    fn run_describe() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_describe(head.commit_id.as_str(), "AAA")?;

        let head = test_repo.commander.get_current_head()?.commit_id;
        assert_eq!(test_repo.commander.get_commit_description(&head)?, "AAA");

        Ok(())
    }

    #[test]
    fn run_describe_leading_dash() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // A message starting with a dash must not be mistaken for a flag.
        let head = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_describe(head.commit_id.as_str(), "-AAA")?;

        let head = test_repo.commander.get_current_head()?.commit_id;
        assert_eq!(test_repo.commander.get_commit_description(&head)?, "-AAA");

        Ok(())
    }

    #[test]
    fn create_bookmark() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let bookmark = test_repo.commander.create_bookmark("test")?;
        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;

        assert_eq!(
            bookmarks,
            [Bookmark {
                name: bookmark.name,
                remote: bookmark.remote,
                present: bookmark.present,
                timestamp: bookmarks[0].timestamp,
            }]
        );

        Ok(())
    }

    #[test]
    fn create_bookmark_commit() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // Create new change, since by default `jj bookmark create` uses current change
        let head = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([head.commit_id.as_str()])?;
        assert_ne!(head, test_repo.commander.get_current_head()?);

        let bookmark = test_repo
            .commander
            .create_bookmark_commit("test", &head.commit_id)?;

        let log = test_repo
            .commander
            .jj([
                "log",
                "--limit",
                "1",
                "--no-graph",
                "-T",
                "commit_id",
                "-r",
                &bookmark.name,
            ])
            .run()?;

        assert_eq!(head.commit_id.to_string(), log);

        Ok(())
    }

    #[test]
    fn set_bookmark_commit() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // Create new change, since by default `jj bookmark create` uses current change
        let old_head = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([old_head.commit_id.as_str()])?;
        let new_head = test_repo.commander.get_current_head()?;
        assert_ne!(old_head, new_head);

        let bookmark = test_repo.commander.create_bookmark("test")?;

        let log = test_repo
            .commander
            .jj([
                "log",
                "--limit",
                "1",
                "--no-graph",
                "-T",
                "commit_id",
                "-r",
                &bookmark.name,
            ])
            .run()?;

        assert_eq!(new_head.commit_id.to_string(), log);

        test_repo
            .commander
            .set_bookmark_commit(&bookmark.name, &old_head.commit_id)?;

        let log = test_repo
            .commander
            .jj([
                "log",
                "--limit",
                "1",
                "--no-graph",
                "-T",
                "commit_id",
                "-r",
                &bookmark.name,
            ])
            .run()?;

        assert_eq!(old_head.commit_id.to_string(), log);

        Ok(())
    }

    #[test]
    fn rename_bookmark() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let bookmark = test_repo.commander.create_bookmark("test1")?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(
            bookmarks,
            [Bookmark {
                name: bookmark.name.clone(),
                remote: bookmark.remote,
                present: bookmark.present,
                timestamp: bookmarks[0].timestamp,
            }]
        );

        test_repo
            .commander
            .rename_bookmark(&bookmark.name, "test2")?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(
            bookmarks,
            [Bookmark {
                name: "test2".to_owned(),
                remote: None,
                present: true,
                timestamp: bookmarks[0].timestamp,
            }]
        );

        Ok(())
    }

    #[test]
    fn delete_bookmark() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let bookmark = test_repo.commander.create_bookmark("test")?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(
            bookmarks,
            [Bookmark {
                name: bookmark.name.clone(),
                remote: bookmark.remote,
                present: bookmark.present,
                timestamp: bookmarks[0].timestamp,
            }]
        );

        test_repo.commander.delete_bookmark(&bookmark.name)?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(bookmarks, []);

        Ok(())
    }

    #[test]
    fn forget_bookmark() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let bookmark = test_repo.commander.create_bookmark("test")?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(
            bookmarks,
            [Bookmark {
                name: bookmark.name.clone(),
                remote: bookmark.remote,
                present: bookmark.present,
                timestamp: bookmarks[0].timestamp,
            }]
        );

        test_repo.commander.forget_bookmark(&bookmark.name)?;

        let bookmarks = test_repo.commander.get_bookmarks_list(false)?;
        assert_eq!(bookmarks, []);

        Ok(())
    }

    #[test]
    fn git_push_new_bookmark() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let remote_dir = tempfile::TempDir::with_prefix("jjscope-remote")?;

        std::process::Command::new("git")
            .args(["init", "--bare", "."])
            .current_dir(remote_dir.path())
            .output()?;
        test_repo
            .commander
            .jj([
                "git",
                "remote",
                "add",
                "origin",
                &remote_dir.path().to_string_lossy(),
            ])
            .run_void()?;

        let head = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_describe(head.commit_id.as_str(), "test commit")?;
        test_repo.commander.create_bookmark("new-bookmark")?;
        // Re-fetch: `run_describe` rewrote the commit, so `head` is now stale.
        let head = test_repo.commander.get_current_head()?;

        // A brand-new, never-tracked bookmark must actually be pushed (not silently
        // no-op'd, which is what `jj git push -r <commit>` alone does).
        test_repo.commander.git_push(false, &head.commit_id)?;

        let remote_bookmarks = test_repo
            .commander
            .jj([
                "log",
                "-r",
                "new-bookmark@origin",
                "--no-graph",
                "-T",
                "commit_id",
            ])
            .run()?;
        assert_eq!(remote_bookmarks.trim(), head.commit_id.as_str());

        Ok(())
    }
}
