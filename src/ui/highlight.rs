/*! The log tab's highlight set: mark a subset of the revisions on show.

Answering "which revisions touch this file?" by *filtering* the log
(`jj log -r 'files(...)'`) throws away the graph, and with it where those
revisions sit relative to `@`, trunk, and each other. So jjscope marks instead:
the log keeps its revset, graph, and node glyphs, and each selected revision gets
a gutter bar (see [gutter_spans][crate::ui::panel::gutter_spans]).

What gets marked is any revset, edited alongside the log's own revset -- so
"revisions touching a path" is just `files(...)`, and anything else jj can select
(`description(...)`, `author(...)`, `conflicts()`, a bookmark's ancestors) works
the same way. The file filter is a convenience layer that writes the `files(...)`
expression for you.

This module holds the state behind the marking. It mirrors
[SearchState][crate::ui::search::SearchState] -- session-only pane state that
restyles rendered lines -- but lives apart from it because it needs commander
types.
*/

use std::collections::HashSet;

use crate::commander::ids::ChangeId;
use crate::commander::log::Head;

/// Key that widens the log's revset to the highlight expression, and how it is
/// spelled to the user. Offered when a highlight matches revisions that the log's
/// own revset excludes, so the gutter would otherwise be inexplicably empty.
///
/// Ctrl+w rather than a bare letter, since the log tab's single-key bindings are
/// dense. Lives here so the panel title and the status message cannot drift apart
/// on what they tell the user to press.
pub const WIDEN_KEY_LABEL: &str = "ctrl+w";

/// The log's highlight set: a revset and the change IDs it selects.
///
/// The set is keyed by **change** ID, not commit ID. Every operation that
/// rewrites a revision (squash, rebase, absorb, describe, diffedit) gives it a
/// new commit ID while preserving its change ID, so a commit-ID-keyed set would
/// go stale on the very next keypress -- and because rebase rewrites descendants
/// too, one keystroke could drop the mark from a whole run of revisions that
/// still match. The same reasoning is why
/// [LogPanel::absorbed_heads][crate::ui::panel::LogPanel] is change-ID-keyed.
///
/// Change IDs fix *identity* staleness but not *content* staleness: a squash
/// moves hunks between revisions, so `files(...)` can select a different set
/// afterwards. That is handled by refetching on every log refresh rather than by
/// anything here.
#[derive(Debug, Default, Clone)]
pub struct HighlightState {
    /// The revset being highlighted, as the user typed it (or as a convenience
    /// layer built it). `None` means nothing is highlighted.
    revset: Option<String>,
    /// A short label for the panel title, when the revset was built for the user
    /// rather than typed by them -- e.g. `src/app.rs` for a file filter, whose
    /// full revset reads `files("src/app.rs")`. `None` shows the revset itself.
    label: Option<String>,
    /// Change IDs of the revisions [Self::revset] selects. An empty set is
    /// meaningful -- a valid revset selecting nothing -- and distinct from
    /// `revset == None`.
    matching: HashSet<ChangeId>,
    /// Whether the last fetch failed. Kept separate from an empty
    /// [Self::matching] so "jj rejected this revset" is never reported as
    /// "nothing matches".
    errored: bool,
}

impl HighlightState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything is currently highlighted.
    pub fn is_active(&self) -> bool {
        self.revset.is_some()
    }

    /// The active revset, or `None`.
    pub fn revset(&self) -> Option<&str> {
        self.revset.as_deref()
    }

    /// What to show in the panel title: the friendly label if the revset was
    /// generated, else the revset itself.
    pub fn display(&self) -> Option<&str> {
        self.label.as_deref().or(self.revset())
    }

    /// Whether the last fetch for the active revset failed.
    pub fn errored(&self) -> bool {
        self.errored
    }

    /// Whether `head`'s revision is in the highlight set. Always false when
    /// nothing is highlighted, so callers can skip an `is_active` check.
    pub fn matches(&self, head: &Head) -> bool {
        self.matching.contains(&head.change_id)
    }

    /// How many revisions in the repo the revset selects. This is a repo-wide
    /// count, not a count of what is visible in the log.
    pub fn matching_count(&self) -> usize {
        self.matching.len()
    }

    /// Activate `revset` with a freshly fetched set of matching change IDs.
    /// `label` overrides the revset in the panel title when set.
    pub fn set(&mut self, revset: String, label: Option<String>, matching: HashSet<ChangeId>) {
        self.revset = Some(revset);
        self.label = label;
        self.matching = matching;
        self.errored = false;
    }

    /// Record that fetching `revset` failed. Nothing is marked, but the revset is
    /// kept so the title still shows what was asked for.
    pub fn set_errored(&mut self, revset: String, label: Option<String>) {
        self.revset = Some(revset);
        self.label = label;
        self.matching.clear();
        self.errored = true;
    }

    /// Clear the highlight.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What applying a highlight revset did, so the caller can pick the right
