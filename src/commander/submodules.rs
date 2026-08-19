//! Git submodule support.
//!
//! jj does not track submodules: it prints `ignoring git submodule at ...` when
//! it imports a repo containing one, and thereafter the gitlink is data jj
//! carries but never interprets. Two consequences drive this module:
//!
//! - A gitlink survives jj's snapshots and rewrites, but nothing surfaces it.
//!   `jj file show <path>` reports "not a file", and `--stat` counts the
//!   40-byte object id as `1 +`, which reads as a one-line text edit.
//! - jj's working-copy snapshot skips submodules entirely, so moving a pointer
//!   leaves `jj status` saying the working copy is clean while `git status`
//!   reports `M <path>`. That is uncommitted state the UI actively denies.
//!
//! Everything here is read-only and goes through `git`, since jj offers no
//! access to any of it. Writing to a submodule would create state jj cannot
//! track, so this module deliberately only reads.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use tracing::instrument;

use crate::commander::Commander;
use crate::commander::ids::CommitId;

/// A submodule's recorded pointer in some revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmodulePointer {
    /// Path of the submodule within the repo, e.g. `vendor/inner`.
    pub path: String,
    /// The commit id the gitlink points at.
    pub commit: String,
}

/// How a submodule's pointer differs between two revisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmoduleChange {
    pub path: String,
    /// Pointer in the parent revision; `None` when the submodule was added.
    pub from: Option<String>,
    /// Pointer in this revision; `None` when the submodule was removed.
    pub to: Option<String>,
    /// One-line summaries of the commits between `from` and `to`, newest
    /// first. Empty when the range cannot be resolved — the submodule is not
    /// checked out, or the objects have not been fetched — which is common
    /// enough that it is a display state, not an error.
    pub commits: Vec<String>,
    /// True when the pointers moved backwards (`to` is an ancestor of `from`).
    pub reversed: bool,
}

/// A submodule whose checked-out commit differs from what the revision records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirtySubmodule {
    pub path: String,
    /// What the revision's gitlink says.
    pub recorded: String,
    /// What is actually checked out, or `None` if the submodule directory has
    /// no resolvable HEAD (not initialized).
    pub actual: Option<String>,
}

impl Commander {
    /// Read the gitlinks recorded in `commit`, i.e. every submodule and the
    /// commit id it points at.
    ///
    /// Maps to `git ls-tree -r <commit>`, filtered to entries of type `commit`
    /// — git's representation of a gitlink. Returns an empty list for a repo
    /// with no submodules, which is the overwhelmingly common case.
    #[instrument(level = "trace", skip(self))]
    pub fn get_submodule_pointers(&self, commit: &CommitId) -> Result<Vec<SubmodulePointer>> {
        let git_dir = self.resolve_git_dir()?;
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args(["ls-tree", "-r", commit.as_str()])
            .output()
            .context("Running git ls-tree to read submodule pointers")?;

        if !output.status.success() {
            // A revision git cannot read is not an error worth surfacing: the
            // caller renders a panel, and no submodules is the right answer.
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_ls_tree_gitlinks(&stdout))
    }

    /// The submodule paths declared in `.gitmodules` at the repo root.
    ///
    /// Read from the working copy rather than a revision: this answers "what
    /// might be checked out right now", which is what the dirty check needs.
    /// An absent or unreadable `.gitmodules` yields an empty list.
    pub fn get_declared_submodule_paths(&self) -> Vec<String> {
        let gitmodules = Path::new(&self.env.root).join(".gitmodules");
        let Ok(contents) = std::fs::read_to_string(&gitmodules) else {
            return Vec::new();
        };
        parse_gitmodules_paths(&contents)
    }

    /// Whether this repo has any submodules at all, by the cheapest check
    /// available: the presence of `.gitmodules` in the working copy.
    ///
    /// Callers use this to skip submodule work entirely in the common case,
    /// so it must stay a filesystem stat and never shell out.
    pub fn has_submodules(&self) -> bool {
        Path::new(&self.env.root).join(".gitmodules").exists()
    }

