#![expect(clippy::borrow_interior_mutable_const)]

use std::collections::BTreeSet;

use anyhow::Result;
use ratatui::crossterm::clipboard::CopyToClipboard;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::event::KeyModifiers;
use ratatui::crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::*;
use ratatui_textarea::CursorMove;
use ratatui_textarea::TextArea;
use tracing::instrument;
use tui_confirm_dialog::ButtonLabel;
use tui_confirm_dialog::ConfirmDialog;
use tui_confirm_dialog::ConfirmDialogState;
use tui_confirm_dialog::Listener;

use crate::commander::Commander;
use crate::commander::files::ConflictSide;
use crate::commander::ids::CommitId;
use crate::commander::log::Head;
use crate::commander::new_commander;
use crate::env::DiffFormat;
use crate::env::JjConfig;
use crate::env::get_env;
use crate::keybinds::LogTabEvent;
use crate::keybinds::LogTabKeybinds;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::commit_show_cache::CommitShowCache;
use crate::ui::commit_show_cache::CommitShowKey;
use crate::ui::commit_show_cache::CommitShowValue;
use crate::ui::dialog::BookmarkSetPopup;
use crate::ui::dialog::HelpPopup;
use crate::ui::dialog::LoaderPopup;
use crate::ui::dialog::MessagePopup;
use crate::ui::dialog::TagSetPopup;
use crate::ui::highlight::HighlightOutcome;
use crate::ui::highlight::WIDEN_KEY_LABEL;
use crate::ui::panel::DetailsPanel;
use crate::ui::panel::LargeStringContent;
use crate::ui::panel::LogPanel;
use crate::ui::utils::PaneDivider;
use crate::ui::utils::centered_rect_line_height;
use crate::ui::utils::tabs_to_spaces;

const NEW_POPUP_ID: u16 = 1;
const ABANDON_POPUP_ID: u16 = 3;
const METAEDIT_UPDATE_CHANGE_ID_POPUP_ID: u16 = 5;
const RESOLVE_POPUP_ID: u16 = 7;

/// State of a multi-phase "pick up, put down" gesture (rebase, squash,
/// insert).
///
/// The gesture's first pick happens *before* the action key: the marked
/// changes, or the change under the cursor if none are marked. Pressing the
/// action key consumes that pick and enters one of these collecting states
/// for the next pick; Enter advances (again taking the marks, or the cursor
/// if none), and Esc cancels. The final Enter executes directly — the
/// deliberate multi-phase gesture is its own confirmation.
#[derive(Clone)]
enum PickState {
    /// No gesture in progress.
    Idle,
    /// After the rebase key: the marks are the candidate parent set, shown
    /// with a distinctive glyph. For a single source they are pre-seeded
    /// with its current parents (`original_parents`), so toggling marks
    /// adds/removes parents relative to today. On Enter the marks become
    /// the destinations — unless they are exactly `original_parents` still
    /// (a no-op rebase nobody wants), in which case the change under the
    /// cursor is the destination, preserving the plain "move it there"
    /// gesture. Pressing the rebase key again toggles whether descendants
    /// come along (`jj rebase -s` vs `-r`).
    RebaseDestinations {
        sources: Vec<CommitId>,
        original_parents: Vec<CommitId>,
        include_descendants: bool,
    },
    /// After the branch-rebase key: collecting the destination(s) for
    /// `jj rebase -b`. No parent set can be shown here: which commits get
    /// new parents (the branch roots) is itself a function of the
    /// destination, so this stays a plain destination pick.
    BranchRebaseDestinations { sources: Vec<CommitId> },
    /// After the squash key: collecting the single destination. On entry the
    /// cursor is placed on the source's parent, so an immediate Enter gives
    /// `jj squash` semantics. Pressing the squash key again toggles
    /// `interactive` (`jj squash -i`), which hands the terminal to the
    /// user's diff editor to pick the hunks that move.
    SquashDestination {
        sources: Vec<CommitId>,
        ignore_immutable: bool,
        interactive: bool,
    },
    /// After the diffedit key: collecting the single `--from` base to edit
    /// `target` against. The cursor stays on `target`, so an immediate Enter
    /// leaves the base unset and edits the revision's own diff against all its
    /// parents (`jj diffedit -r target`). Marking a revision, or moving the
    /// cursor off `target`, edits relative to that revision instead
    /// (`jj diffedit --from <base> --to target`).
    DiffEditFrom { target: CommitId },
}

/// Draw a one-line prompt bar over the bottom border row of `panel_area`, with
/// `prompt` in `prompt_color` followed by `textarea`.
///
/// Shared by the `/` search bar and the file-filter bar, which differ only in
/// their prompt: both sit on the panel's bottom border so they cost no log rows.
fn draw_prompt_bar(
    f: &mut Frame<'_>,
    panel_area: Rect,
    prompt: &str,
    prompt_color: Color,
    textarea: &TextArea<'_>,
) {
    // Sit on the bottom border row of the panel, inset past the rounded corners.
    let bar = Rect {
        x: panel_area.x + 1,
        y: panel_area.y + panel_area.height.saturating_sub(1),
        width: panel_area.width.saturating_sub(2),
        height: 1,
    };
    f.render_widget(Clear, bar);

    let prompt_width = prompt.chars().count() as u16;
    let prompt_rect = Rect {
        width: prompt_width.min(bar.width),
        ..bar
    };
    f.render_widget(
        Span::styled(prompt.to_owned(), Style::new().fg(prompt_color).bold()),
        prompt_rect,
    );

    let input = Rect {
        x: bar.x + prompt_width,
        width: bar.width.saturating_sub(prompt_width),
        ..bar
    };
    f.render_widget(textarea, input);
}

/// Which log-tab events still make sense while the cursor is parked on an
/// "(elided revisions)" row.
///
/// Navigation, view controls, and repo-wide actions are fine; anything that
/// reads the selection as a revision to act on is not. Enter is allowed
/// because it is what expands the placeholder.
fn elided_row_allows(event: LogTabEvent) -> bool {
    matches!(
        event,
        LogTabEvent::ScrollDown
            | LogTabEvent::ScrollUp
            | LogTabEvent::ScrollDownHalf
            | LogTabEvent::ScrollUpHalf
            | LogTabEvent::ScrollToBottom
            | LogTabEvent::ScrollToTop
            | LogTabEvent::FocusCurrent
            | LogTabEvent::ToggleDiffFormat
            | LogTabEvent::Refresh
            | LogTabEvent::OpenFiles
            | LogTabEvent::EditRevset
            | LogTabEvent::Search
            | LogTabEvent::FileFilter
            | LogTabEvent::Cancel
            | LogTabEvent::ClosePopup
            | LogTabEvent::Undo
            | LogTabEvent::Redo
            | LogTabEvent::Fetch { .. }
            | LogTabEvent::OpenHelp
            | LogTabEvent::Unbound
    )
}

/// Abbreviate a git object id for display, matching the 8 characters jj shows
/// for its own commit ids. Ids shorter than that are left alone.
fn short_commit(commit: &str) -> &str {
    const SHORT_LEN: usize = 8;
    commit.get(..SHORT_LEN).unwrap_or(commit)
}

/// Width of a revset field's label column, e.g. `" Show:"` plus its trailing space.
const REVSET_FIELD_LABEL_WIDTH: u16 = 7;

/// Draw one row of the revset editor: a label, then the field's input beside it.
///
/// The focused field's label is brightened, since a [TextArea] gives no cursor of
/// its own while it is not the one taking input.
fn draw_revset_field(
    f: &mut Frame<'_>,
    row: Rect,
    label: &str,
    focused: bool,
    textarea: &TextArea<'_>,
) {
    let style = if focused {
        Style::new().bold().cyan()
    } else {
        Style::new().fg(Color::DarkGray)
    };

    let label_rect = Rect {
        width: REVSET_FIELD_LABEL_WIDTH.min(row.width),
        ..row
    };
    f.render_widget(
        Paragraph::new(Span::styled(label.to_owned(), style)),
        label_rect,
    );

    let input = Rect {
        x: row.x + REVSET_FIELD_LABEL_WIDTH,
        width: row.width.saturating_sub(REVSET_FIELD_LABEL_WIDTH),
        ..row
    };
    f.render_widget(textarea, input);
}

/// Which field of the revset editor has the cursor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RevsetField {
    /// The revset the log shows. Empty means the configured default.
    Log,
    /// The revset to mark within what the log shows. Empty means mark nothing.
    Mark,
}

/// The two-field revset editor popup.
///
/// `Tab` moves between the fields, `Ctrl+s` applies both, `Esc` cancels. Each
/// field is independently clearable: emptying the log field restores the default
/// revset, and emptying the mark field turns the gutter off.
struct RevsetEditor<'a> {
    log: TextArea<'a>,
    mark: TextArea<'a>,
    focus: RevsetField,
}

impl<'a> RevsetEditor<'a> {
    fn new(log_revset: Option<&str>, mark_revset: Option<&str>) -> Self {
        let field = |value: Option<&str>| {
            let mut textarea = TextArea::new(
                value
                    .unwrap_or_default()
                    .lines()
                    .map(String::from)
                    .collect(),
            );
            // Pre-select the existing expression so typing replaces it, the way
            // an address bar behaves. Replacing the revset is the common case,
            // and with the cursor merely parked at the end there was no quick
            // way to clear the field: typing appended, turning `dev::@` into
            // `dev::@dev::@` and failing to parse. An arrow key or a click
            // drops the selection and keeps the text for editing in place.
            textarea.move_cursor(CursorMove::End);
            textarea.select_all();
            textarea
        };
        Self {
            log: field(log_revset),
            mark: field(mark_revset),
            focus: RevsetField::Log,
        }
    }

    fn focused_mut(&mut self) -> &mut TextArea<'a> {
        match self.focus {
            RevsetField::Log => &mut self.log,
            RevsetField::Mark => &mut self.mark,
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            RevsetField::Log => RevsetField::Mark,
            RevsetField::Mark => RevsetField::Log,
        };
    }

    /// The log revset as entered, or `None` if left empty (meaning the default).
    fn log_revset(&self) -> Option<String> {
        let value = self.log.lines().join("\n");
        Some(value).filter(|value| !value.trim().is_empty())
    }

    /// The mark revset as entered, empty string if cleared.
    fn mark_revset(&self) -> String {
        self.mark.lines().join("\n")
    }
}