/// feedback.
///
/// The distinctions matter: an empty gutter means something different when 42
/// revisions match but none are in the log's revset ([Self::Applied] with
/// `visible == 0`, so widen the revset) than when nothing matches at all
/// ([Self::NoneMatching], so check the expression) or when jj rejected it
/// ([Self::Failed]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HighlightOutcome {
    /// The highlight was cleared (empty input, or toggled off).
    Cleared,
    /// Revisions match. `matching` counts them repo-wide, `visible` counts how
    /// many are in the log as currently shown.
    Applied { matching: usize, visible: usize },
    /// The revset is valid but selects no revision in the repo.
    NoneMatching,
    /// jj rejected the revset. Carries its diagnostic.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commander::ids::CommitId;

    fn head(change_id: &str, commit_id: &str) -> Head {
        Head {
            change_id: ChangeId(change_id.to_owned()),
            commit_id: CommitId(commit_id.to_owned()),
            divergent: false,
            immutable: false,
        }
    }

    fn matching(change_ids: &[&str]) -> HashSet<ChangeId> {
        change_ids
            .iter()
            .map(|id| ChangeId((*id).to_owned()))
            .collect()
    }

    #[test]
    fn is_inactive_by_default() {
        let highlight = HighlightState::new();
        assert!(!highlight.is_active());
        assert_eq!(highlight.revset(), None);
        assert_eq!(highlight.display(), None);
        assert!(!highlight.errored());
        assert_eq!(highlight.matching_count(), 0);
    }

    #[test]
    fn clear_deactivates() {
        let mut highlight = HighlightState::new();
        highlight.set("conflicts()".to_owned(), None, matching(&["kkkk"]));
        assert!(highlight.is_active());

        highlight.clear();
        assert!(!highlight.is_active());
        assert_eq!(highlight.matching_count(), 0);
    }

    #[test]
    fn an_empty_set_stays_active_and_is_not_an_error() {
        // A valid revset selecting nothing: the highlight is on, the gutter is
        // empty, and that is the honest answer -- not a failure.
        let mut highlight = HighlightState::new();
        highlight.set("conflicts()".to_owned(), None, HashSet::new());

        assert!(highlight.is_active());
        assert!(!highlight.errored());
        assert_eq!(highlight.matching_count(), 0);
        assert!(!highlight.matches(&head("kkkk", "aaaa")));
    }

    #[test]
    fn errored_is_distinguishable_from_an_empty_set() {
        let mut highlight = HighlightState::new();
        highlight.set_errored("bogusfn(".to_owned(), None);

        assert!(highlight.is_active());
        assert!(highlight.errored());
        assert_eq!(highlight.revset(), Some("bogusfn("));
        assert_eq!(highlight.matching_count(), 0);
    }

    #[test]
    fn setting_a_fresh_result_clears_a_previous_error() {
        let mut highlight = HighlightState::new();
        highlight.set_errored("bogusfn(".to_owned(), None);
        highlight.set("conflicts()".to_owned(), None, matching(&["kkkk"]));

        assert!(!highlight.errored());
        assert_eq!(highlight.revset(), Some("conflicts()"));
    }

    #[test]
    fn matches_by_change_id_ignoring_commit_id() {
        // The whole point of keying by change id: the same change with a
        // rewritten commit id still matches.
        let mut highlight = HighlightState::new();
        highlight.set("conflicts()".to_owned(), None, matching(&["kkkk"]));

        assert!(highlight.matches(&head("kkkk", "aaaa")));
        assert!(highlight.matches(&head("kkkk", "bbbb")));
        assert!(!highlight.matches(&head("mmmm", "aaaa")));
    }

    #[test]
    fn a_label_stands_in_for_the_revset_in_the_title() {
        // A file filter shows the path, not the files(...) wrapper it generated.
        let mut highlight = HighlightState::new();
        highlight.set(
            r#"files("src/app.rs")"#.to_owned(),
            Some("src/app.rs".to_owned()),
            matching(&["kkkk"]),
        );

        assert_eq!(highlight.display(), Some("src/app.rs"));
        assert_eq!(highlight.revset(), Some(r#"files("src/app.rs")"#));
    }

    #[test]
    fn a_typed_revset_shows_itself_in_the_title() {
        let mut highlight = HighlightState::new();
        highlight.set("conflicts()".to_owned(), None, matching(&["kkkk"]));

        assert_eq!(highlight.display(), Some("conflicts()"));
    }
}