    /// Compare the gitlinks in `commit` against its parent, describing what
    /// each submodule pointer did.
    ///
    /// `parent` is the revision to diff against; pass the first parent. For a
    /// root commit, pass `None` and every submodule reads as newly added.
    #[instrument(level = "trace", skip(self))]
    pub fn get_submodule_changes(
        &self,
        commit: &CommitId,
        parent: Option<&CommitId>,
    ) -> Result<Vec<SubmoduleChange>> {
        let current = self.get_submodule_pointers(commit)?;
        let previous = match parent {
            Some(parent) => self.get_submodule_pointers(parent)?,
            None => Vec::new(),
        };

        let mut changes = Vec::new();
        for pointer in &current {
            let from = previous
                .iter()
                .find(|p| p.path == pointer.path)
                .map(|p| p.commit.clone());
            if from.as_deref() == Some(pointer.commit.as_str()) {
                continue;
            }
            let (commits, reversed) = match &from {
                Some(from) => self.describe_submodule_range(&pointer.path, from, &pointer.commit),
                None => (Vec::new(), false),
            };
            changes.push(SubmoduleChange {
                path: pointer.path.clone(),
                from,
                to: Some(pointer.commit.clone()),
                commits,
                reversed,
            });
        }

        // Submodules present in the parent but gone here were removed.
        for pointer in &previous {
            if !current.iter().any(|p| p.path == pointer.path) {
                changes.push(SubmoduleChange {
                    path: pointer.path.clone(),
                    from: Some(pointer.commit.clone()),
                    to: None,
                    commits: Vec::new(),
                    reversed: false,
                });
            }
        }

        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    /// Summarize the commits between two submodule pointers, by running
    /// `git log` inside the submodule's own checkout.
    ///
    /// Returns the summaries and whether the move was backwards. Any failure —
    /// the submodule is not initialized, or the objects were never fetched —
    /// yields an empty list rather than an error: the pointers alone are still
    /// worth showing, and this is the normal state for a submodule the user has
    /// not checked out.
    fn describe_submodule_range(&self, path: &str, from: &str, to: &str) -> (Vec<String>, bool) {
        let sub_dir = Path::new(&self.env.root).join(path);
        // Require a real checkout: in an empty submodule directory git would
        // resolve against the *outer* repo instead (see [Self::submodule_head]).
        // The range would almost certainly come back empty there anyway, since
        // the outer repo lacks the submodule's objects, but an id collision
        // would otherwise print commits from the wrong repository.
        if !sub_dir.is_dir() || self.submodule_head(path).is_none() {
            return (Vec::new(), false);
        }

        let forward = self.submodule_log(&sub_dir, from, to);
        if !forward.is_empty() {
            return (forward, false);
        }
        // Nothing in `from..to` can mean the pointer moved backwards, so try
        // the other direction before concluding the range is unresolvable.
        let backward = self.submodule_log(&sub_dir, to, from);
        let reversed = !backward.is_empty();
        (backward, reversed)
    }

    /// `git log --oneline <from>..<to>` inside a submodule checkout.
    fn submodule_log(&self, sub_dir: &PathBuf, from: &str, to: &str) -> Vec<String> {
        const MAX_COMMITS: usize = 10;

        let Ok(output) = Command::new("git")
            .current_dir(sub_dir)
            .args(["log", "--no-color", "--oneline", "--no-decorate"])
            // One more than shown, so the caller can tell "exactly ten" from
            // "ten and more".
            .arg(format!("--max-count={}", MAX_COMMITS + 1))
            .arg(format!("{from}..{to}"))
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// Submodules whose checked-out commit does not match what `commit`
    /// records — uncommitted pointer moves that jj's own status cannot see.
    ///
    /// Only considers submodules declared in `.gitmodules`, so a stale gitlink
    /// for a submodule that has since been removed does not raise a warning.
    #[instrument(level = "trace", skip(self))]
    pub fn get_dirty_submodules(&self, commit: &CommitId) -> Result<Vec<DirtySubmodule>> {
        if !self.has_submodules() {
            return Ok(Vec::new());
        }

        let declared = self.get_declared_submodule_paths();
        if declared.is_empty() {
            return Ok(Vec::new());
        }
        let recorded = self.get_submodule_pointers(commit)?;

        let mut dirty = Vec::new();
        for pointer in recorded {
            if !declared.contains(&pointer.path) {
                continue;
            }
            let actual = self.submodule_head(&pointer.path);
            // An uninitialized submodule (no HEAD) is the normal state after a
            // fresh clone, not a pointer the user moved, so it is not dirty.
            if let Some(actual) = actual
                && actual != pointer.commit
            {
                dirty.push(DirtySubmodule {
                    path: pointer.path.clone(),
                    recorded: pointer.commit.clone(),
                    actual: Some(actual),
                });
            }
        }
        Ok(dirty)
    }

    /// The commit currently checked out in a submodule, if it has one.
    ///
    /// Returns `None` for an uninitialized submodule — an empty directory, or
    /// one with no git repo in it. That case must not be mistaken for a moved
    /// pointer: without a boundary, `git rev-parse HEAD` in an empty submodule
    /// directory walks *up* and answers with the outer repo's HEAD, which never
    /// matches the gitlink and would report every uninitialized submodule as
    /// dirty. `--no-flags` style containment via `GIT_CEILING_DIRECTORIES` is
    /// unreliable for this, so the repo root is required to be the submodule
    /// directory itself.
    fn submodule_head(&self, path: &str) -> Option<String> {
        let sub_dir = Path::new(&self.env.root).join(path);
        if !sub_dir.is_dir() {
            return None;
        }

        // Refuse to answer unless this directory is itself the top of a git
        // repo (or a gitlink file pointing into the outer repo's modules dir).
        let toplevel = Command::new("git")
            .current_dir(&sub_dir)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()?;
        if !toplevel.status.success() {
            return None;
        }
        let toplevel = String::from_utf8_lossy(&toplevel.stdout).trim().to_owned();
        // Compare canonicalized, since the toplevel comes back fully resolved
        // while `sub_dir` may contain symlinks or `..` components.
        let same_repo = std::fs::canonicalize(&sub_dir)
            .ok()
            .zip(std::fs::canonicalize(&toplevel).ok())
            .is_some_and(|(a, b)| a == b);
        if !same_repo {
            return None;
        }

        let output = Command::new("git")
            .current_dir(&sub_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!head.is_empty()).then_some(head)
    }
}

/// Path prefix jj gives each side of a conflict when it materializes a
/// conflicted revision as a git tree.
///
/// A conflicted commit is stored with the whole tree repeated once per side —
/// `.jjconflict-base-0/`, `.jjconflict-side-0/`, `.jjconflict-side-1/`, ... —
/// *alongside* the real paths. Every submodule therefore appears several extra
/// times under these prefixes, and since the parent revision has no such paths
/// each copy reads as a newly added submodule.
const CONFLICT_SIDE_PREFIX: &str = ".jjconflict-";

/// Pull the gitlink entries out of `git ls-tree -r` output.
///
/// Lines look like `<mode> <type> <object>\t<path>`; a gitlink has type
/// `commit` and mode `160000`. The path is tab-separated so that paths
/// containing spaces survive.
///
/// Entries under a [CONFLICT_SIDE_PREFIX] directory are skipped: they are jj's
/// internal per-side copies of the same submodules, not submodules of their
/// own, and the real paths are present in the same tree.
fn parse_ls_tree_gitlinks(stdout: &str) -> Vec<SubmodulePointer> {
    stdout
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut fields = meta.split_whitespace();
            let _mode = fields.next()?;
            if fields.next()? != "commit" {
                return None;
            }
            if path.starts_with(CONFLICT_SIDE_PREFIX) {
                return None;
            }
            let object = fields.next()?;
            Some(SubmodulePointer {
                path: path.to_owned(),
                commit: object.to_owned(),
            })
        })
        .collect()
}