/// Log tab. Shows `jj log` in main panel and shows selected change details of in details panel.
pub struct LogTab<'a> {
    /// The revset editor popup: which revisions the log shows, and which of them
    /// to mark. `None` when the popup is closed.
    ///
    /// Both fields are edited together because they answer two halves of one
    /// question -- what am I looking at, and what am I looking *for* -- and
    /// keeping them in one popup means the marking expression can be written with
    /// the log's own revset visible above it.
    revset_editor: Option<RevsetEditor<'a>>,

    /// The vim-style `/` search input bar, shown at the bottom of the log
    /// panel while the user is typing a query. `None` when not searching.
    search_textarea: Option<TextArea<'a>>,

    /// The file-filter input bar, shown at the bottom of the log panel while the
    /// user is typing a path. A convenience layer over the highlight revset: it
    /// turns a path into `files(...)` so the common case needs no revset syntax.
    /// `None` when not entering one.
    file_filter_textarea: Option<TextArea<'a>>,

    /// A pending "widen the log's revset to the highlight expression" offer,
    /// holding the revset it would apply. Set when a highlight matches revisions
    /// that the log's revset excludes, so the gutter would be empty; consumed by
    /// the next keypress, alongside the status message that advertises it.
    pending_widen: Option<String>,

    /// The list of changes shown to the left
    log_panel: LogPanel<'a>,

    /// The panel showing change content to the right
    head_panel: DetailsPanel,

    /// The selected change content key in the cache
    head_key: CommitShowKey,

    /// Cached change content
    commit_show_cache: CommitShowCache,

    /// The currently selected change. It is a copy of `self.log_panel.head`,
    /// so if these differ, we need to update `self.head`
    head: Head,

    diff_format: DiffFormat,

    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    bookmark_set_popup_tx: std::sync::mpsc::Sender<bool>,
    bookmark_set_popup_rx: std::sync::mpsc::Receiver<bool>,

    tag_set_popup_tx: std::sync::mpsc::Sender<bool>,
    tag_set_popup_rx: std::sync::mpsc::Receiver<bool>,

    /// Whether the change `n` is about to create should be left alone rather
    /// than moved into (`N`), i.e. `jj new --no-edit`.
    no_edit_new: bool,

    metaedit_update_change_id_ignore_immutable: bool,

    resolve_keep_destination: bool,

    pick_state: PickState,

    config: JjConfig,
    pane_divider: PaneDivider,
    keybinds: LogTabKeybinds,
}

/**
# Supporting functions
Normally the event handling code would call
member functions on log_panel and head_panel, but some operations
are a little more complex. They get a supporting function.

The main functions are:

* [set_head](LogTab::set_head) - Move the selection to a particular
  commit. Update panels.

* [refresh_log_output](LogTab::refresh_log_output) - Update the log panel
  by running `jj log`, and update the details panel.
  (called by set_head)

* [sync_head_output](LogTab::sync_head_output) - Make right panel show
  what left panel selected.
  (called by refresh_log_output)

* [refresh_head_output](LogTab::refresh_head_output) - Update content of
  right panel
  (called by sync_head_output)

* [compute_head_content](LogTab::compute_head_content) - Call `jj show` and
  wrap the output as a ShowCacheValue
  (called by refresh_head_output)
*/
impl<'a> LogTab<'a> {
    #[instrument(level = "info", name = "Initializing log tab", parent = None, skip())]
    pub fn new() -> Result<Self> {
        let diff_format = get_env().jj_config.diff_format();

        let head = new_commander().get_current_head()?;

        const NO_WIDTH: usize = 0;
        let head_key = CommitShowKey::new(head.clone(), diff_format.clone(), NO_WIDTH);

        let mut commit_show_cache = CommitShowCache::new();

        let _new_content = commit_show_cache.get_or_insert(&head_key, || {
            Self::compute_head_content(NO_WIDTH, &head, &diff_format)
        });

        let (popup_tx, popup_rx) = std::sync::mpsc::channel();
        let (bookmark_set_popup_tx, bookmark_set_popup_rx) = std::sync::mpsc::channel();
        let (tag_set_popup_tx, tag_set_popup_rx) = std::sync::mpsc::channel();

        let mut keybinds = LogTabKeybinds::default();
        if let Some(keybinds_config) = get_env().jj_config.keybinds() {
            keybinds.extend_from_config(keybinds_config);
        }
        keybinds.extend_from_description_transforms(get_env().jj_config.description_transforms());

        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        let mut log_tab = Self {
            revset_editor: None,
            file_filter_textarea: None,
            pending_widen: None,
            search_textarea: None,

            log_panel: LogPanel::new()?,

            head,
            head_panel: DetailsPanel::new(),
            head_key,

            commit_show_cache,

            diff_format,

            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,

            bookmark_set_popup_tx,
            bookmark_set_popup_rx,

            tag_set_popup_tx,
            tag_set_popup_rx,

            no_edit_new: false,

            metaedit_update_change_id_ignore_immutable: false,

            resolve_keep_destination: false,

            pick_state: PickState::Idle,

            config,
            pane_divider,
            keybinds,
        };

        // Seed the submodule check so a pointer that was already moved before
        // jjscope started is reported on the first frame, not only after a
        // refresh.
        log_tab.refresh_dirty_submodules();

        Ok(log_tab)
    }

    /// Set cursor and update log panel and diff panel
    pub fn set_head(&mut self, head: Head) {
        self.log_panel.set_head(head);
        self.refresh_log_output();
    }

    /// Update the log panel and diff panel. This will also refresh
    /// the diff cache.
    fn refresh_log_output(&mut self) {
        self.log_panel.refresh_log_output();
        self.update_cache_active_commits();
        self.sync_head_output();
    }

    /// Re-check whether any submodule's checked-out commit has drifted from
    /// what `@` records, and show it in the panel title.
    ///
    /// Deliberately not called from [Self::refresh_log_output], which runs on
    /// every cursor move: this shells out to git once per submodule and the
    /// answer depends only on `@` and the working copy, neither of which
    /// scrolling changes. Refreshes, tab focus, and startup cover it.
    ///
    /// Costs nothing in a repo without submodules — [Commander::has_submodules]
    /// is a single filesystem check.
    fn refresh_dirty_submodules(&mut self) {
        let commander = new_commander();
        if !commander.has_submodules() {
            self.log_panel.dirty_submodules.clear();
            return;
        }
        // A failure here means git could not answer, which is not worth a popup
        // on a background check: fall back to reporting nothing dirty.
        self.log_panel.dirty_submodules = commander
            .get_current_head()
            .and_then(|head| commander.get_dirty_submodules(&head.commit_id))
            .unwrap_or_default();
    }

    /// Extract selection from log panel and update change details panel
    fn sync_head_output(&mut self) {
        self.head = self.log_panel.head.clone();
        self.refresh_head_output();
    }

    /// Refesh the diff of the currently selected change
    fn refresh_head_output(&mut self) {
        // If the key matches, then we can use the cached value.
        // This is not entierly true. A reconfiguration of jj could
        // generate different output for some keys. We probably need
        // a forced cache clear function.

        // TODO use shared function to build key, so width can be cleared if not needed
        let inner_width = self.head_panel.columns() as usize;
        let key = CommitShowKey::new(self.head.clone(), self.diff_format.clone(), inner_width);
        let _new_content = self.commit_show_cache.get_or_insert(&key, || {
            Self::compute_head_content(inner_width, &self.head, &self.diff_format)
        });

        let content_changed = self.head_key != key;

        // Only update if content actually changed to prevent scroll jumping
        if content_changed {
            self.head_key = key;
            self.head_panel.scroll_to(0);
        }
    }

    //
    // Cache related
    //

    /// Mark all active elements as dirty, which will trigger a cache
    /// update next time they are requested.
    fn mark_cache_as_dirty(&mut self) {
        self.commit_show_cache.mark_dirty();
    }

    /// Get the list of active commits from the log panel, and mark
    /// the changes there as active. For non-active changes, keep at most
    /// one commit.
    fn update_cache_active_commits(&mut self) {
        let key = CommitShowKey::new(
            self.head.clone(),
            self.diff_format.clone(),
            self.head_panel.columns() as usize,
        );
        let active_heads = self.log_panel.log_heads();
        self.commit_show_cache.set_active(active_heads, &key);
    }

    /// Extract head content from commander.get_commit_show
    /// Wraps it in a cache value before returning it.
    fn compute_head_content(
        inner_width: usize,
        head: &Head,
        diff_format: &DiffFormat,
    ) -> CommitShowValue {
        // Call jj show
        let commit_id = &head.commit_id;
        let mut commander = new_commander();
        commander.limit_width(inner_width);
        let head_output = commander
            .get_commit_show(commit_id, diff_format, true)
            .map(|text| tabs_to_spaces(&text));
        // Format output as string
        let mut output = match head_output {
            Ok(head_output) => head_output,
            Err(err) => err.to_string(),
        };

        // jj carries gitlinks but never interprets them, so `jj show` renders a
        // submodule bump as a one-line text edit. Append what actually changed.
        if let Some(section) = Self::submodule_section(&commander, head) {
            output.push_str(&section);
        }

        // Build value used by cache and return it
        let key = CommitShowKey::new(head.clone(), diff_format.clone(), inner_width);
        CommitShowValue::new(key, output)
    }

    /// Render the submodule pointer changes this revision makes, if any, as a
    /// section to append to the details panel.
    ///
    /// Returns `None` when the repo has no submodules (the common case, and a
    /// single filesystem check) or when this revision touches none of them.
    fn submodule_section(commander: &Commander, head: &Head) -> Option<String> {
        if !commander.has_submodules() {
            return None;
        }

        // Diff against the first parent: a merge's gitlink is compared to the
        // branch it continues, which is the same convention `jj show` uses for
        // file contents.
        let parent = commander
            .get_commit_parents(&head.commit_id)
            .ok()
            .and_then(|parents| parents.into_iter().next());
        let changes = commander
            .get_submodule_changes(&head.commit_id, parent.as_ref())
            .ok()?;
        if changes.is_empty() {
            return None;
        }

        let mut section = String::from("\n");
        for change in &changes {
            section.push_str(&format!("\nSubmodule {}:\n", change.path));
            match (&change.from, &change.to) {
                (Some(from), Some(to)) => {
                    let arrow = if change.reversed { "←" } else { "→" };
                    section.push_str(&format!(
                        "    {} {arrow} {}\n",
                        short_commit(from),
                        short_commit(to)
                    ));
                }
                (None, Some(to)) => {
                    section.push_str(&format!("    added at {}\n", short_commit(to)));
                }
                (Some(from), None) => {
                    section.push_str(&format!("    removed (was {})\n", short_commit(from)));
                }
                (None, None) => {}
            }

            if change.commits.is_empty() {
                // No range to show: either the submodule is not checked out, or
                // the objects were never fetched. Say so rather than leaving the
                // reader to wonder whether the bump was empty.
                if change.from.is_some() && change.to.is_some() {
                    section.push_str("    (commits unavailable — submodule not checked out?)\n");
                }
            } else {
                const SHOWN: usize = 10;
                if change.reversed {
                    section.push_str("    rolled back over:\n");
                }
                for line in change.commits.iter().take(SHOWN) {
                    section.push_str(&format!("      {line}\n"));
                }
                if change.commits.len() > SHOWN {
                    section.push_str("      …\n");
                }
            }
        }
        Some(section)
    }
}

