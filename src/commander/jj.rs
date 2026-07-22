/*!
[Commander] member functions related to various simpler jj commands.

The module implementes a number of jj commands.
Surprisingly, this module also contains jj bookmark commands.
These functions are used everywhere (bookmark tab, log tab).
*/
use std::collections::HashMap;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use tracing::instrument;

use crate::commander::CommandError;
use crate::commander::Commander;
use crate::commander::bookmarks::Bookmark;
use crate::commander::ids::ChangeId;
use crate::commander::ids::CommitId;
use crate::commander::log::Head;

/// The revisions `jj absorb` rewrote, split by why they were rewritten.
#[derive(Debug, Default)]
pub struct AbsorbOutcome {
    /// Revisions that actually received absorbed hunks.
    pub absorbed: Vec<Head>,
    /// Revisions rewritten only because they descend from an absorbed
    /// revision and were rebased along with it.
    pub rebased: Vec<Head>,
}

/// Parse the commit ID prefix out of `jj new --no-edit`'s
/// `Created new commit <change_id> <commit_id> ...` stderr message.
fn parse_created_commit(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .find_map(|line| line.strip_prefix("Created new commit "))?
        .split_whitespace()
        .nth(1)
}

/// Parse the change ID prefixes out of `jj absorb`'s "Absorbed changes into
/// N revisions:" stderr listing, whose entries look like
/// `  wnzmyvwk b9a3db4b commit-A`. Returns an empty list if the message
/// isn't present (nothing was absorbed, or a future jj changed the wording).
fn parse_absorb_destinations(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .skip_while(|line| !line.starts_with("Absorbed changes into"))
        .skip(1)
        .map_while(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

impl Commander {
    /// Create a new change after revisions, moving `@` into it.
    /// Maps to `jj new <revision>...`
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the UI uses run_new_no_edit; tests use this as a fixture"
        )
    )]
    #[instrument(level = "trace", skip(self, revisions))]
    pub fn run_new<'a, T: IntoIterator<Item = &'a str>>(&self, revisions: T) -> Result<()> {
        let args = ["new"].into_iter().chain::<T>(revisions);
        self.jj(args).run_void().context("Failed executing jj new")
    }

    /// Create a new change after revisions without moving `@`, and return it.
    /// Maps to `jj new --no-edit <revision>...`
    ///
    /// `--no-edit` keeps `@` where it is: moving `@` drastically twists the
    /// printed graph around the working copy, and moving it *off* an empty
    /// undescribed change (e.g. a megamerge working set) would silently
    /// abandon that change. Callers should put the cursor on the returned
    /// change instead; `edit` from there is the compound action.
    #[instrument(level = "trace", skip(self, revisions))]
    pub fn run_new_no_edit<'a, T: IntoIterator<Item = &'a str>>(
        &self,
        revisions: T,
    ) -> Result<Head> {
        let args = ["new", "--no-edit"].into_iter().chain::<T>(revisions);
        let stderr = self
            .jj(args)
            .run_stderr()
            .context("Failed executing jj new")?;
        let commit_prefix = parse_created_commit(&stderr)
            .ok_or_else(|| anyhow!("jj new did not report the created commit"))?;
        self.get_head(commit_prefix)
    }

    /// Create a new change inserted after and/or before revisions, without
    /// moving `@`, and return it.
    /// Maps to `jj new --no-edit -A <revision>... -B <revision>...`
    ///
    /// `--no-edit` keeps `@` where it is: moving `@` would twist the printed
    /// graph around, and moving it *off* an empty undescribed change (e.g. a
    /// megamerge working set) would silently abandon that change. Callers
    /// should put the cursor on the returned change instead, from where the
    /// user can `edit` into it if that's what they want.
    #[instrument(level = "trace", skip(self, after, before))]
    pub fn run_new_insert(&self, after: &[CommitId], before: &[CommitId]) -> Result<Head> {
        let mut args = vec!["new".to_owned(), "--no-edit".to_owned()];
        for commit_id in after {
            args.push("-A".to_owned());
            args.push(commit_id.as_str().to_owned());
        }
        for commit_id in before {
            args.push("-B".to_owned());
            args.push(commit_id.as_str().to_owned());
        }

        let stderr = self
            .jj(args)
            .run_stderr()
            .context("Failed executing jj new")?;
        let commit_prefix = parse_created_commit(&stderr)
            .ok_or_else(|| anyhow!("jj new did not report the created commit"))?;
        self.get_head(commit_prefix)
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

    /// Rebase changes. Maps to `jj rebase -s <rev>... -d <rev>...` or similar
    ///
    /// Multiple sources are all passed with the same `src_mode` flag, and
    /// multiple targets with the same `tgt_mode` flag; with `-d`, multiple
    /// targets rebase the sources onto a merge of the targets.
    #[instrument(level = "trace", skip(self))]
    pub fn run_rebase(
        &self,
        src_mode: &str,
        src_revs: &[CommitId],
        tgt_mode: &str,
        tgt_revs: &[CommitId],
    ) -> Result<()> {
        let mut args = vec!["rebase"];
        for src_rev in src_revs {
            args.push(src_mode);
            args.push(src_rev.as_str());
        }
        for tgt_rev in tgt_revs {
            args.push(tgt_mode);
            args.push(tgt_rev.as_str());
        }

        Ok(self.jj(args).run_void()?)
    }

    /// Squash whole change(s) into another change.
    /// Maps to `jj squash -u --from <revision>... --into <revision>`
    ///
    /// `-u` keeps the destination's description, since jj's default of
    /// combining the descriptions would open an editor.
    #[instrument(level = "trace", skip(self))]
    pub fn run_squash_into(
        &self,
        from: &[CommitId],
        into: &str,
        ignore_immutable: bool,
    ) -> Result<()> {
        let mut args = vec!["squash", "-u"];
        for from_rev in from {
            args.push("--from");
            args.push(from_rev.as_str());
        }
        args.push("--into");
        args.push(into);
        if ignore_immutable {
            args.push("--ignore-immutable");
        }

        self.jj(args)
            .run_void()
            .context("Failed executing jj squash")
    }

    /// Remove redundant parent edges (parents that are also indirect
    /// ancestors through another parent) from the given revisions.
    /// Maps to `jj simplify-parents -r <revision>...`, or `-s` with
    /// `include_descendants`, which also simplifies all their descendants.
    ///
    /// Returns jj's human-readable summary ("Removed N edges from M out of
    /// K commits."), or `None` if there was nothing to simplify.
    #[instrument(level = "trace", skip(self, revisions))]
    pub fn run_simplify_parents(
        &self,
        revisions: &[CommitId],
        include_descendants: bool,
    ) -> Result<Option<String>> {
        let flag = if include_descendants { "-s" } else { "-r" };
        let mut args = vec!["simplify-parents"];
        for revision in revisions {
            args.push(flag);
            args.push(revision.as_str());
        }

        let stderr = self
            .jj(args)
            .run_stderr()
            .context("Failed executing jj simplify-parents")?;
        Ok(stderr
            .lines()
            .find(|line| line.starts_with("Removed "))
            .map(str::to_owned))
    }

    /// Absorb a change's diff into its mutable ancestors. Maps to `jj absorb --from <revision>`
    ///
    /// Returns the rewritten revisions split into the ones that actually
    /// received hunks and the ones that were only rebased along, so callers
    /// can highlight them differently. The destinations come from absorb's
    /// "Absorbed changes into N revisions:" stderr listing; the full
    /// rewritten set is found by snapshotting all mutable revisions
    /// beforehand and comparing each change's commit ID afterwards (this
    /// includes descendants on sibling branches, which rebase propagation
    /// also rewrites). `revision` itself is excluded — it always gets
    /// rewritten, by having its diff carved out.
    #[instrument(level = "trace", skip(self))]
    pub fn run_absorb(&self, revision: &str) -> Result<AbsorbOutcome> {
        let before = self.get_mutable_heads()?;

        let stderr = self
            .jj(["absorb", "--from", revision])
            .run_stderr()
            .context("Failed executing jj absorb")?;
        let destinations = parse_absorb_destinations(&stderr);

        let after: HashMap<ChangeId, Head> = self
            .get_mutable_heads()?
            .into_iter()
            .map(|head| (head.change_id.clone(), head))
            .collect();

        let rewritten: Vec<Head> = before
            .iter()
            .filter(|old_head| old_head.commit_id.as_str() != revision)
            .filter_map(|old_head| match after.get(&old_head.change_id) {
                Some(new_head) if new_head.commit_id != old_head.commit_id => {
                    Some(new_head.clone())
                }
                _ => None,
            })
            .collect();

        // If revisions were rewritten but the destination listing couldn't be
        // parsed (e.g. a future jj changed the wording), don't guess at a
        // split: report everything as absorbed, matching the old behavior.
        if !rewritten.is_empty() && destinations.is_empty() {
            return Ok(AbsorbOutcome {
                absorbed: rewritten,
                rebased: vec![],
            });
        }

        let (absorbed, rebased) = rewritten.into_iter().partition(|head| {
            destinations
                .iter()
                .any(|prefix| head.change_id.as_str().starts_with(prefix.as_str()))
        });
        Ok(AbsorbOutcome { absorbed, rebased })
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
    pub fn git_push(&self, commit_id: &CommitId) -> Result<String, CommandError> {
        let mut args = vec!["git".to_owned(), "push".to_owned()];
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
    use std::fs;

    use super::*;
    use crate::commander::tests::TestRepo;

    #[test]
    fn run_absorb() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let file = test_repo.directory.path().join("f.txt");

        // commit A: parent of commit B. The unchanged middle line keeps the two
        // edited lines in separate diff hunks, which absorb needs to split them
        // to different destinations.
        fs::write(&file, "a\nunchanged\nb\n")?;
        let commit_a = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_describe(commit_a.commit_id.as_str(), "commit A")?;

        // commit B: on top of commit A
        test_repo.commander.run_new(["@"])?;
        fs::write(&file, "a\nunchanged\nb2\n")?;
        let commit_b = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_describe(commit_b.commit_id.as_str(), "commit B")?;

        // Working copy on top of commit B, editing a line each commit introduced
        test_repo.commander.run_new(["@"])?;
        fs::write(&file, "a3\nunchanged\nb3\n")?;
        let working_copy = test_repo.commander.get_current_head()?;

        let outcome = test_repo
            .commander
            .run_absorb(working_copy.commit_id.as_str())?;

        let mut absorbed_change_ids: Vec<_> = outcome
            .absorbed
            .iter()
            .map(|head| head.change_id.as_str())
            .collect();
        absorbed_change_ids.sort();
        let mut expected_change_ids =
            vec![commit_a.change_id.as_str(), commit_b.change_id.as_str()];
        expected_change_ids.sort();
        assert_eq!(absorbed_change_ids, expected_change_ids);

        // Both rewritten commits received hunks, so nothing was rebased-only
        assert!(outcome.rebased.is_empty());

        // Each absorbed head's commit ID should have moved on from the pre-absorb one
        for head in &outcome.absorbed {
            assert!(head.commit_id != commit_a.commit_id && head.commit_id != commit_b.commit_id);
        }

        Ok(())
    }

    #[test]
    fn run_absorb_split_absorbed_from_rebased() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let file_a = test_repo.directory.path().join("a.txt");

        // commit A: introduces a.txt (the only absorb destination).
        // Describe before capturing the head: describing rewrites the
        // commit, which would leave a captured commit ID stale (hidden).
        fs::write(&file_a, "a\n")?;
        test_repo.commander.run_describe("@", "commit A")?;
        let commit_a = test_repo.commander.get_current_head()?;

        // commit B: on top of A, touches only b.txt, so it can't absorb
        // anything but gets rebased when A is rewritten
        test_repo.commander.run_new(["@"])?;
        fs::write(test_repo.directory.path().join("b.txt"), "b\n")?;
        test_repo.commander.run_describe("@", "commit B")?;
        let commit_b = test_repo.commander.get_current_head()?;

        // commit S: sibling branch off A; also rebased when A is rewritten,
        // even though it isn't an ancestor of the working copy
        test_repo.commander.run_new([commit_a.commit_id.as_str()])?;
        fs::write(test_repo.directory.path().join("s.txt"), "s\n")?;
        test_repo.commander.run_describe("@", "commit S")?;
        let commit_s = test_repo.commander.get_current_head()?;

        // Working copy on top of B, editing the line A introduced
        test_repo.commander.run_new([commit_b.commit_id.as_str()])?;
        fs::write(&file_a, "a2\n")?;
        let working_copy = test_repo.commander.get_current_head()?;

        let outcome = test_repo
            .commander
            .run_absorb(working_copy.commit_id.as_str())?;

        let absorbed_change_ids: Vec<_> = outcome
            .absorbed
            .iter()
            .map(|head| head.change_id.as_str())
            .collect();
        assert_eq!(absorbed_change_ids, vec![commit_a.change_id.as_str()]);

        let mut rebased_change_ids: Vec<_> = outcome
            .rebased
            .iter()
            .map(|head| head.change_id.as_str())
            .collect();
        rebased_change_ids.sort();
        let mut expected_rebased = vec![commit_b.change_id.as_str(), commit_s.change_id.as_str()];
        expected_rebased.sort();
        assert_eq!(rebased_change_ids, expected_rebased);

        Ok(())
    }

    #[test]
    fn parse_absorb_destinations_listing() {
        let stderr = "\
Absorbed changes into 2 revisions:
  qpvuntsm 90e5407c commit A
  kntqzsqt 4c30963b commit B
Rebased 1 descendant commits.
Working copy  (@) now at: oymkkrtq 8e05ce0c (empty) wc
";
        assert_eq!(
            parse_absorb_destinations(stderr),
            vec!["qpvuntsm".to_owned(), "kntqzsqt".to_owned()]
        );
    }

    #[test]
    fn parse_absorb_destinations_nothing() {
        assert_eq!(
            parse_absorb_destinations("Nothing changed.\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn run_absorb_nothing_to_absorb() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let head = test_repo.commander.get_current_head()?;
        let outcome = test_repo.commander.run_absorb(head.commit_id.as_str())?;

        assert!(outcome.absorbed.is_empty());
        assert!(outcome.rebased.is_empty());

        Ok(())
    }

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
    fn run_squash_into_parent() -> Result<()> {
        let test_repo = TestRepo::new()?;
        let file = test_repo.directory.path().join("f.txt");

        let parent = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new(["@"])?;
        fs::write(&file, "child content")?;
        let child = test_repo.commander.get_current_head()?;

        test_repo.commander.run_squash_into(
            std::slice::from_ref(&child.commit_id),
            parent.commit_id.as_str(),
            false,
        )?;

        // The parent picked up the child's content...
        let parent = test_repo
            .commander
            .get_change_head(&parent.change_id)?
            .expect("squash destination should still exist");
        let content = test_repo
            .commander
            .jj(["file", "show", "-r", parent.commit_id.as_str(), "f.txt"])
            .run()?;
        assert_eq!(content, "child content");

        // ...and the now-empty child was abandoned
        assert_eq!(test_repo.commander.get_change_head(&child.change_id)?, None);

        Ok(())
    }

    #[test]
    fn run_new_no_edit_keeps_working_copy() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        let created = test_repo
            .commander
            .run_new_no_edit([base.commit_id.as_str()])?;

        // `@` did not move; the returned change is a new child of base
        assert_eq!(test_repo.commander.get_current_head()?, base);
        assert_ne!(created.change_id, base.change_id);
        let parent = test_repo.commander.get_commit_parent(&created.commit_id)?;
        assert_eq!(parent.change_id, base.change_id);

        Ok(())
    }

    #[test]
    fn run_new_insert_keeps_working_copy() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let child = test_repo.commander.get_current_head()?;

        let inserted = test_repo.commander.run_new_insert(
            std::slice::from_ref(&base.commit_id),
            std::slice::from_ref(&child.commit_id),
        )?;

        // `@` did not move to the inserted change
        let head = test_repo.commander.get_current_head()?;
        assert_eq!(head.change_id, child.change_id);

        // The returned change is the inserted one, between base and child
        let parent = test_repo.commander.get_commit_parent(&head.commit_id)?;
        assert_eq!(parent, inserted);
        let grandparent = test_repo.commander.get_commit_parent(&inserted.commit_id)?;
        assert_eq!(grandparent.change_id, base.change_id);

        Ok(())
    }

    #[test]
    fn run_simplify_parents() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // Merge of siblings A and B, then B is moved (alone) onto A: the
        // merge is reparented to (A, base), where base is redundant since
        // it's an ancestor of A.
        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let commit_a = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let commit_b = test_repo.commander.get_current_head()?;
        test_repo
            .commander
            .run_new([commit_a.commit_id.as_str(), commit_b.commit_id.as_str()])?;
        let merge = test_repo.commander.get_current_head()?;
        test_repo.commander.run_rebase(
            "-r",
            std::slice::from_ref(&commit_b.commit_id),
            "-d",
            std::slice::from_ref(&commit_a.commit_id),
        )?;

        let merge = test_repo
            .commander
            .get_change_head(&merge.change_id)?
            .expect("merge should still exist");
        let summary = test_repo
            .commander
            .run_simplify_parents(std::slice::from_ref(&merge.commit_id), false)?;
        assert!(summary.is_some_and(|summary| summary.starts_with("Removed ")));

        // The redundant base edge is gone; A is the only parent
        let merge = test_repo
            .commander
            .get_change_head(&merge.change_id)?
            .expect("merge should still exist");
        let parents = test_repo
            .commander
            .jj([
                "log",
                "--no-graph",
                "-T",
                r#"change_id ++ "\n""#,
                "-r",
                &format!("{}-", merge.commit_id),
            ])
            .run()?;
        assert_eq!(parents.trim(), commit_a.change_id.as_str());

        // Running it again finds nothing to do
        let summary = test_repo
            .commander
            .run_simplify_parents(std::slice::from_ref(&merge.commit_id), false)?;
        assert_eq!(summary, None);

        Ok(())
    }

    #[test]
    fn run_squash_into_multiple_sources() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        fs::write(test_repo.directory.path().join("a.txt"), "a")?;
        let source_a = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        fs::write(test_repo.directory.path().join("b.txt"), "b")?;
        let source_b = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let dest = test_repo.commander.get_current_head()?;

        test_repo.commander.run_squash_into(
            &[source_a.commit_id.clone(), source_b.commit_id.clone()],
            dest.commit_id.as_str(),
            false,
        )?;

        // The destination received both sources' files
        let dest = test_repo
            .commander
            .get_change_head(&dest.change_id)?
            .expect("squash destination should still exist");
        let files = test_repo
            .commander
            .jj(["file", "list", "-r", dest.commit_id.as_str()])
            .run()?;
        assert!(files.contains("a.txt") && files.contains("b.txt"));

        Ok(())
    }

    #[test]
    fn run_rebase_branch_mode() -> Result<()> {
        let test_repo = TestRepo::new()?;

        // Branch base -> A -> B, plus a sibling destination D off base
        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let commit_a = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new(["@"])?;
        let commit_b = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let dest = test_repo.commander.get_current_head()?;

        // Rebasing -b with the branch *tip* moves the whole branch: A (the
        // branch root relative to the destination) gets the new parent
        test_repo.commander.run_rebase(
            "-b",
            std::slice::from_ref(&commit_b.commit_id),
            "-d",
            std::slice::from_ref(&dest.commit_id),
        )?;

        let commit_a = test_repo
            .commander
            .get_change_head(&commit_a.change_id)?
            .expect("commit A should still exist");
        let parent = test_repo.commander.get_commit_parent(&commit_a.commit_id)?;
        assert_eq!(parent.change_id, dest.change_id);

        Ok(())
    }

    #[test]
    fn run_rebase_multiple_destinations() -> Result<()> {
        let test_repo = TestRepo::new()?;

        let base = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let head_a = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let head_b = test_repo.commander.get_current_head()?;
        test_repo.commander.run_new([base.commit_id.as_str()])?;
        let moved = test_repo.commander.get_current_head()?;

        test_repo.commander.run_rebase(
            "-r",
            std::slice::from_ref(&moved.commit_id),
            "-d",
            &[head_a.commit_id.clone(), head_b.commit_id.clone()],
        )?;

        // The rebased change became a merge of both destinations
        let parents = test_repo
            .commander
            .jj([
                "log",
                "--no-graph",
                "-T",
                r#"commit_id ++ "\n""#,
                "-r",
                &format!("{}-", moved.change_id.as_str()),
            ])
            .run()?;
        let mut parents: Vec<&str> = parents.lines().collect();
        parents.sort();
        let mut expected = vec![head_a.commit_id.as_str(), head_b.commit_id.as_str()];
        expected.sort();
        assert_eq!(parents, expected);

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
        test_repo.commander.git_push(&head.commit_id)?;

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
