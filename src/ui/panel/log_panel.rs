/*! The log panel shows the list of changes on the left side of the
log tab. */

use std::collections::HashSet;

use ansi_to_tui::IntoText;
use anyhow::Result;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::MouseEvent;
use ratatui::crossterm::event::MouseEventKind;
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::text::ToText;
use ratatui::widgets::*;

use crate::commander::CommandError;
use crate::commander::ids::ChangeId;
use crate::commander::ids::CommitId;
use crate::commander::log::Head;
use crate::commander::log::LogOutput;
use crate::commander::new_commander;
use crate::env::JjConfig;
use crate::env::get_env;
use crate::keybinds::LogTabEvent;
use crate::keybinds::LogTabKeybinds;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::highlight::HighlightOutcome;
use crate::ui::highlight::HighlightState;
use crate::ui::highlight::WIDEN_KEY_LABEL;
use crate::ui::search::SearchState;
use crate::ui::search::first_match_index_at_or_after;
use crate::ui::search::highlight_matches;
use crate::ui::search::next_match_index;

/**
    A panel that displays the output of jj log.
    This panel is used on the left side of the log tab.
    It shows a selected change, which is expanded
    on the right side of the log tab.

    The log operates with two index:
    - line index (into self.log_output.text)
    - head index (into self.log_output.heads)

    The line index is used for scrolling at the display level.

    The head index is used for scrolling at the user level
    as well as for selecting which lines to highlight.
*/
pub struct LogPanel<'a> {
    /// Output from 'jj log' as provided by command::get_show_log
    log_output: Result<LogOutput, CommandError>,

    /// Output from 'jj log' converted to Ratatui Text
    log_output_text: Text<'a>,

    /// Scroll offset and cursor position
    log_list_state: ListState,

    /// Area were log content was drawn. This excludes the border.
    pub log_rect: Rect,

    /// The revision filter used for the log
    pub log_revset: Option<String>,

    /// Currently selected commit
    pub head: Head,

    /// Currently marked commits
    pub marked_heads: HashSet<CommitId>,

    /// Commits marked as *before*-anchors: the change a command creates or
    /// moves should land below these, i.e. they become its children
    /// (`jj new`/`jj rebase -B`). [Self::marked_heads] are the corresponding
    /// after-anchors, so the two sets together describe a splice.
    ///
    /// Kept separate rather than as a flag on one set because a revision can
    /// legitimately be neither, and because the two render differently.
    pub before_marked_heads: HashSet<CommitId>,

    /// When true, the marked commits are the candidate parent set of a
    /// rebase in progress and render as [NODE_PARENT] instead of
    /// [NODE_MARKED], so toggling a mark visibly adds/removes a future
    /// parent edge.
    pub marks_are_parents: bool,

    /// Changes the most recent `jj absorb` moved hunks into, shown with a
    /// distinct node glyph ([NODE_ABSORBED]) until the next keypress (mirrors
    /// how `App::status_message` clears). Keyed by change ID rather than
    /// commit ID since absorb rewrites the commit ID of every revision it
    /// touches.
    pub absorbed_heads: HashSet<ChangeId>,

    /// Changes the most recent `jj absorb` rewrote only by rebasing them on
    /// top of an absorbed-into revision, shown with [NODE_REBASED]. Cleared
    /// together with [Self::absorbed_heads].
    pub rebased_heads: HashSet<ChangeId>,

    /// When set, shown as the panel title instead of the usual "Log"/"Log for: <revset>" title
    pub title_override: Option<String>,

    /// Active vim-style search. While a query is set, lines whose text
    /// contains it are highlighted, and n/N navigate between matching changes.
    /// Shared with the bookmarks tab via [crate::ui::search].
    search: SearchState,

    /// Active highlight revset. While set, the revisions it selects get a gutter
    /// bar (see [gutter_spans]) in a new leftmost column, leaving the rest of the
    /// log untouched. Session-only, like [Self::search].
    highlight: HighlightState,

    /// Area where panel was drawn. This includes the border.
    panel_rect: Rect,

    /// Configuration of colours
    config: JjConfig,
}