/**
# Event handling
Event handling happens in [`LogTab::handle_event`]. Over time, this has
caused it to grow to a very long match with many arms. The size makes it hard
to see what is going on, and the indentation is very deep.

To fix this, we have begun a new code pattern, were the match arm simply
calls a function. Most actions are two step operations, first create a dialog
, then execcute some command. This is reflected in two functions located near
each other in code:
* `handle_<action>` - Set up the dialog and show it.
* `execute_<action>` - Perform some action after the dialog closed.
*/
impl<'a> LogTab<'a> {
    fn handle_new(&mut self, no_edit: bool) -> Result<ComponentInputResult> {
        let mark_count = self.log_panel.marked_heads.len();
        let before_count = self.log_panel.before_marked_heads.len();
        // A splice is more consequential than appending a leaf — it re-parents
        // the before-anchors — so it says so explicitly rather than reusing the
        // plain "new change" wording.
        let text = if before_count > 0 {
            let after = if mark_count > 0 {
                format!("{mark_count} marked parents")
            } else {
                format!("the selected change ({})", self.head.change_id.as_str())
            };
            Text::from(vec![Line::from(format!(
                "Are you sure you want to insert a new change after {after} and before {before_count} marked changes?"
            ))])
            .fg(Color::default())
        } else if mark_count > 0 {
            Text::from(vec![Line::from(format!(
                "Are you sure you want to create a new change with {mark_count} marked parents?"
            ))])
            .fg(Color::default())
        } else {
            Text::from(vec![
                Line::from("Are you sure you want to create a new change?"),
                Line::from(format!("New parent: {}", self.head.change_id.as_str())),
            ])
            .fg(Color::default())
        };
        // `N` leaves `@` where it is, which is the whole difference from `n` —
        // say so, since the graph afterwards looks the same either way apart
        // from where `@` sits.
        let text = if no_edit {
            let mut lines = text.lines;
            lines.push(Line::from("@ stays where it is.").fg(Color::DarkGray));
            Text::from(lines)
        } else {
            text
        };
        self.popup = ConfirmDialogState::new(
            NEW_POPUP_ID,
            Span::styled(" New ", Style::new().bold().cyan()),
            text,
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        self.no_edit_new = no_edit;
        Ok(ComponentInputResult::Handled)
    }

    /// Execute new command, after self.popup returned.
    ///
    /// jj refuses some of these outright — inserting before an immutable
    /// commit, say — so failures are surfaced as a popup rather than
    /// propagated: an `Err` out of `update()` reaches the top level and tears
    /// the TUI down, which is far too much for a rejected command.
    fn execute_new(&mut self) -> Result<Option<AppAction>> {
        let before = self.log_panel.extract_and_clear_before_marks();
        let commit_ids = self.log_panel.extract_and_clear_head_marks();

        let no_edit = self.no_edit_new;
        self.no_edit_new = false;

        // The after-anchors: the marked parents, or the selected change.
        let after = if commit_ids.is_empty() {
            vec![self.head.commit_id.clone()]
        } else {
            commit_ids
        };

        let outcome = if before.is_empty() && !no_edit {
            // The plain leaf case: `jj new` onto the parents, moving `@` into
            // the change, since starting work there is the point of `n`.
            new_commander()
                .run_new(after.iter().map(CommitId::as_str))
                .and_then(|()| new_commander().get_current_head())
        } else {
            // Either a splice (before-anchors) or `N` (leave `@` alone). Both
            // are `jj new --no-edit`, which reports the change it created so
            // the cursor can be put on it without `@` moving.
            new_commander().run_new_insert(&after, &before)
        };

        match outcome {
            Err(err) => {
                // The marks are already consumed, and the graph is untouched;
                // leave the cursor where it is so the user can retry.
                return Ok(Some(AppAction::SetPopup(Some(Box::new(
                    MessagePopup::new("New", format!("{err:#}")),
                )))));
            }
            Ok(head) => self.set_head(head),
        }

        Ok(Some(AppAction::ChangeHead(self.head.clone())))
    }

    /// Take the current pick of a "pick up, put down" gesture: the marked
    /// changes, or the change under the cursor if none are marked. Sorted for
    /// a stable order, since mark storage is an unordered set.
    fn take_picked_commits(&mut self) -> Vec<CommitId> {
        let mut marks = self.log_panel.extract_and_clear_head_marks();
        if marks.is_empty() {
            return vec![self.head.commit_id.clone()];
        }
        marks.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        marks
    }

    fn message_popup(title: &'static str, message: &'static str) -> Result<ComponentInputResult> {
        Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
            Some(Box::new(MessagePopup::new(title, message))),
        )))
    }

    /// Report a jj command's failure in a popup.
    ///
    /// jj refuses plenty of things for good reasons — undoing a merge
    /// operation, editing an immutable commit — and its message usually says
    /// what to do instead. Propagating the error would take that message out of
    /// the top level and tear the TUI down with it, which is far too much for a
    /// command that simply declined to run.
    fn command_error_popup(
        title: &'static str,
        err: impl std::fmt::Display,
    ) -> ComponentInputResult {
        ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(MessagePopup::new(
            title,
            format!("{err:#}"),
        )))))
    }

    /// Pick up change(s) to rebase; the parent set is edited next. For a
    /// single source, its current parents are pre-seeded as marks so that
    /// toggling a mark visibly adds/removes a future parent edge.
    ///
    /// A before-anchor set before the key switches the gesture to insert mode
    /// (`-A`/`-B`). There the parents are not pre-seeded: the after-anchors are
    /// an absolute set picked during the phase, not an edit of today's parents,
    /// so seeding them would silently add anchors the user never picked.
    fn start_rebase(&mut self) -> Result<()> {
        let sources = self.take_picked_commits();
        // Read *after* taking the sources: the pre-key pick consumes the
        // after-marks, but before-marks are left alone and carry into the phase.
        let inserting = !self.log_panel.before_marked_heads.is_empty();

        let mut original_parents = Vec::new();
        if let [source] = sources.as_slice()
            && !inserting
        {
            original_parents = new_commander().get_commit_parents(source)?;
            self.log_panel.marked_heads = original_parents.iter().cloned().collect();
        }
        self.log_panel.marks_are_parents = true;

        self.pick_state = PickState::RebaseDestinations {
            sources,
            original_parents,
            // Descendants come along by default (`jj rebase -s`): moving a
            // change usually means moving the work built on top of it, and
            // leaving them behind re-parents them onto the change's old parents,
            // which is the surprising outcome. `r` switches to this-change-only.
            include_descendants: true,
        };
        self.update_pick_title();
        // Bake the parent glyphs into the graph
        self.refresh_log_output();
        Ok(())
    }

    /// Toggle whether the rebase gesture brings descendants along
    /// (`jj rebase -s` vs `-r`).
    fn toggle_rebase_descendants(&mut self) {
        if let PickState::RebaseDestinations {
            include_descendants,
            ..
        } = &mut self.pick_state
        {
            *include_descendants = !*include_descendants;
            self.update_pick_title();
        }
    }

    /// Pick up change(s) whose whole branch should be rebased; the
    /// destination(s) are picked next (`jj rebase -b`).
    fn start_branch_rebase(&mut self) {
        let sources = self.take_picked_commits();
        self.log_panel.marks_are_parents = true;
        self.pick_state = PickState::BranchRebaseDestinations { sources };
        self.update_pick_title();
    }

    /// Rebase `sources` onto `targets` with the given source mode, following
    /// the first source with the cursor afterwards.
    fn execute_rebase(
        &mut self,
        src_mode: &str,
        sources: Vec<CommitId>,
        targets: Vec<CommitId>,
    ) -> Result<ComponentInputResult> {
        // Resolve the first source before the rebase rewrites it, so the
        // cursor can follow the moved change afterwards
        let follow = sources
            .first()
            .and_then(|source| new_commander().get_head(source.as_str()).ok());

        if let Err(err) = new_commander().run_rebase(src_mode, &sources, "-d", &targets) {
            return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new("Rebase", format!("{err:#}")))),
            )));
        }

        let follow = follow.unwrap_or_else(|| self.head.clone());
        self.set_head(new_commander().get_head_latest(&follow)?);
        Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
            vec![
                AppAction::ChangeHead(self.head.clone()),
                AppAction::SetStatusMessage("Rebased | u: undo".to_owned()),
            ],
        )))
    }

    /// Pick up change(s) to squash; the destination is picked next, with the
    /// cursor pre-placed on the source's parent as the natural default.
    fn start_squash(&mut self, ignore_immutable: bool) {
        let sources = self.take_picked_commits();
        if let [source] = sources.as_slice()
            && let Ok(parent) = new_commander().get_commit_parent(source)
        {
            self.set_head(parent);
        }
        self.pick_state = PickState::SquashDestination {
            sources,
            ignore_immutable,
            interactive: false,
        };
        self.update_pick_title();
    }

    /// The squash key toggles hunk-by-hunk mode mid-gesture.
    fn toggle_squash_interactive(&mut self) {
        if let PickState::SquashDestination { interactive, .. } = &mut self.pick_state {
            *interactive = !*interactive;
            self.update_pick_title();
        }
    }

    /// Pick up the change to diff-edit; the base is picked next. The cursor
    /// stays on the change, so an immediate Enter edits that revision's own
    /// diff against all its parents (`jj diffedit -r`). Moving the cursor, or
    /// marking a revision, edits against that base instead. Diffedit operates
    /// on a single revision, so the picked-up target is always the change
    /// under the cursor, not a mark set.
    fn start_diffedit(&mut self) -> Result<ComponentInputResult> {
        if self.head.immutable {
            return Self::message_popup(
                "Diff edit",
                "The change cannot be edited because it is immutable.",
            );
        }
        // Deliberately *not* checking emptiness here. "Empty" means empty
        // against this revision's own parents, which is only the `-r` case; the
        // whole point of the gesture is that a different base can be picked,
        // and against that base an "empty" revision may well have a diff. The
        // check happens once the base is known (see [Self::advance_pick]).
        let target = self.head.commit_id.clone();
        self.pick_state = PickState::DiffEditFrom { target };
        self.update_pick_title();
        Ok(ComponentInputResult::Handled)
    }

    /// Move `moving` so it sits between the anchors (`jj rebase -r -A -B`),
    /// following the first moved change with the cursor afterwards.
    ///
    /// Several changes move as one set, keeping their order relative to each
    /// other; the cursor follows the first, matching [Self::execute_rebase].
    fn execute_rebase_insert(
        &mut self,
        moving: &[CommitId],
        after: &[CommitId],
        before: &[CommitId],
    ) -> Result<ComponentInputResult> {
        let Some(follow) = moving.first() else {
            return Ok(ComponentInputResult::Handled);
        };
        // Resolve before the rebase rewrites the moved change, so the cursor
        // can follow it afterwards
        let landed = new_commander()
            .get_head(follow.as_str())
            .and_then(|moving_head| {
                new_commander().run_rebase_insert(moving, after, before)?;
                new_commander().get_head_latest(&moving_head)
            });
        match landed {
            Err(err) => Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new("Insert", format!("{err:#}")))),
            ))),
            Ok(landed) => {
                self.set_head(landed);
                Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::ChangeHead(self.head.clone()),
                        AppAction::SetStatusMessage("Inserted | u: undo".to_owned()),
                    ],
                )))
            }
        }
    }

    /// End the gesture but keep the marks, for a command that takes over the
    /// phase's pick instead of completing it (see the new-sibling arm in
    /// [Self::input]). Leaving `pick_state` live would strand the user in a
    /// phase whose marks another command already consumed.
    ///
    /// `marks_are_parents` is cleared so the marks stop rendering as pending
    /// parent edges (`✚`); they are an ordinary mark set again, which is what
    /// the command reading them expects.
    fn end_pick_keeping_marks(&mut self) {
        self.log_panel.marks_are_parents = false;
        self.pick_state = PickState::Idle;
        self.log_panel.title_override = None;
        self.refresh_log_output();
    }

    fn cancel_pick(&mut self) {
        self.log_panel.extract_and_clear_head_marks();
        self.log_panel.extract_and_clear_before_marks();
        self.log_panel.marks_are_parents = false;
        self.pick_state = PickState::Idle;
        self.log_panel.title_override = None;
        // Un-bake any mark glyphs from the graph
        self.refresh_log_output();
    }

    /// Move the selection to the next (`forward`) or previous search match,
    /// wrapping around, and sync the details panel. No-op if no search is
    /// active or nothing matches.
    fn navigate_search(&mut self, forward: bool) {
        let count = self.log_panel.select_adjacent_match(forward);
        if count > 0 {
            self.sync_head_output();
        }
    }

    fn update_pick_title(&mut self) {
        let hint = match &self.pick_state {
            PickState::Idle => None,
            PickState::RebaseDestinations {
                sources,
                include_descendants,
                ..
            } => {
                let what_moves = if *include_descendants {
                    "+ descendants".to_owned()
                } else if sources.len() > 1 {
                    format!("{} changes", sources.len())
                } else {
                    "this change".to_owned()
                };
                // Before-anchors change what Enter will do, so the hint says so
                // rather than leaving the mode switch invisible.
                let before_count = self.log_panel.before_marked_heads.len();
                if before_count > 0 {
                    Some(format!(
                        " Insert [{what_moves}] (r: switch): before {before_count} marked (⌄); space picks what it goes after (✓); enter: apply, esc: cancel "
                    ))
                } else {
                    Some(format!(
                        " Rebase [{what_moves}] (r: switch): space toggles parents (✚); i: go before; n: new sibling instead; enter: apply, or onto cursor if untouched; esc: cancel "
                    ))
                }
            }
            PickState::BranchRebaseDestinations { .. } => Some(
                " Branch rebase: pick destination(s) (space: mark several, enter: apply, esc: cancel) "
                    .to_owned(),
            ),
            PickState::SquashDestination { interactive, .. } => {
                let what_moves = if *interactive {
                    "chosen hunks"
                } else {
                    "everything"
                };
                Some(format!(
                    " Squash [{what_moves}] (s: switch): pick destination (enter: confirm, esc: cancel) "
                ))
            }
            PickState::DiffEditFrom { .. } => Some(
                " Diff edit: enter: this revision's own diff, or pick a base to edit against (esc: cancel) "
                    .to_owned(),
            ),
        };
        self.log_panel.title_override = hint;
    }

    /// Advance the pick gesture on Enter: take the current pick and either
    /// move to the next phase or execute.
    fn advance_pick(&mut self) -> Result<ComponentInputResult> {
        match self.pick_state.clone() {
            PickState::Idle => Ok(ComponentInputResult::Handled),
            PickState::RebaseDestinations {
                sources,
                original_parents,
                include_descendants,
            } => {
                // Before-anchors switch the gesture wholesale from "-d onto
                // this parent set" to "-A/-B splice". The parent-edit logic
                // below, and its no-op-means-onto-the-cursor fallback, do not
                // apply: in insert mode the after-anchors are an absolute set,
                // and an empty one is meaningful (insert with only -B).
                if !self.log_panel.before_marked_heads.is_empty() {
                    let before = self.log_panel.extract_and_clear_before_marks();
                    let after = self.log_panel.extract_and_clear_head_marks();
                    self.log_panel.marks_are_parents = false;
                    self.pick_state = PickState::Idle;
                    self.log_panel.title_override = None;

                    return self.execute_rebase_insert(&sources, &after, &before);
                }

                // The marks are the edited parent set. Don't consume them
                // yet: validation failures keep the phase (and glyphs) alive.
                let marks = &self.log_panel.marked_heads;
                let unchanged = marks.len() == original_parents.len()
                    && original_parents.iter().all(|parent| marks.contains(parent));

                let targets: Vec<CommitId> = if unchanged {
                    // No-op parent edit; fall back to the plain "move it
                    // onto the cursor" gesture
                    vec![self.head.commit_id.clone()]
                } else {
                    // Surviving parents keep their original order; newly
                    // added ones follow (sorted — mark storage is unordered)
                    let mut added: Vec<CommitId> = marks
                        .iter()
                        .filter(|mark| !original_parents.contains(mark))
                        .cloned()
                        .collect();
                    added.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                    original_parents
                        .iter()
                        .filter(|parent| marks.contains(parent))
                        .cloned()
                        .chain(added)
                        .collect()
                };

                if targets.is_empty() {
                    return Self::message_popup("Rebase", "The parent set cannot be empty.");
                }
                if targets.iter().any(|target| sources.contains(target)) {
                    return Self::message_popup(
                        "Rebase",
                        "A picked-up change cannot become its own parent.",
                    );
                }

                self.log_panel.extract_and_clear_head_marks();
                self.log_panel.marks_are_parents = false;
                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;
                let src_mode = if include_descendants { "-s" } else { "-r" };
                self.execute_rebase(src_mode, sources, targets)
            }
            PickState::BranchRebaseDestinations { sources } => {
                let targets = self.take_picked_commits();
                if targets.iter().any(|target| sources.contains(target)) {
                    return Self::message_popup(
                        "Rebase",
                        "The destination is one of the picked-up changes.",
                    );
                }
                self.log_panel.marks_are_parents = false;
                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;
                self.execute_rebase("-b", sources, targets)
            }
            PickState::SquashDestination {
                sources,
                ignore_immutable,
                interactive,
            } => {
                let targets = self.take_picked_commits();
                let [target] = targets.as_slice() else {
                    return Self::message_popup("Squash", "Pick a single destination change.");
                };
                if sources.contains(target) {
                    return Self::message_popup("Squash", "Cannot squash a change into itself.");
                }
                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;

                // Resolve the destination before squashing rewrites it, so the
                // selection can follow it afterwards
                let target_head = new_commander().get_head(target.as_str())?;

                if interactive {
                    // Put the cursor on the destination now; the post-command
                    // refresh follows it through the rewrite
                    self.set_head(target_head);
                    return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                        vec![
                            AppAction::ChangeHead(self.head.clone()),
                            AppAction::RunInteractive(Commander::squash_interactive_command(
                                &sources,
                                target.as_str(),
                                ignore_immutable,
                            )),
                        ],
                    )));
                }

                if let Err(err) =
                    new_commander().run_squash_into(&sources, target.as_str(), ignore_immutable)
                {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new("Squash", format!("{err:#}")))),
                    )));
                }
                self.set_head(new_commander().get_head_latest(&target_head)?);
                Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::ChangeHead(self.head.clone()),
                        AppAction::SetStatusMessage("Squashed | u: undo".to_owned()),
                    ],
                )))
            }
            PickState::DiffEditFrom { target } => {
                // The base is the marked revision, or the change under the
                // cursor if none are marked. Landing on the target itself means
                // "just edit this revision", i.e. `-r` with no base: a revision
                // cannot be diffed against itself, and `--from` against a single
                // parent would misrepresent a merge.
                let marks = self.log_panel.marked_heads.len();
                if marks > 1 {
                    return Self::message_popup(
                        "Diff edit",
                        "Diff edit needs a single base. Pick one revision to edit against.",
                    );
                }
                let picked = self.take_picked_commits();
                let [from] = picked.as_slice() else {
                    return Self::message_popup(
                        "Diff edit",
                        "Pick a single revision to edit against.",
                    );
                };
                let from = (*from != target).then(|| from.clone());

                // Only the no-base case (`-r`) needs the emptiness guard: there
                // the diff really is the revision against its own parents, and
                // jj would open the editor on nothing. With a base picked, an
                // "empty" revision can still differ from it, which is exactly
                // why the check cannot happen before the base is known.
                if from.is_none() && new_commander().check_revision_empty(target.as_str())? {
                    return Self::message_popup(
                        "Diff edit",
                        "The change is empty against its own parents. Pick another revision to edit against.",
                    );
                }

                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;

                // Put the cursor on the edited change now; the post-command
                // refresh follows it through the rewrite
                let target_head = new_commander().get_head(target.as_str())?;
                self.set_head(target_head);
                let command = match from {
                    Some(from) => {
                        Commander::diffedit_from_interactive_command(from.as_str(), target.as_str())
                    }
                    None => Commander::diffedit_interactive_command(target.as_str()),
                };
                Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::ChangeHead(self.head.clone()),
                        AppAction::RunInteractive(command),
                    ],
                )))
            }
        }
    }

    fn handle_abandon(&mut self) -> Result<ComponentInputResult> {
        // Cannot abandon immutable changes
        if self.head.immutable {
            return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new(
                    "Abandon",
                    "The change cannot be abandoned because it is immutable.",
                ))),
            )));
        }

        // Ask for confirmation by launching a popup
        let mark_count = self.log_panel.marked_heads.len();
        let text = if mark_count > 0 {
            Text::from(vec![Line::from(format!(
                "Are you sure you want to abandon {} marked changes?",
                mark_count
            ))])
            .fg(Color::default())
        } else {
            Text::from(vec![
                Line::from("Are you sure you want to abandon this change?"),
                Line::from(format!("Change: {}", self.head.change_id.as_str())),
            ])
            .fg(Color::default())
        };
        self.popup = ConfirmDialogState::new(
            ABANDON_POPUP_ID,
            Span::styled(" Abandon ", Style::new().bold().cyan()),
            text,
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        Ok(ComponentInputResult::Handled)
    }

    // Execute abandon command, after self.popup returned
    fn execute_abandon(&mut self) -> Result<Option<AppAction>> {
        // If none marked, mark current head
        if self.log_panel.marked_heads.is_empty() {
            self.log_panel.toggle_head_mark();
        }
        // Move selection to parent until it is no longer inside the marked commits
        let old_selection = self.head.clone();
        let mut selection = self.head.clone();
        while self.log_panel.is_head_marked(&selection) {
            selection = new_commander().get_commit_parent(&selection.commit_id)?;
        }
        // Abandon marked commmits
        let commit_id_list = self.log_panel.extract_and_clear_head_marks();
        // Runs from the confirm-dialog path in `update()`, where an `Err` would
        // reach the top level and tear the TUI down over a refused command.
        if let Err(err) = new_commander().run_abandon(&commit_id_list) {
            return Ok(Some(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new("Abandon", format!("{err:#}")),
            )))));
        }
        // Update selection to latest version, in case abandon triggered a rebase.
        let new_selection = new_commander().get_head_latest(&selection)?;
        // Update log panel and diff panel
        self.set_head(new_selection.clone());
        // If selection was moved, tell the application
        if new_selection != old_selection {
            Ok(Some(AppAction::ChangeHead(self.head.clone())))
        } else {
            Ok(None)
        }
    }

    /// Apply a configured description transform to the marked changes, or to
    /// the selected change if none are marked.
    ///
    /// This rewrites descriptions without confirmation, since it is meant to be
    /// a single keystroke; `u` undoes it. Nothing is written until every change
    /// has passed the immutability check and every template has rendered, so a
    /// batch either applies completely or not at all.
    /// Highlight the revisions touching a path the user typed.
    ///
    /// Matched with jj's default `prefix-glob:` kind, so a directory marks
    /// everything beneath it and glob characters work -- what someone typing a
    /// path into a filter box means. The files tab, which hands over a path jj
    /// named rather than one the user typed, matches exactly instead: see
    /// [Self::apply_exact_file_filter].
    ///
    /// An empty path clears the highlight, so the filter bar can be dismissed by
    /// submitting nothing.
    pub fn apply_file_filter(&mut self, path: &str) -> ComponentInputResult {
        self.apply_path_highlight(path, Commander::files_revset)
    }

    /// Highlight the revisions touching any of exactly these `paths`.
    ///
    /// Used by the files tab's handoff: the paths came from jj's own diff
    /// summary, so matching them exactly is unambiguous, where prefix matching
    /// would also mark revisions touching unrelated files that merely share a
    /// prefix.
    pub fn apply_exact_file_filter(&mut self, paths: &[String]) -> ComponentInputResult {
        let paths: BTreeSet<String> = paths.iter().cloned().collect();
        let Some(revset) = Commander::any_of_files_revset(&paths) else {
            self.clear_highlight();
            return ComponentInputResult::Handled;
        };

        // Label with the path when there is one, since that is what the user
        // pointed at; a set is summarized by count, the revset being unreadable
        // past a couple of terms.
        let label = match paths.len() {
            1 => paths.iter().next().expect("one path").clone(),
            n => format!("{n} files"),
        };

        let outcome = self.log_panel.set_highlight(&revset, Some(label));
        self.report_highlight_outcome(outcome)
    }

    /// Mark every revision that touches any file the marked revisions touch —
    /// or the selected revision's files, if none are marked.
    ///
    /// The source revisions match themselves, since their own files are in the
    /// set; that is wanted, as it shows the group being compared against.
    fn apply_related_file_highlight(&mut self) -> Result<ComponentInputResult> {
        // Peek at the marks rather than consuming them: this highlights, it does
        // not act on the revisions, and clearing them would make the repeated
        // "mark some, look, mark more" loop tedious.
        let mut sources: Vec<CommitId> = self.log_panel.marked_heads.iter().cloned().collect();
        if sources.is_empty() {
            sources.push(self.head.commit_id.clone());
        }
        sources.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        let paths = new_commander().get_touched_paths(&sources)?;
        let Some(revset) = Commander::any_of_files_revset(&paths) else {
            return Self::message_popup(
                "Related revisions",
                "The revision touches no files, so there is nothing to match against.",
            );
        };

        // Label with a summary rather than the generated expression: one file
        // per term, the revset is unreadable past a couple of paths.
        let label = match (sources.len(), paths.len()) {
            (1, 1) => "files of 1 revision (1 file)".to_owned(),
            (1, files) => format!("files of 1 revision ({files} files)"),
            (revs, 1) => format!("files of {revs} revisions (1 file)"),
            (revs, files) => format!("files of {revs} revisions ({files} files)"),
        };

        let outcome = self.log_panel.set_highlight(&revset, Some(label));
        Ok(self.report_highlight_outcome(outcome))
    }

    /// Shared body of the two file-filter entry points: build a revset from the
    /// path with `to_revset`, then highlight it, labelled with the path rather
    /// than the generated `files(...)` expression.
    fn apply_path_highlight(
        &mut self,
        path: &str,
        to_revset: impl Fn(&str) -> String,
    ) -> ComponentInputResult {
        let path = path.trim();
        if path.is_empty() {
            self.clear_highlight();
            return ComponentInputResult::Handled;
        }

        let outcome = self
            .log_panel
            .set_highlight(&to_revset(path), Some(path.to_owned()));
        self.report_highlight_outcome(outcome)
    }

    /// Turn a [HighlightOutcome] into user feedback.
    ///
    /// The empty-gutter cases are the ones worth distinguishing: revisions match
    /// but the log's revset hides them all (offer to widen it), nothing matches at
    /// all (check the expression), or jj rejected the revset (show its
    /// diagnostic, which is multi-line and genuinely useful).
    fn report_highlight_outcome(&mut self, outcome: HighlightOutcome) -> ComponentInputResult {
        self.pending_widen = None;

        let subject = self
            .log_panel
            .highlight_revset()
            .unwrap_or_default()
            .to_owned();

        match outcome {
            HighlightOutcome::Cleared => ComponentInputResult::Handled,
            HighlightOutcome::Applied { matching, visible } if visible > 0 => {
                ComponentInputResult::HandledAction(AppAction::SetStatusMessage(format!(
                    "Marking {matching} revision{} ({visible} in view)",
                    if matching == 1 { "" } else { "s" }
                )))
            }
            HighlightOutcome::Applied { matching, .. } => {
                // Nothing to show: the revset selects revisions, but the log's own
                // revset excludes every one of them. Offer to widen rather than
                // silently changing what the user is looking at.
                self.pending_widen = Some(subject);
                let (plural, verb) = if matching == 1 {
                    ("", "matches")
                } else {
                    ("s", "match")
                };
                ComponentInputResult::HandledAction(AppAction::SetStatusMessage(format!(
                    "{matching} revision{plural} {verb}, none in this revset — \
                     {WIDEN_KEY_LABEL} to show them",
                )))
            }
            HighlightOutcome::NoneMatching => ComponentInputResult::HandledAction(
                AppAction::SetStatusMessage("No revisions match".to_owned()),
            ),
            HighlightOutcome::Failed(err) => {
                // The highlight is left inactive by `set_highlight`'s error path in
                // the sense that nothing is marked; clear it so the title does not
                // advertise a revset that never worked.
                self.clear_highlight();
                ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(
                    MessagePopup::new("Highlight revset", err),
                ))))
            }
        }
    }

    /// Clear the highlight and any widen offer that went with it.
    ///
    /// The two always go together: an offer to widen the revset makes no sense once
    /// there is no marking left to widen towards.
    fn clear_highlight(&mut self) {
        self.log_panel.clear_highlight();
        self.pending_widen = None;
    }

    /// Take up the pending "widen the log's revset" offer: show exactly the
    /// revisions the highlight selects.
    ///
    /// Reuses the ordinary revset path, so `Ctrl+r` afterwards shows and edits the
    /// widened revset like any other.
    fn accept_widen(&mut self) -> ComponentInputResult {
        let Some(revset) = self.pending_widen.take() else {
            return ComponentInputResult::NotHandled;
        };
        self.log_panel.log_revset = Some(revset);
        self.refresh_log_output();
        ComponentInputResult::Handled
    }

    /// Reveal the revisions behind the "(elided revisions)" row the cursor is
    /// parked on, by widening the log's revset to include that one gap.
    ///
    /// The added term is the range between the placeholder's owner and its
    /// nearest *shown* ancestors:
    ///
    /// ```text
    /// <current> | (heads(::N ~ N & (<current>))::N)
    /// ```
    ///
    /// Scoped to the one gap rather than a blanket `connected(<current>)`,
    /// which would fill in every elision in the graph at once — on a log with
    /// several branches that is a much bigger change to the view than clicking
    /// one placeholder asks for.
    fn expand_elided(&mut self) -> Result<ComponentInputResult> {
        let Some(owner) = self.log_panel.elided_owner_selection() else {
            return Ok(ComponentInputResult::NotHandled);
        };

        // With no explicit revset the log is showing jj's default, so that is
        // what has to be widened.
        let current = match self.log_panel.log_revset.clone() {
            Some(revset) => revset,
            None => match new_commander().get_default_log_revset() {
                Ok(revset) => revset,
                Err(err) => {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            "Expand",
                            format!("Could not read the log's default revset: {err}"),
                        ))),
                    )));
                }
            },
        };

        let owner_id = owner.commit_id.as_str();
        let expanded =
            format!("({current}) | (heads(::{owner_id} ~ {owner_id} & ({current}))::{owner_id})");

        self.log_panel.log_revset = Some(expanded);
        // Land on the revision the gap belonged to, so the newly revealed
        // ancestors appear just below the cursor rather than somewhere off
        // screen.
        self.set_head(owner);
        self.refresh_log_output();
        Ok(ComponentInputResult::HandledAction(
            AppAction::SetStatusMessage(
                "Expanded elided revisions | Ctrl+r: edit revset".to_owned(),
            ),
        ))
    }

    fn handle_transform_description(&mut self, index: usize) -> Result<ComponentInputResult> {
        let Some(transform) = get_env()
            .jj_config
            .description_transforms()
            .get(index)
            .cloned()
        else {
            return Ok(ComponentInputResult::NotHandled);
        };

        let commit_ids = if self.log_panel.marked_heads.is_empty() {
            vec![self.head.commit_id.clone()]
        } else {
            let mut marks: Vec<CommitId> = self.log_panel.marked_heads.iter().cloned().collect();
            marks.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            marks
        };

        let commander = new_commander();
        for commit_id in &commit_ids {
            if commander.check_revision_immutable(commit_id.as_str())? {
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(MessagePopup::new(
                        "Transform description",
                        format!(
                            "The description of {} cannot be changed because it is immutable.",
                            commit_id.as_str()
                        ),
                    ))),
                )));
            }
        }

        // Render every description first: a template error partway through a
        // batch would otherwise leave it half-rewritten.
        let mut rewrites = Vec::with_capacity(commit_ids.len());
        for commit_id in &commit_ids {
            let description = commander.get_commit_description(commit_id)?;
            match transform.apply(&description) {
                Ok(new_description) => rewrites.push((commit_id, new_description)),
                Err(err) => {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            "Transform description",
                            format!("{err:#}"),
                        ))),
                    )));
                }
            }
        }

        for (commit_id, new_description) in &rewrites {
            commander.run_describe(commit_id.as_str(), new_description)?;
        }

        self.log_panel.extract_and_clear_head_marks();
        self.set_head(new_commander().get_head_latest(&self.head)?);

        let count = commit_ids.len();
        let message = if count == 1 {
            format!(
                "Applied \"{}\" to the description | u: undo",
                transform.name
            )
        } else {
            format!(
                "Applied \"{}\" to {count} descriptions | u: undo",
                transform.name
            )
        };
        Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
            vec![
                AppAction::RefreshTab(),
                AppAction::SetStatusMessage(message),
            ],
        )))
    }

    fn handle_resolve(&mut self, keep_destination: bool) -> Result<ComponentInputResult> {
        if self.head.immutable {
            return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new(
                    "Resolve",
                    "The conflicts cannot be resolved because the change is immutable.",
                ))),
            )));
        }

        let conflicts = new_commander().get_conflicts(&self.head.commit_id)?;
        if conflicts.is_empty() {
            return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new(
                    "Resolve",
                    "The change has no conflicts to resolve.",
                ))),
            )));
        }

        let side = if keep_destination {
            "the destination side (e.g. the rebase or squash destination)"
        } else {
            "the moved side (the rebased or squashed revision's version)"
        };
        let mut lines = vec![
            Line::from(format!(
                "Resolve {} conflicted file(s) in favor of {side}?",
                conflicts.len()
            )),
            Line::from(format!("Change: {}", self.head.change_id.as_str())),
        ];
        const MAX_LISTED_CONFLICTS: usize = 8;
        for conflict in conflicts.iter().take(MAX_LISTED_CONFLICTS) {
            lines.push(Line::from(format!("  {}", conflict.path)));
        }
        if conflicts.len() > MAX_LISTED_CONFLICTS {
            lines.push(Line::from(format!(
                "  ...and {} more",
                conflicts.len() - MAX_LISTED_CONFLICTS
            )));
        }

        self.popup = ConfirmDialogState::new(
            RESOLVE_POPUP_ID,
            Span::styled(" Resolve ", Style::new().bold().cyan()),
            Text::from(lines).fg(Color::default()),
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        self.resolve_keep_destination = keep_destination;
        Ok(ComponentInputResult::Handled)
    }

    // Execute resolve command, after self.popup returned
    fn execute_resolve(&mut self) -> Result<Option<AppAction>> {
        let side = if self.resolve_keep_destination {
            ConflictSide::Destination
        } else {
            ConflictSide::Source
        };
        if let Err(err) = new_commander().run_resolve(self.head.commit_id.as_str(), None, side) {
            return Ok(Some(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new("Resolve", err.to_string()),
            )))));
        }

        self.set_head(new_commander().get_head_latest(&self.head)?);
        Ok(Some(AppAction::Multiple(vec![
            AppAction::ChangeHead(self.head.clone()),
            AppAction::SetStatusMessage("Resolved conflicts | u: undo".to_owned()),
        ])))
    }

    fn handle_event(&mut self, log_tab_event: LogTabEvent) -> Result<ComponentInputResult> {
        // An "(elided revisions)" row is a placeholder, not a revision. While
        // the cursor is parked on one, `self.head` still names the revision it
        // hangs beneath — convenient for the details panel, but wrong to act
        // on: `d` there would describe a revision the user is not pointing at.
        // Allow only what makes sense on a placeholder and refuse the rest,
        // rather than guarding thirty arms individually.
        if self.log_panel.is_on_elided_row() && !elided_row_allows(log_tab_event) {
            return Self::message_popup(
                "Elided revisions",
                "This row stands for hidden revisions, not a change. Press Enter to reveal them, or move to a revision first.",
            );
        }

        match log_tab_event {
            LogTabEvent::ScrollDown
            | LogTabEvent::ScrollUp
            | LogTabEvent::ScrollDownHalf
            | LogTabEvent::ScrollUpHalf
            | LogTabEvent::ScrollToBottom
            | LogTabEvent::ScrollToTop
            | LogTabEvent::ToggleHeadMark
            | LogTabEvent::ToggleHeadBeforeMark => {
                self.log_panel.handle_event(log_tab_event)?;
                self.sync_head_output();
            }
            LogTabEvent::FocusCurrent => {
                self.set_head(new_commander().get_current_head()?);
            }
            LogTabEvent::ToggleDiffFormat => {
                self.diff_format = self.diff_format.get_next(self.config.diff_tool());
                self.refresh_head_output();
            }
            LogTabEvent::Refresh => {
                self.mark_cache_as_dirty();
                // Re-resolve the selected head: external jj activity may have
                // rewritten it (new commit id) or abandoned it entirely, and
                // refreshing only the graph would leave the details panel — and
                // every subsequent action on self.head — pointing at the old
                // commit. get_head_latest follows the change's evolution and
                // falls back to @ if the change is gone.
                self.set_head(new_commander().get_head_latest(&self.head)?);
                self.refresh_dirty_submodules();
            }

            LogTabEvent::Duplicate => {
                let _ = new_commander().run_duplicate(&self.head.change_id.to_string());
                self.refresh_log_output();
            }

            LogTabEvent::CreateNew { no_edit } => {
                return self.handle_new(no_edit);
            }
            LogTabEvent::Rebase => {
                self.start_rebase()?;
            }
            LogTabEvent::RebaseBranch => {
                self.start_branch_rebase();
            }
            LogTabEvent::Squash { ignore_immutable } => {
                self.start_squash(ignore_immutable);
            }
            LogTabEvent::Split => {
                if self.head.immutable {
                    return Self::message_popup(
                        "Split",
                        "The change cannot be split because it is immutable.",
                    );
                }
                // jj would happily "split" an empty change into two empty
                // changes without ever opening the editor — catch it here
                if new_commander().check_revision_empty(self.head.commit_id.as_str())? {
                    return Self::message_popup(
                        "Split",
                        "The change is empty; there is nothing to split.",
                    );
                }
                return Ok(ComponentInputResult::HandledAction(
                    AppAction::RunInteractive(Commander::split_interactive_command(
                        self.head.commit_id.as_str(),
                    )),
                ));
            }
            LogTabEvent::DiffEdit => {
                return self.start_diffedit();
            }
            LogTabEvent::EditChange { ignore_immutable } => {
                if self.head.immutable && !ignore_immutable {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            " Edit ",
                            "The change cannot be edited because it is immutable.",
                        ))),
                    )));
                }

                // No confirmation: with `n` no longer moving @, editing into
                // a change is a frequent, cheap, and undoable action
                if let Err(err) =
                    new_commander().run_edit(self.head.commit_id.as_str(), ignore_immutable)
                {
                    return Ok(Self::command_error_popup("Edit", err));
                }
                self.refresh_log_output();
                return Ok(ComponentInputResult::HandledAction(AppAction::ChangeHead(
                    self.head.clone(),
                )));
            }
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable } => {
                if self.head.immutable && !ignore_immutable {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            " Update change id ",
                            "The change id cannot be updated because the change is immutable.",
                        ))),
                    )));
                }

                let mut lines = vec![
                    Line::from("Are you sure you want to generate a new change id?"),
                    Line::from(format!("Change: {}", self.head.change_id.as_str())),
                    Line::from("This is useful to resolve divergence."),
                ];
                if ignore_immutable {
                    lines.push(Line::from("This change is immutable."))
                }
                self.popup = ConfirmDialogState::new(
                    METAEDIT_UPDATE_CHANGE_ID_POPUP_ID,
                    Span::styled(" Update change id ", Style::new().bold().cyan()),
                    Text::from(lines).fg(Color::default()),
                );
                self.popup
                    .with_yes_button(ButtonLabel::YES.clone())
                    .with_no_button(ButtonLabel::NO.clone())
                    .with_listener(Some(self.popup_tx.clone()))
                    .open();
                self.metaedit_update_change_id_ignore_immutable = ignore_immutable;
            }
            LogTabEvent::Abandon => {
                return self.handle_abandon();
            }
            LogTabEvent::SimplifyParents {
                include_descendants,
            } => {
                let picks = self.take_picked_commits();
                match new_commander().run_simplify_parents(&picks, include_descendants) {
                    Err(err) => {
                        return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                            Some(Box::new(MessagePopup::new(
                                "Simplify parents",
                                format!("{err:#}"),
                            ))),
                        )));
                    }
                    Ok(summary) => {
                        let status_message = match summary {
                            Some(summary) => format!("{summary} | u: undo"),
                            None => "No redundant parents to remove".to_owned(),
                        };
                        self.set_head(new_commander().get_head_latest(&self.head)?);
                        return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                            vec![
                                AppAction::ChangeHead(self.head.clone()),
                                AppAction::SetStatusMessage(status_message),
                            ],
                        )));
                    }
                }
            }
            LogTabEvent::ResolveConflicts { keep_destination } => {
                return self.handle_resolve(keep_destination);
            }
            LogTabEvent::ResolveInEditor => {
                if self.head.immutable {
                    return Self::message_popup(
                        "Resolve",
                        "The conflicts cannot be resolved because the change is immutable.",
                    );
                }
                if new_commander()
                    .get_conflicts(&self.head.commit_id)?
                    .is_empty()
                {
                    return Self::message_popup(
                        "Resolve",
                        "The change has no conflicts to resolve.",
                    );
                }
                return Ok(ComponentInputResult::HandledAction(
                    AppAction::RunInteractive(Commander::resolve_interactive_command(
                        self.head.commit_id.as_str(),
                        None,
                    )),
                ));
            }
            LogTabEvent::Absorb => {
                let outcome = match new_commander().run_absorb(self.head.commit_id.as_str()) {
                    Ok(outcome) => outcome,
                    Err(err) => return Ok(Self::command_error_popup("Absorb", err)),
                };

                let status_message = if outcome.absorbed.is_empty() {
                    "Nothing to absorb".to_owned()
                } else {
                    let mut message = match outcome.absorbed.len() {
                        1 => "Absorbed into 1 revision (★)".to_owned(),
                        n => format!("Absorbed into {n} revisions (★)"),
                    };
                    match outcome.rebased.len() {
                        0 => {}
                        1 => message.push_str(", rebased 1 other (☆)"),
                        m => message.push_str(&format!(", rebased {m} others (☆)")),
                    }
                    message
                };
                // Set before set_head/refresh_log_output below, which bakes the
                // absorb glyphs into the freshly fetched log text.
                self.log_panel.absorbed_heads = outcome
                    .absorbed
                    .into_iter()
                    .map(|head| head.change_id)
                    .collect();
                self.log_panel.rebased_heads = outcome
                    .rebased
                    .into_iter()
                    .map(|head| head.change_id)
                    .collect();
                self.set_head(new_commander().get_head_latest(&self.head)?);

                return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::ChangeHead(self.head.clone()),
                        AppAction::SetStatusMessage(status_message),
                    ],
                )));
            }
            LogTabEvent::Undo => {
                // jj declines some undos outright (a merge operation, say) and
                // its message names the alternative, so show it rather than
                // letting the error escape and take the TUI with it.
                if let Err(err) = new_commander().run_undo() {
                    return Ok(Self::command_error_popup("Undo", err));
                }
                return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::RefreshTab(),
                        AppAction::SetStatusMessage("Undid last operation | U: redo".to_owned()),
                    ],
                )));
            }
            LogTabEvent::Redo => {
                if let Err(err) = new_commander().run_redo() {
                    return Ok(Self::command_error_popup("Redo", err));
                }
                return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::RefreshTab(),
                        AppAction::SetStatusMessage("Redid last undone operation".to_owned()),
                    ],
                )));
            }
            LogTabEvent::Describe => {
                if self.head.immutable {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            "Describe",
                            "The change cannot be described because it is immutable.",
                        ))),
                    )));
                }
                // Hand the description to the user's editor rather than an
                // in-TUI text box: descriptions get long, and a real editor
                // brings their own keybindings, wrapping, and undo.
                return Ok(ComponentInputResult::HandledAction(
                    AppAction::RunInteractive(Commander::describe_interactive_command(
                        self.head.commit_id.as_str(),
                        false,
                    )),
                ));
            }
            LogTabEvent::TransformDescription { index } => {
                return self.handle_transform_description(index);
            }
            LogTabEvent::EditRevset => {
                self.revset_editor = Some(RevsetEditor::new(
                    self.log_panel.log_revset.as_deref(),
                    self.log_panel.highlight_revset(),
                ));
                return Ok(ComponentInputResult::Handled);
            }
            LogTabEvent::FileFilter => {
                // The key toggles: with a filter already up, clear it rather than
                // asking for another path.
                if self.log_panel.has_active_highlight() {
                    self.clear_highlight();
                    return Ok(ComponentInputResult::Handled);
                }
                // Unlike search, do not apply as the user types -- each update
                // shells out to jj.
                self.file_filter_textarea = Some(TextArea::default());
                return Ok(ComponentInputResult::Handled);
            }
            LogTabEvent::RelatedFileFilter => {
                // Toggles, like the path file filter.
                if self.log_panel.has_active_highlight() {
                    self.clear_highlight();
                    return Ok(ComponentInputResult::Handled);
                }
                return self.apply_related_file_highlight();
            }
            LogTabEvent::Search => {
                // Start a fresh query. Highlighting updates live as the user
                // types; the selection only jumps on Enter.
                let textarea = TextArea::default();
                self.log_panel.set_search_query("");
                self.search_textarea = Some(textarea);
                return Ok(ComponentInputResult::Handled);
            }
            LogTabEvent::SetBookmark => {
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(BookmarkSetPopup::new(
                        self.config.clone(),
                        Some(self.head.change_id.clone()),
                        self.head.commit_id.clone(),
                        self.bookmark_set_popup_tx.clone(),
                    ))),
                )));
            }
            LogTabEvent::SetTag => {
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(TagSetPopup::new(
                        self.head.commit_id.clone(),
                        self.tag_set_popup_tx.clone(),
                    ))),
                )));
            }
            LogTabEvent::OpenFiles => {
                // On a placeholder, Enter reveals what it stands for rather
                // than opening files for the revision it hangs beneath.
                if self.log_panel.is_on_elided_row() {
                    return self.expand_elided();
                }
                return Ok(ComponentInputResult::HandledAction(AppAction::ViewFiles(
                    self.head.clone(),
                )));
            }
            LogTabEvent::OpenTree => match new_commander().open_revision_tree_command(&self.head) {
                Ok(command) => {
                    return Ok(ComponentInputResult::HandledAction(
                        AppAction::OpenInEditor(command),
                    ));
                }
                Err(err) => {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new(
                            "Browse revision",
                            format!("{err:#}"),
                        ))),
                    )));
                }
            },
            LogTabEvent::CopyChangeId => {
                // Copy change ID to clipboard using crossterm
                let change_id = self.head.change_id.as_str();
                let _ = execute!(
                    std::io::stdout(),
                    CopyToClipboard::to_clipboard_from(change_id)
                );
            }
            LogTabEvent::CopyRev => {
                // Copy revision (commit ID) to clipboard using crossterm
                let commit_id = self.head.commit_id.as_str();
                let _ = execute!(
                    std::io::stdout(),
                    CopyToClipboard::to_clipboard_from(commit_id)
                );
            }
            LogTabEvent::Push => {
                let commit_id = self.head.commit_id.clone();

                let loader = LoaderPopup::new("Pushing".to_string(), move || {
                    new_commander().git_push(&commit_id)
                });

                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(loader)),
                )));
            }
            LogTabEvent::Fetch { all_remotes } => {
                let loader = LoaderPopup::new("Fetching".to_string(), move || {
                    new_commander().git_fetch(all_remotes)
                });

                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(loader)),
                )));
            }
            LogTabEvent::OpenHelp => {
                let mut main_panel_help = self.keybinds.make_main_panel_help();
                main_panel_help.extend(self.keybinds.make_description_transforms_help(
                    get_env().jj_config.description_transforms(),
                ));
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(HelpPopup::new(
                        main_panel_help,
                        vec![
                            ("Ctrl+e/Ctrl+y".to_owned(), "scroll down/up".to_owned()),
                            (
                                "Ctrl+d/Ctrl+u".to_owned(),
                                "scroll down/up by ½ page".to_owned(),
                            ),
                            (
                                "Ctrl+f/Ctrl+b".to_owned(),
                                "scroll down/up by page".to_owned(),
                            ),
                            ("w".to_owned(), "toggle diff format".to_owned()),
                            ("W".to_owned(), "toggle wrapping".to_owned()),
                        ],
                    ))),
                )));
            }
            LogTabEvent::Save
            | LogTabEvent::Cancel
            | LogTabEvent::ClosePopup
            | LogTabEvent::Unbound => return Ok(ComponentInputResult::NotHandled),
        };
        Ok(ComponentInputResult::Handled)
    }
}