/// Read the `path = ...` entries out of a `.gitmodules` file.
///
/// Hand-parsed rather than shelling out to `git config -f`: this runs on every
/// refresh, and the format is a simple INI whose only field of interest here is
/// `path`.
fn parse_gitmodules_paths(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            // The whole key must be `path`; `pathspec` is a different field.
            if key.trim() != "path" {
                return None;
            }
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ls_tree_picks_only_gitlinks() {
        let stdout = "100644 blob fbe940c242cc389fb74e0b845f8d0058a3bdae7c\t.gitmodules\n\
             100644 blob bf1a1fdefa3c7f4b0180a75a951e9574662a8bc8\tREADME.md\n\
             160000 commit cc993a43f8293788a2af5cb19f6642d8282aac0a\tvendor/inner\n";
        let links = parse_ls_tree_gitlinks(stdout);
        assert_eq!(
            links,
            vec![SubmodulePointer {
                path: "vendor/inner".to_owned(),
                commit: "cc993a43f8293788a2af5cb19f6642d8282aac0a".to_owned(),
            }]
        );
    }

    #[test]
    fn parse_ls_tree_skips_jj_conflict_side_copies() {
        // A conflicted revision is stored as a tree carrying the real paths
        // *plus* one copy per conflict side. Counting the copies reported the
        // same submodules several times over, each as a fresh addition since
        // the parent has no such paths.
        let stdout = "160000 commit a5481aae\t.jjconflict-base-0/ext/aws_sdk_cpp\n\
             160000 commit 6b04c989\t.jjconflict-base-0/ext/osqp\n\
             160000 commit a5481aae\t.jjconflict-side-0/ext/aws_sdk_cpp\n\
             160000 commit 6b04c989\t.jjconflict-side-0/ext/osqp\n\
             160000 commit a5481aae\t.jjconflict-side-1/ext/aws_sdk_cpp\n\
             160000 commit 6b04c989\t.jjconflict-side-1/ext/osqp\n\
             160000 commit a5481aae\text/aws_sdk_cpp\n\
             160000 commit 6b04c989\text/osqp\n";
        let links = parse_ls_tree_gitlinks(stdout);
        assert_eq!(
            links,
            vec![
                SubmodulePointer {
                    path: "ext/aws_sdk_cpp".to_owned(),
                    commit: "a5481aae".to_owned(),
                },
                SubmodulePointer {
                    path: "ext/osqp".to_owned(),
                    commit: "6b04c989".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn parse_ls_tree_keeps_paths_with_spaces() {
        let stdout = "160000 commit abc123\tvendor/my module\n";
        let links = parse_ls_tree_gitlinks(stdout);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "vendor/my module");
    }

    #[test]
    fn parse_ls_tree_of_a_repo_without_submodules_is_empty() {
        let stdout = "100644 blob bf1a1fde\tREADME.md\n";
        assert!(parse_ls_tree_gitlinks(stdout).is_empty());
    }

    #[test]
    fn parse_gitmodules_reads_paths() {
        let contents = "[submodule \"vendor/inner\"]\n\
             \tpath = vendor/inner\n\
             \turl = /tmp/inner\n\
             [submodule \"other\"]\n\
             \tpath = libs/other\n\
             \turl = https://example.invalid/other\n";
        assert_eq!(
            parse_gitmodules_paths(contents),
            vec!["vendor/inner".to_owned(), "libs/other".to_owned()]
        );
    }

    #[test]
    fn parse_gitmodules_ignores_pathspec_like_keys() {
        // `path` must be the whole key: `pathspec = ...` is not a submodule path.
        let contents = "[submodule \"a\"]\n\tpathspec = nope\n\tpath = real/one\n";
        assert_eq!(parse_gitmodules_paths(contents), vec!["real/one".to_owned()]);
    }
}