/// Node glyph shown in place of jj's usual node (`@`/`○`/`◆`/...) for a marked commit.
const NODE_MARKED: char = '✓';
/// Node glyph for a marked commit while the marks are a rebase's candidate
/// parent set (see [LogPanel::marks_are_parents]).
const NODE_PARENT: char = '✚';
/// Node glyph for a commit marked as a *before*-anchor (see
/// [LogPanel::before_marked_heads]). Reads as "what we're placing goes above
/// this one", the mirror of the after-anchors' [NODE_MARKED].
const NODE_BEFORE: char = '⌄';
/// Node glyph shown in place of jj's usual node for a commit `jj absorb` just
/// moved hunks into.
const NODE_ABSORBED: char = '★';
/// Node glyph shown in place of jj's usual node for a commit `jj absorb`
/// rewrote only by rebasing it (no hunks moved into it).
const NODE_REBASED: char = '☆';

/// Gutter bar marking a line whose revision is in the highlight set.
const GUTTER_MARK: &str = "▌";
/// Stand-in keeping unhighlighted lines aligned with highlighted ones.
const GUTTER_BLANK: &str = " ";
/// Space between the gutter bar and the graph.
const GUTTER_SEPARATOR: &str = " ";
/// Colour of the gutter bar. Avoids the meanings already in play elsewhere --
/// yellow is search and untracked files, red is conflicts and errors.
const GUTTER_COLOR: Color = Color::Cyan;

/// Longest highlight expression shown in the panel title before it is elided.
/// A revset can be arbitrarily long, and the log's own revset shares the title.
const TITLE_MAX_LEN: usize = 30;