impl Component for LogTab<'_> {
    fn focus(&mut self) -> Result<()> {
        let latest_head = new_commander().get_head_latest(&self.head)?;
        self.set_head(latest_head);
        self.refresh_dirty_submodules();
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        // Check for popup action
        if let Ok(res) = self.popup_rx.try_recv()
            && res.1.unwrap_or(false)
        {
            match res.0 {
                NEW_POPUP_ID => {
                    return self.execute_new();
                }
                METAEDIT_UPDATE_CHANGE_ID_POPUP_ID => {
                    new_commander().run_metaedit_update_change_id(
                        self.head.commit_id.as_str(),
                        self.metaedit_update_change_id_ignore_immutable,
                    )?;
                    return Ok(Some(AppAction::RefreshTab()));
                }
                ABANDON_POPUP_ID => {
                    return self.execute_abandon();
                }
                RESOLVE_POPUP_ID => {
                    return self.execute_resolve();
                }
                _ => {}
            }
        }

        if let Ok(true) = self.bookmark_set_popup_rx.try_recv() {
            self.refresh_log_output();
        }

        if let Ok(true) = self.tag_set_popup_rx.try_recv() {
            self.refresh_log_output();
        }

        Ok(None)
    }

    fn draw(
        &mut self,
        f: &mut ratatui::prelude::Frame<'_>,
        area: ratatui::prelude::Rect,
    ) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        // Draw log
        self.log_panel.draw(f, chunks[0])?;

        // Draw the vim-style search bar over the bottom row of the log panel.
        // Only one of these bars is ever open, since each input handler consumes
        // the event that would open the other.
        if let Some(search_textarea) = self.search_textarea.as_mut() {
            draw_prompt_bar(f, chunks[0], "/", Color::Yellow, search_textarea);
        }

        // Draw the file-filter bar in the same place
        if let Some(file_filter_textarea) = self.file_filter_textarea.as_mut() {
            draw_prompt_bar(f, chunks[0], "touching:", Color::Cyan, file_filter_textarea);
        }

        // Draw change details
        if let Some(content) = self.commit_show_cache.get(&self.head_key) {
            self.head_panel
                .render_context::<LargeStringContent>(content.value())
                .title(format!(" Details for {} ", self.head.change_id))
                .draw(f, chunks[1])
        }

        // Draw popup
        if self.popup.is_opened() {
            let popup = ConfirmDialog::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Green))
                .selected_button_style(
                    Style::default()
                        .bg(self.config.highlight_color())
                        .underlined(),
                );
            f.render_stateful_widget(popup, area, &mut self.popup);
        }

        // Draw revset textarea
        {
            if let Some(revset_editor) = self.revset_editor.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(" Revsets ", Style::new().bold().cyan()))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                // 2 border rows + one row per field + 3 help rows (separator, hint,
                // key list).
                let area = centered_rect_line_height(area, 40, 7);
                f.render_widget(Clear, area);
                f.render_widget(&block, area);

                let popup_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Length(3),
                    ])
                    .split(block.inner(area));

                let log_focused = revset_editor.focus == RevsetField::Log;
                draw_revset_field(
                    f,
                    popup_chunks[0],
                    " Show:",
                    log_focused,
                    &revset_editor.log,
                );
                draw_revset_field(
                    f,
                    popup_chunks[1],
                    " Mark:",
                    !log_focused,
                    &revset_editor.mark,
                );

                // Both fields take a revset, which is easy to forget on `Mark:`
                // where a bare path is the tempting thing to type -- and would be
                // read as a revision name. Point at the key that does accept a path.
                let hint = if log_focused {
                    "revisions to show, e.g. ::@ | empty: default"
                } else {
                    "revset to mark, e.g. conflicts() | for a path, use T"
                };
                let help = Paragraph::new(vec![
                    Line::from(Span::styled(hint, Style::new().fg(Color::DarkGray))),
                    "Tab: switch field | Ctrl+s: save | Escape: cancel".into(),
                ])
                .fg(Color::DarkGray)
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );

                f.render_widget(help, popup_chunks[2]);
            }
        }

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        if let Some(search_textarea) = self.search_textarea.as_mut() {
            if let Event::Key(key) = event {
                // Enter confirms the search (vim-style); Esc cancels it.
                match key.code {
                    KeyCode::Enter => {
                        let query = search_textarea.lines().join("");
                        self.search_textarea = None;
                        self.log_panel.set_search_query(&query);
                        if self.log_panel.has_active_search() {
                            let count = self.log_panel.select_first_match();
                            self.sync_head_output();
                            if count == 0 {
                                return Ok(ComponentInputResult::HandledAction(
                                    AppAction::SetStatusMessage(format!(
                                        "No matches for \"{query}\""
                                    )),
                                ));
                            }
                        }
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.search_textarea = None;
                        self.log_panel.clear_search();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            // Any other key edits the query; update the live highlight.
            search_textarea.input(event);
            let query = search_textarea.lines().join("");
            self.log_panel.set_search_query(&query);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(file_filter_textarea) = self.file_filter_textarea.as_mut() {
            if let Event::Key(key) = event {
                // Enter applies the filter; Esc abandons the edit. Esc leaves any
                // already-active highlight alone -- the key that opened this bar is
                // itself the toggle-off.
                match key.code {
                    KeyCode::Enter => {
                        let path = file_filter_textarea.lines().join("");
                        self.file_filter_textarea = None;
                        return Ok(self.apply_file_filter(&path));
                    }
                    KeyCode::Esc => {
                        self.file_filter_textarea = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            // Any other key edits the path. Deliberately no live update: each one
            // would shell out to jj.
            file_filter_textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(revset_editor) = self.revset_editor.as_mut() {
            if let Event::Key(key) = event {
                // Tab moves between the two fields before the keybind lookup, so a
                // configured binding on Tab cannot shadow it inside this popup.
                if key.code == KeyCode::Tab {
                    revset_editor.toggle_focus();
                    return Ok(ComponentInputResult::Handled);
                }
                match self.keybinds.match_event(key) {
                    LogTabEvent::Save => {
                        let log_revset = revset_editor.log_revset();
                        let mark_revset = revset_editor.mark_revset();
                        self.revset_editor = None;

                        self.log_panel.log_revset = log_revset;
                        // Apply the mark revset before refreshing, so the refresh's
                        // own re-fetch of the highlight set is the only query --
                        // and so `visible` is counted against the new log revset.
                        let outcome = self.log_panel.set_highlight(&mark_revset, None);
                        self.refresh_log_output();
                        return Ok(self.report_highlight_outcome(outcome));
                    }
                    LogTabEvent::Cancel => {
                        self.revset_editor = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => (),
                }
            }
            revset_editor.focused_mut().input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Event::Key(key) = &event {
            let key = *key;
            if key.kind != KeyEventKind::Press {
                return Ok(ComponentInputResult::Handled);
            }

            // Clear the absorb highlight on the next keypress, mirroring how
            // App::status_message clears (see LogTabEvent::Absorb).
            self.log_panel.clear_absorbed_heads();

            // Take up a pending "widen the log's revset" offer. Unlike the status
            // message that first advertises it, the offer deliberately outlives the
            // next keypress: the natural response to an empty gutter is to scroll
            // around looking for marks, and that must not silently retire it. It is
            // cleared when the condition behind it goes away instead -- see
            // `report_highlight_outcome` and `clear_highlight`.
            if self.pending_widen.is_some()
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('w')
            {
                return Ok(self.accept_widen());
            }

            if self.popup.is_opened() {
                if matches!(
                    self.keybinds.match_event(key),
                    LogTabEvent::ClosePopup | LogTabEvent::Cancel
                ) {
                    self.popup = ConfirmDialogState::default();
                } else {
                    self.popup.handle(&key);
                }

                return Ok(ComponentInputResult::Handled);
            }

            // While a search is active (query confirmed), n/N navigate matches
            // instead of creating changes, and Esc clears the search. This is
            // context-sensitive: with no active search, these keys behave
            // normally.
            //
            // n/N apply during a pick gesture too: they only move the cursor,
            // which is the whole point of searching while choosing a target.
            // Esc does not — there it means "cancel the gesture", handled by
            // the pick filter below, so the more destructive reading wins and
            // a stray Esc cannot leave a half-finished pick running.
            if self.log_panel.has_active_search() {
                let searching_idle = matches!(self.pick_state, PickState::Idle);
                match self.keybinds.match_event(key) {
                    LogTabEvent::CreateNew { no_edit: false } => {
                        self.navigate_search(true);
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::CreateNew { no_edit: true } => {
                        self.navigate_search(false);
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::Cancel if searching_idle => {
                        self.log_panel.clear_search();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }

            // With a highlight active and no search to clear first, Esc clears the
            // highlight. Search wins when both are up, keeping the pre-existing
            // meaning of Esc unchanged.
            if self.log_panel.has_active_highlight()
                && !self.log_panel.has_active_search()
                && matches!(self.pick_state, PickState::Idle)
                && matches!(self.keybinds.match_event(key), LogTabEvent::Cancel)
            {
                self.clear_highlight();
                return Ok(ComponentInputResult::Handled);
            }

            if !matches!(self.pick_state, PickState::Idle) {
                match self.keybinds.match_event(key) {
                    LogTabEvent::OpenFiles => {
                        // A placeholder is not a revision, so it cannot be the
                        // pick. Say so rather than silently using the revision
                        // it hangs beneath, which is not what the cursor is on.
                        if self.log_panel.is_on_elided_row() {
                            return Self::message_popup(
                                "Elided revisions",
                                "This row stands for hidden revisions, not a change. Move to a revision to pick it.",
                            );
                        }
                        // "enter" advances the pick gesture instead of opening
                        // files while a pick is being collected.
                        return self.advance_pick();
                    }
                    LogTabEvent::Cancel => {
                        self.cancel_pick();
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::Rebase
                        if matches!(self.pick_state, PickState::RebaseDestinations { .. }) =>
                    {
                        // The rebase key toggles what moves mid-gesture
                        self.toggle_rebase_descendants();
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::Squash { .. }
                        if matches!(self.pick_state, PickState::SquashDestination { .. }) =>
                    {
                        // The squash key toggles interactive mode mid-gesture
                        self.toggle_squash_interactive();
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::ToggleHeadBeforeMark
                        if matches!(self.pick_state, PickState::RebaseDestinations { .. }) =>
                    {
                        // A before-anchor switches the gesture into insert mode,
                        // so the title has to be recomputed alongside the mark.
                        self.log_panel.toggle_head_before_mark();
                        self.update_pick_title();
                        self.sync_head_output();
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::CreateNew { no_edit }
                        if matches!(self.pick_state, PickState::RebaseDestinations { .. }) =>
                    {
                        // New sibling: the phase has already seeded the marks
                        // with the source's parents, so `n` here creates a
                        // change beside it rather than moving it. End the
                        // gesture first — but leave the marks, which are the
                        // parent set `handle_new` is about to read.
                        self.end_pick_keeping_marks();
                        return self.handle_new(no_edit);
                    }
                    _ => {}
                }
            }

            if self.head_panel.input(key) {
                return Ok(ComponentInputResult::Handled);
            }

            let input_result = self.log_panel.input(event)?;
            if input_result.is_handled() {
                self.sync_head_output();
                return Ok(input_result);
            }

            let log_tab_event = self.keybinds.match_event(key);
            return self.handle_event(log_tab_event);
        }

        if let Event::Mouse(mouse_event) = event {
            if self
                .pane_divider
                .handle_mouse(mouse_event, self.config.layout())
            {
                return Ok(ComponentInputResult::Handled);
            }
            let input_result = self.log_panel.input(event.clone())?;
            if input_result.is_handled() {
                self.sync_head_output();
                return Ok(input_result);
            }
            if self.head_panel.input_mouse(mouse_event) {
                return Ok(ComponentInputResult::Handled);
            }
            return Ok(ComponentInputResult::NotHandled);
        }

        Ok(ComponentInputResult::Handled)
    }
}