/// Shorten `text` for the panel title, keeping the tail off. Counts characters
/// rather than bytes so a multi-byte path is not cut mid-character.
fn truncate_for_title(text: &str) -> String {
    if text.chars().count() <= TITLE_MAX_LEN {
        return text.to_owned();
    }
    let kept: String = text.chars().take(TITLE_MAX_LEN.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Append what is being marked to the log panel's title.
///
/// Appends rather than replaces, so the log's own revset stays visible in exactly
/// the case that would otherwise be baffling: a marking that matches plenty of
/// revisions, none of which that revset lets through. That case also names the
/// counts and the key that widens the revset, because the status message which
/// first reports it is gone by the next keypress -- and an empty gutter invites
/// precisely the scrolling that clears it. On the border, the explanation stays
/// where someone hunting for the missing marks will actually look.
fn append_marking_to_title(
    title: &str,
    marking: &str,
    errored: bool,
    matching: usize,
    visible: usize,
) -> String {
    let title = title.trim_end();
    let marking = truncate_for_title(marking);

    if errored {
        // Say so when the revset failed, rather than showing it as though it were
        // marking an empty set.
        return format!("{title} — marking failed: {marking} ");
    }
    if matching > 0 && visible == 0 {
        return format!(
            "{title} — marking: {marking} (0 of {matching} in view — {WIDEN_KEY_LABEL} to show) "
        );
    }
    format!("{title} — marking: {marking} ")
}

/// Set the background colour of a whole line, including past its last span so
/// the highlight runs to the edge of the panel.
fn set_bg(line: &mut Line, bg_color: Color) {
    // Set background to use when no Span is present
    // This makes the highlight continue beyond the last Span
    line.style = line.style.patch(Style::default().bg(bg_color));

    for span in line.spans.iter_mut() {
        span.style = span.style.bg(bg_color)
    }
}

/// The gutter spans for one log line.
///
/// Returns nothing at all when no highlight is active, so an unmarked log keeps
/// its columns exactly where they were; a highlight shifts every line right by the
/// same amount, marked or not, so the graph stays aligned with itself.
///
/// Split into two spans rather than one `"▌ "` so the bar's colour cannot bleed
/// into the separator.
pub fn gutter_spans<'s>(active: bool, highlighted: bool) -> Vec<Span<'s>> {
    if !active {
        return vec![];
    }
    let mark = if highlighted {
        GUTTER_MARK
    } else {
        GUTTER_BLANK
    };
    vec![
        Span::styled(mark, Style::default().fg(GUTTER_COLOR)),
        Span::raw(GUTTER_SEPARATOR),
    ]
}

/*
pub enum LogPanelEvent {
    /* Commands to LogPanel */

    /// Refresh current state
    Refresh,
    /// Move selection down the given number of changes
    MoveRelative(isize),

    /* Notifications from LogPanel */

    /// Emitted when selection was changed
    SetHead(Head),
}
*/

fn get_head_index(head: &Head, log_output: &Result<LogOutput, CommandError>) -> Option<usize> {
    match log_output {
        Ok(log_output) => log_output
            .heads
            .iter()
            .position(|heads| heads == head)
            .or_else(|| {
                log_output
                    .heads
                    .iter()
                    .position(|commit| commit.change_id == head.change_id)
            }),
        Err(_) => None,
    }
}

impl<'a> LogPanel<'a> {
    pub fn new() -> Result<Self> {
        let log_revset = new_commander().env.default_revset.clone();
        let log_output = new_commander().get_log(&log_revset, &[]);
        let head = new_commander().get_current_head()?;

        let log_list_state = ListState::default().with_selected(get_head_index(&head, &log_output));

        let mut keybinds = LogTabKeybinds::default();
        if let Some(keybinds_config) = new_commander().env.jj_config.keybinds() {
            keybinds.extend_from_config(keybinds_config);
        }

        let log_output_text = match log_output.as_ref() {
            Ok(log_output) => log_output
                .graph
                .into_text()
                .unwrap_or(Text::from("Could not turn text into TUI text (coloring)")),
            Err(_) => Text::default(),
        };

        Ok(Self {
            log_output_text,
            log_output,
            log_list_state,
            log_rect: Rect::ZERO,

            log_revset,

            head,
            marked_heads: HashSet::new(),
            before_marked_heads: HashSet::new(),
            marks_are_parents: false,
            absorbed_heads: HashSet::new(),
            rebased_heads: HashSet::new(),
            title_override: None,
            search: SearchState::new(),
            highlight: HighlightState::new(),

            panel_rect: Rect::ZERO,

            config: get_env().jj_config.clone(),
        })
    }

    //
    //  Handle jj log output
    //

    /// Run jj log and store output for display
    pub fn refresh_log_output(&mut self) {
        let marked_ids: Vec<&str> = self.marked_heads.iter().map(CommitId::as_str).collect();
        let before_ids: Vec<&str> = self
            .before_marked_heads
            .iter()
            .map(CommitId::as_str)
            .collect();
        let absorbed_ids: Vec<&str> = self.absorbed_heads.iter().map(ChangeId::as_str).collect();
        let rebased_ids: Vec<&str> = self.rebased_heads.iter().map(ChangeId::as_str).collect();
        // `✚` reads as "add a parent edge", which only makes sense while the
        // marks really are an edit of the parent set. Once a before-anchor puts
        // the gesture in insert mode they are plain anchors again, so they fall
        // back to `✓` and match the hint in the panel title.
        let mark_glyph = if self.marks_are_parents && self.before_marked_heads.is_empty() {
            NODE_PARENT
        } else {
            NODE_MARKED
        };
        let node_overrides = [
            (mark_glyph, marked_ids.as_slice()),
            (NODE_BEFORE, before_ids.as_slice()),
            (NODE_ABSORBED, absorbed_ids.as_slice()),
            (NODE_REBASED, rebased_ids.as_slice()),
        ];

        self.log_output = new_commander().get_log(&self.log_revset, &node_overrides);
        self.log_output_text = match self.log_output.as_ref() {
            Ok(log_output) => log_output
                .graph
                .into_text()
                .unwrap_or(Text::from("Could not turn text into TUI text (coloring)")),
            Err(_) => Text::default(),
        };

        // Re-run the highlight revset on every refresh rather than caching it.
        // Keying by change ID keeps the set valid across the commit-ID rewrites
        // that operations perform, but not across changes in what the revset
        // *selects*: squashing moves hunks between revisions, so `files(...)`
        // legitimately picks a different set afterwards. Refetching is one extra
        // `jj log` on a path that already runs two.
        self.refresh_highlight();
    }

    /// Re-run the active highlight revset, if any, and store the result.
    fn refresh_highlight(&mut self) {
        let Some(revset) = self.highlight.revset().map(str::to_owned) else {
            return;
        };
        let label = self.highlight.display().map(str::to_owned);
        // `display()` falls back to the revset, so only keep it as a label when
        // it is genuinely a different, friendlier string.
        let label = label.filter(|label| label != &revset);

        match new_commander().get_changes_in(&revset) {
            Ok(matching) => self.highlight.set(revset, label, matching),
            Err(_) => self.highlight.set_errored(revset, label),
        }
    }

    /// Convert log output to a list of formatted lines
    ///
    /// Marked and absorbed-into commits are shown by replacing jj's graph node
    /// glyph itself (via a `templates.log_node` override baked into
    /// `log_output.graph`, see [Self::refresh_log_output]), rather than by
    /// adding a separate gutter mark, so the selection reads as "this node,
    /// right here" instead of an extra column to parse.
    ///
    /// The highlight set is the exception: it *does* get a gutter column, because
    /// unlike a mark it has to be readable at the same time as the node glyph,
    /// `@`, and the selection -- a glyph swap can only show one thing at a time.
    fn output_to_lines(&self, log_output: &LogOutput) -> Vec<Line<'a>> {
        let highlighting = self.highlight.is_active();

        self.log_output_text
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let mut line = line.to_owned();

                // Prepend the gutter before anything else, so the selection
                // background below paints it too and the selected row reads as one
                // continuous band instead of a bar floating outside the highlight.
                // `head_at` is None for the synthetic "(elided revisions)" row,
                // which correctly gets a blank rather than a mark.
                if highlighting {
                    let highlighted = log_output
                        .head_at(i)
                        .is_some_and(|head| self.highlight.matches(head));
                    for span in gutter_spans(true, highlighted).into_iter().rev() {
                        line.spans.insert(0, span);
                    }
                }

                // Highlight lines that correspond to self.head first, so the
                // search match (applied after) wins on the selected line and
                // stays legible instead of being repainted by the selection.
                if log_output.head_at(i) == Some(&self.head) {
                    set_bg(&mut line, self.config.highlight_color());
                };

                // Highlight the search query wherever it appears in the line.
                if let Some(query) = self.search.query() {
                    highlight_matches(&mut line, query);
                }

                line
            })
            .collect()
    }

    /// Get lines to show in log list
    fn log_lines(&self) -> Vec<Line<'a>> {
        match self.log_output.as_ref() {
            Ok(log_output) => self.output_to_lines(log_output),
            Err(err) => err.into_text("Error getting log").unwrap().lines,
        }
    }

    /// Get a list of all heads in log list
    pub fn log_heads(&self) -> Vec<Head> {
        match self.log_output.as_ref() {
            Ok(log_output) => log_output.heads.clone(),
            Err(_) => vec![],
        }
    }

    //
    //  Selected head and the special head index
    //

    /// Find the line in self.log_output that match self.head
    fn selected_log_line(&self) -> Option<usize> {
        let log_output = self.log_output.as_ref().ok()?;

        log_output
            .graph_heads
            .iter()
            .position(|opt_h| opt_h.as_ref().is_some_and(|h| h == &self.head))
    }

    /// Find head of the provided log_output line
    fn head_at_log_line(&mut self, log_line: usize) -> Option<Head> {
        self.log_output.as_ref().ok()?.head_at(log_line).cloned()
    }

    // Return the head-index for the selection
    fn get_current_head_index(&self) -> Option<usize> {
        get_head_index(&self.head, &self.log_output)
    }

    /// Number of log list items that fit on screen. Think of this as
    /// in unit head-index. Moving the head-index this much causes a
    /// full page scroll.
    fn visible_heads(&self) -> u16 {
        // Every item in the log list is 2 lines high, so divide screen rows
        // by 2 to get the number of log items that fit in it.
        self.log_rect.height / 2
    }

    /// Move selection to a specific head. This may cause the next draw to
    /// scroll to a different line.
    pub fn set_head(&mut self, head: Head) {
        head.clone_into(&mut self.head);
    }

    /// Move selection relative to the current position.
    /// The scroll is relative to head-index, not line-index.
    /// This will update self.head
    fn scroll_relative(&mut self, scroll: isize) {
        let log_output = match self.log_output.as_ref() {
            Ok(log_output) => log_output,
            Err(_) => return,
        };

        let heads: &Vec<Head> = log_output.heads.as_ref();

        let current_head_index = self.get_current_head_index();
        let next_head = match current_head_index {
            Some(current_head_index) => heads.get(
                current_head_index
                    .saturating_add_signed(scroll)
                    .min(heads.len() - 1),
            ),
            None => heads.first(),
        };
        if let Some(next_head) = next_head {
            self.set_head(next_head.clone());
        }
        // TODO Notify about change of head
    }

    //
    //  Marked heads
    //

    /// Mark or unmark the specified head
    ///
    /// Re-runs `jj log` immediately: the mark is now shown by replacing the
    /// commit's graph node glyph (see [Self::refresh_log_output]), which is
    /// baked into the fetched log text rather than added at render time, so
    /// it needs a fresh fetch to become visible.
    pub fn set_head_mark(&mut self, head: &Head, mark: bool) {
        if mark {
            // The two anchor sets are exclusive: a revision cannot be both
            // above and below the change being placed.
            self.before_marked_heads.remove(&head.commit_id);
            self.marked_heads.insert(head.commit_id.clone());
        } else {
            self.marked_heads.remove(&head.commit_id);
        }
        self.refresh_log_output();
    }

    /// Check if a head is marked for batch operation
    pub fn is_head_marked(&self, head: &Head) -> bool {
        self.marked_heads.contains(&head.commit_id)
    }

    /// LogTabEvent: Toggle mark on the current head
    pub fn toggle_head_mark(&mut self) {
        let was_marked = self.is_head_marked(&self.head);
        self.set_head_mark(&self.head.clone(), !was_marked);
    }

    /// LogTabEvent: Toggle the before-anchor mark on the current head.
    ///
    /// Mirrors [Self::toggle_head_mark]; marking as a before-anchor clears any
    /// after-mark on the same revision, since the two are exclusive.
    pub fn toggle_head_before_mark(&mut self) {
        let commit_id = self.head.commit_id.clone();
        if self.before_marked_heads.remove(&commit_id) {
            self.refresh_log_output();
            return;
        }
        self.marked_heads.remove(&commit_id);
        self.before_marked_heads.insert(commit_id);
        self.refresh_log_output();
    }

    /// Extract the list of all marked heads and clear it
    ///
    /// Refreshes immediately, for the same reason [Self::set_head_mark] does:
    /// the mark glyph is baked into the fetched log text.
    pub fn extract_and_clear_head_marks(&mut self) -> Vec<CommitId> {
        let commit_ids = self.marked_heads.drain().collect();
        self.refresh_log_output();
        commit_ids
    }

    /// Extract the before-anchor marks and clear them, like
    /// [Self::extract_and_clear_head_marks].
    pub fn extract_and_clear_before_marks(&mut self) -> Vec<CommitId> {
        let commit_ids = self.before_marked_heads.drain().collect();
        self.refresh_log_output();
        commit_ids
    }

    //
    //  Highlight revset
    //

    /// Whether a highlight revset is currently active.
    pub fn has_active_highlight(&self) -> bool {
        self.highlight.is_active()
    }

    /// The active highlight revset, or `None`.
    pub fn highlight_revset(&self) -> Option<&str> {
        self.highlight.revset()
    }

    /// Apply `revset` as the highlight set, fetching the revisions it selects.
    /// An empty or whitespace-only revset clears the highlight.
    ///
    /// `label` replaces the revset in the panel title, for revsets built on the
    /// user's behalf (a file filter shows the path, not the `files(...)` wrapper).
    ///
    /// Returns what happened so the caller can pick the right feedback: an empty
    /// gutter means something different when the revset matches nothing than when
    /// it matches plenty of revisions that are all outside the log's own revset.
    pub fn set_highlight(&mut self, revset: &str, label: Option<String>) -> HighlightOutcome {
        let revset = revset.trim();
        if revset.is_empty() {
            self.highlight.clear();
            return HighlightOutcome::Cleared;
        }

        match new_commander().get_changes_in(revset) {
            Ok(matching) => {
                self.highlight.set(revset.to_owned(), label, matching);
                if self.highlight.matching_count() == 0 {
                    return HighlightOutcome::NoneMatching;
                }
                HighlightOutcome::Applied {
                    matching: self.highlight.matching_count(),
                    visible: self.visible_highlight_count(),
                }
            }
            Err(err) => {
                let message = format!("{err}");
                self.highlight.set_errored(revset.to_owned(), label);
                HighlightOutcome::Failed(message)
            }
        }
    }

    /// How many of the revisions currently shown in the log are highlighted.
    ///
    /// Distinct from [HighlightState::matching_count], which counts the whole
    /// repo: a revset can select many revisions with none of them in view.
    fn visible_highlight_count(&self) -> usize {
        self.log_heads()
            .iter()
            .filter(|head| self.highlight.matches(head))
            .count()
    }

    /// Clear the highlight revset.
    pub fn clear_highlight(&mut self) {
        self.highlight.clear();
    }

    //
    //  Absorbed heads
    //

    /// Clear the set of changes highlighted as absorbed into
    ///
    /// Refreshes immediately, for the same reason [Self::set_head_mark] does:
    /// the highlight glyph is baked into the fetched log text.
    pub fn clear_absorbed_heads(&mut self) {
        if self.absorbed_heads.is_empty() && self.rebased_heads.is_empty() {
            return;
        }
        self.absorbed_heads.clear();
        self.rebased_heads.clear();
        self.refresh_log_output();
    }

    //
    //  Search
    //

    /// Whether a search is currently active (a non-empty query is set).
    pub fn has_active_search(&self) -> bool {
        self.search.is_active()
    }

    /// Set the live search query used for highlighting as the user types.
    /// An empty or whitespace-only query clears the search. Does not move
    /// the selection — navigation happens on Enter / n / N.
    pub fn set_search_query(&mut self, query: &str) {
        self.search.set_query(query);
    }

    /// Clear any active search (query and highlights).
    pub fn clear_search(&mut self) {
        self.search.clear();
    }

    /// The heads that currently match the search query, in log (top-to-bottom)
    /// order and de-duplicated. A head matches if any of its displayed lines
    /// contains the query. Empty when there's no query or no match.
    fn matching_heads(&self) -> Vec<Head> {
        let Some(query) = self.search.query() else {
            return vec![];
        };
        let Ok(log_output) = self.log_output.as_ref() else {
            return vec![];
        };

        let mut matches: Vec<Head> = Vec::new();
        for (i, line) in self.log_output_text.iter().enumerate() {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .to_lowercase();
            if !text.contains(query) {
                continue;
            }
            if let Some(head) = log_output.head_at(i)
                && !matches.contains(head)
            {
                matches.push(head.clone());
            }
        }
        matches
    }

    /// Move the selection to the first match at or after the current
    /// selection (wrapping to the top). Used on Enter, right after the query
    /// is set. Returns the number of matches found (0 if none).
    pub fn select_first_match(&mut self) -> usize {
        let matches = self.matching_heads();
        let heads = self.log_heads();
        let current = self.head_position(&heads);
        let match_positions = self.match_positions(&matches, &heads);
        if let Some(pos) = first_match_index_at_or_after(&match_positions, current)
            && let Some(head) = heads.get(pos)
        {
            self.set_head(head.clone());
        }
        matches.len()
    }

    /// Move the selection to the next (`forward`) or previous match relative
    /// to the current selection, wrapping around. Returns the number of
    /// matches (0 if none).
    pub fn select_adjacent_match(&mut self, forward: bool) -> usize {
        let matches = self.matching_heads();
        let heads = self.log_heads();
        let current = self.head_position(&heads);
        let match_positions = self.match_positions(&matches, &heads);
        if let Some(pos) = next_match_index(&match_positions, current, forward)
            && let Some(head) = heads.get(pos)
        {
            self.set_head(head.clone());
        }
        matches.len()
    }

    /// The index of the current selection within `heads` (0 if not found).
    fn head_position(&self, heads: &[Head]) -> usize {
        heads.iter().position(|h| h == &self.head).unwrap_or(0)
    }

    /// The positions (indices into `heads`) of the matching heads, ascending.
    fn match_positions(&self, matches: &[Head], heads: &[Head]) -> Vec<usize> {
        let mut positions: Vec<usize> = matches
            .iter()
            .filter_map(|h| heads.iter().position(|x| x == h))
            .collect();
        positions.sort_unstable();
        positions
    }

    //
    //  Event handling
    //

    pub fn handle_event(&mut self, log_tab_event: LogTabEvent) -> Result<ComponentInputResult> {
        match log_tab_event {
            LogTabEvent::ScrollDown => {
                self.scroll_relative(1);
            }
            LogTabEvent::ScrollUp => {
                self.scroll_relative(-1);
            }
            LogTabEvent::ScrollDownHalf => {
                self.scroll_relative(self.visible_heads() as isize / 2);
            }
            LogTabEvent::ScrollUpHalf => {
                self.scroll_relative((self.visible_heads() as isize / 2).saturating_neg());
            }
            LogTabEvent::ScrollToBottom => {
                self.scroll_relative(isize::MAX);
            }
            LogTabEvent::ScrollToTop => {
                self.scroll_relative(-isize::MAX);
            }
            LogTabEvent::ToggleHeadBeforeMark => {
                self.toggle_head_before_mark();
                return Ok(ComponentInputResult::Handled);
            }
            LogTabEvent::ToggleHeadMark => {
                self.toggle_head_mark();
            }
            _ => {
                return Ok(ComponentInputResult::NotHandled);
            }
        }
        Ok(ComponentInputResult::Handled)
    }
}

impl Component for LogPanel<'_> {
    // Called when switching to tab
    fn focus(&mut self) -> Result<()> {
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        self.panel_rect = area;

        let mut title = match (&self.title_override, &self.log_revset) {
            (Some(title_override), _) => title_override.clone(),
            (None, Some(log_revset)) => format!(" Log for: {log_revset} "),
            (None, None) => " Log ".to_owned(),
        };

        if let Some(highlighted) = self.highlight.display() {
            title = append_marking_to_title(
                &title,
                highlighted,
                self.highlight.errored(),
                self.highlight.matching_count(),
                self.visible_highlight_count(),
            );
        }

        let log_lines = self.log_lines();
        let log_length: usize = log_lines.len();
        let log_block = Block::bordered()
            .title(title)
            .border_type(BorderType::Rounded);
        self.log_rect = log_block.inner(area);
        self.log_list_state.select(self.selected_log_line());
        let log = List::new(log_lines).block(log_block).scroll_padding(7);
        f.render_stateful_widget(log, area, &mut self.log_list_state);

        // Show scrollbar if lines don't fit the screen height
        if log_length > self.log_rect.height.into() {
            let index = self.log_list_state.selected().unwrap_or(0);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            let mut scrollbar_state = ScrollbarState::default()
                .content_length(log_length)
                .position(index);

            f.render_stateful_widget(
                scrollbar,
                area.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut scrollbar_state,
            );
        }

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        if let Event::Mouse(mouse_event) = event {
            // Determine if mouse event is inside log-view
            let mouse_pos = Position::new(mouse_event.column, mouse_event.row);
            if !self.panel_rect.contains(mouse_pos) {
                return Ok(ComponentInputResult::NotHandled);
            }

            // Execute command dependent on panel and event kind
            match mouse_event.kind {
                MouseEventKind::ScrollUp => {
                    self.handle_event(LogTabEvent::ScrollUp)?;
                    return Ok(ComponentInputResult::Handled);
                }
                MouseEventKind::ScrollDown => {
                    self.handle_event(LogTabEvent::ScrollDown)?;
                    return Ok(ComponentInputResult::Handled);
                }
                MouseEventKind::Up(_) => {
                    // Check all items in list

                    // TODO make a function that constructs the log list
                    let log_lines = self.log_lines();
                    let log_items: Vec<ListItem> = log_lines
                        .iter()
                        .map(|line| ListItem::from(line.to_text()))
                        .collect();

                    // Select the clicked change
                    if let Some(inx) = list_item_from_mouse_event(
                        &log_items,
                        self.log_rect,
                        &self.log_list_state,
                        &mouse_event,
                    ) && let Some(head) = self.head_at_log_line(inx)
                    {
                        self.set_head(head);
                        return Ok(ComponentInputResult::Handled);
                    }
                }
                _ => {} // Handle other mouse events if necessary
            }
        }

        Ok(ComponentInputResult::NotHandled)
    }
}

// Determine which list item a mouse event is related to
fn list_item_from_mouse_event(
    list: &[ListItem],
    list_rect: Rect,
    list_state: &ListState,
    mouse_event: &MouseEvent,
) -> Option<usize> {
    let mouse_pos = Position::new(mouse_event.column, mouse_event.row);
    if !list_rect.contains(mouse_pos) {
        return None;
    }

    // Assume that each item is exactly one line.
    // This is not true in the general case, but it is in this module.
    let mouse_offset = mouse_pos.y - list_rect.y;
    let item_index = list_state.offset() + mouse_offset as usize;
    if item_index >= list.len() {
        return None;
    }
    Some(item_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::search::highlight_matches;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// A log line with the gutter already prepended, as `output_to_lines` builds
    /// it.
    fn gutter_line(highlighted: bool) -> Line<'static> {
        let mut line = Line::from("○  kntqzsrq fix path handling");
        for span in gutter_spans(true, highlighted).into_iter().rev() {
            line.spans.insert(0, span);
        }
        line
    }

    #[test]
    fn gutter_is_absent_when_no_highlight_is_active() {
        // The column-shift guarantee: with no highlight, the log's columns are
        // exactly where they were before this feature existed.
        assert!(gutter_spans(false, false).is_empty());
        assert!(gutter_spans(false, true).is_empty());
    }

    #[test]
    fn marked_and_unmarked_gutters_are_the_same_width() {
        // Unequal widths would stagger the graph between marked and unmarked rows.
        let width = |highlighted| -> usize {
            gutter_spans(true, highlighted)
                .iter()
                .map(|span| span.content.chars().count())
                .sum()
        };
        let expected = GUTTER_MARK.chars().count() + GUTTER_SEPARATOR.chars().count();
        assert_eq!(width(true), expected);
        assert_eq!(width(false), expected);
    }

    #[test]
    fn only_the_marked_gutter_draws_a_bar() {
        assert_eq!(
            line_text(&gutter_line(true)),
            "▌ ○  kntqzsrq fix path handling"
        );
        assert_eq!(
            line_text(&gutter_line(false)),
            "  ○  kntqzsrq fix path handling"
        );
    }

    #[test]
    fn gutter_survives_the_selection_background() {
        // The gutter is prepended before the selection is painted, so the bar sits
        // inside the highlight band rather than leaving a hole at its left edge --
        // and keeps its own foreground colour, since set_bg only sets `bg`.
        let mut line = gutter_line(true);
        set_bg(&mut line, Color::Rgb(50, 50, 150));

        let bar = line.spans.first().expect("gutter span");
        assert_eq!(bar.content.as_ref(), GUTTER_MARK);
        assert_eq!(bar.style.bg, Some(Color::Rgb(50, 50, 150)));
        assert_eq!(bar.style.fg, Some(GUTTER_COLOR));
    }

    #[test]
    fn gutter_survives_a_search_highlight() {
        // `highlight_matches` flattens the line and rebuilds every span, so the
        // gutter has to come through that intact.
        let mut line = gutter_line(true);
        highlight_matches(&mut line, "path");

        assert_eq!(line_text(&line), "▌ ○  kntqzsrq fix path handling");
        let bar = line.spans.first().expect("gutter span");
        assert_eq!(bar.content.as_ref(), GUTTER_MARK);
        assert_eq!(bar.style.fg, Some(GUTTER_COLOR));
    }

    #[test]
    fn truncate_for_title_leaves_short_text_alone() {
        assert_eq!(truncate_for_title("src/app.rs"), "src/app.rs");
    }

    #[test]
    fn truncate_for_title_elides_a_long_revset() {
        let long = "description(glob:'*something quite long here*')";
        let truncated = truncate_for_title(long);
        assert_eq!(truncated.chars().count(), TITLE_MAX_LEN);
        assert!(truncated.ends_with('…'), "got {truncated:?}");
    }

    #[test]
    fn truncate_for_title_counts_characters_not_bytes() {
        // A path of multi-byte characters must not be cut mid-character.
        let wide = "é".repeat(TITLE_MAX_LEN + 10);
        let truncated = truncate_for_title(&wide);
        assert_eq!(truncated.chars().count(), TITLE_MAX_LEN);
    }

    #[test]
    fn title_names_what_is_being_marked() {
        assert_eq!(
            append_marking_to_title(" Log ", "src/app.rs", false, 3, 3),
            " Log — marking: src/app.rs "
        );
    }

    #[test]
    fn title_keeps_the_logs_own_revset() {
        // Both halves matter: what am I looking at, and what am I looking for.
        assert_eq!(
            append_marking_to_title(" Log for: ::@ ", "conflicts()", false, 2, 1),
            " Log for: ::@ — marking: conflicts() "
        );
    }

    #[test]
    fn title_explains_an_empty_gutter_and_offers_the_way_out() {
        // The regression this guards: with matches that the log's revset excludes,
        // the gutter is empty and the status message reporting why is gone by the
        // next keypress. The title has to carry it.
        let title = append_marking_to_title(" Log ", "justfile", false, 3, 0);
        assert_eq!(
            title,
            format!(" Log — marking: justfile (0 of 3 in view — {WIDEN_KEY_LABEL} to show) ")
        );
    }

    #[test]
    fn title_does_not_offer_to_widen_when_marks_are_visible() {
        let title = append_marking_to_title(" Log ", "justfile", false, 3, 1);
        assert!(!title.contains(WIDEN_KEY_LABEL), "got {title:?}");
    }

    #[test]
    fn title_does_not_offer_to_widen_when_nothing_matches() {
        // Nothing to widen towards, so no offer -- an empty gutter is the answer.
        let title = append_marking_to_title(" Log ", "no/such/path", false, 0, 0);
        assert_eq!(title, " Log — marking: no/such/path ");
    }

    #[test]
    fn title_reports_a_failed_revset_as_failed() {
        // Never shown as though it were marking an empty set.
        let title = append_marking_to_title(" Log ", "bogusfn(", true, 0, 0);
        assert_eq!(title, " Log — marking failed: bogusfn( ");
        assert!(!title.contains(WIDEN_KEY_LABEL), "got {title:?}");
    }
}
