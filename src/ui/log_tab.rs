#![expect(clippy::borrow_interior_mutable_const)]

use std::cmp::max;

use anyhow::Result;
use ratatui::crossterm::clipboard::CopyToClipboard;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyEventKind;
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
use crate::ui::dialog::RebasePopup;
use crate::ui::dialog::RebasePopupExit;
use crate::ui::panel::DetailsPanel;
use crate::ui::panel::LargeStringContent;
use crate::ui::panel::LogPanel;
use crate::ui::utils::PaneDivider;
use crate::ui::utils::centered_rect_fixed;
use crate::ui::utils::centered_rect_line_height;
use crate::ui::utils::tabs_to_spaces;

const NEW_POPUP_ID: u16 = 1;
const EDIT_POPUP_ID: u16 = 2;
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
    /// After the rebase key: collecting the destination(s). Multiple
    /// destinations rebase `sources` onto their merge.
    RebaseDestinations { sources: Vec<CommitId> },
    /// After the squash key: collecting the single destination. On entry the
    /// cursor is placed on the source's parent, so an immediate Enter gives
    /// `jj squash` semantics.
    SquashDestination {
        sources: Vec<CommitId>,
        ignore_immutable: bool,
    },
    /// After the insert-move key: collecting the `-A` (insert-after) anchors
    /// for the change being moved.
    InsertAfter { moving: CommitId },
    /// Collecting the `-B` (insert-before) anchors: entered directly by the
    /// insert-new key (whose pre-key pick is the `-A` anchors; `moving` is
    /// `None` since a brand-new change is created), or as the second phase
    /// of insert-move.
    InsertBefore {
        moving: Option<CommitId>,
        after: Vec<CommitId>,
    },
}

/// Log tab. Shows `jj log` in main panel and shows selected change details of in details panel.
pub struct LogTab<'a> {
    /// The revset filter to apply to jj log
    log_revset_textarea: Option<TextArea<'a>>,

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

    describe_textarea: Option<TextArea<'a>>,
    describe_after_new: bool,

    rebase_popup: Option<RebasePopup>,
    /// The first picked-up rebase source, resolved before the rebase rewrites
    /// it, so the selection can follow the moved change afterwards.
    rebase_follow: Option<Head>,

    edit_ignore_immutable: bool,

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

        let mut keybinds = LogTabKeybinds::default();
        if let Some(keybinds_config) = get_env().jj_config.keybinds() {
            keybinds.extend_from_config(keybinds_config);
        }

        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        Ok(Self {
            log_revset_textarea: None,

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

            describe_textarea: None,
            describe_after_new: false,

            rebase_popup: None,
            rebase_follow: None,

            edit_ignore_immutable: false,

            metaedit_update_change_id_ignore_immutable: false,

            resolve_keep_destination: false,

            pick_state: PickState::Idle,

            config,
            pane_divider,
            keybinds,
        })
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
        let output = match head_output {
            Ok(head_output) => head_output,
            Err(err) => err.to_string(),
        };
        // Build value used by cache and return it
        let key = CommitShowKey::new(head.clone(), diff_format.clone(), inner_width);
        CommitShowValue::new(key, output)
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
    fn handle_new(&mut self, describe: bool) -> Result<ComponentInputResult> {
        let mark_count = self.log_panel.marked_heads.len();
        let text = if mark_count > 0 {
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
        self.describe_after_new = describe;
        Ok(ComponentInputResult::Handled)
    }

    // Execute new command, after self.popup returned
    fn execute_new(&mut self) -> Result<Option<AppAction>> {
        let commit_ids = self.log_panel.extract_and_clear_head_marks();
        if commit_ids.is_empty() {
            new_commander().run_new([self.head.commit_id.as_str()])?;
        } else {
            new_commander().run_new(commit_ids.iter().map(CommitId::as_str))?;
        }
        self.set_head(new_commander().get_current_head()?);
        if self.describe_after_new {
            self.describe_after_new = false;
            let textarea = TextArea::default();
            self.describe_textarea = Some(textarea);
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

    /// Pick up change(s) to rebase; the destinations are picked next.
    fn start_rebase(&mut self) {
        let sources = self.take_picked_commits();
        self.pick_state = PickState::RebaseDestinations { sources };
        self.update_pick_title();
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
        };
        self.update_pick_title();
    }

    /// Pick up the `-A` (insert-after) anchors for a brand-new change; the
    /// `-B` (insert-before) anchors are picked next.
    ///
    /// Exception: for exactly two picks where one is an ancestor of the
    /// other, the assignment is forced (the reverse would be a cycle), so
    /// the new change is inserted between them immediately.
    fn start_insert_new(&mut self) -> Result<ComponentInputResult> {
        let picked = self.take_picked_commits();

        if let [x, y] = picked.as_slice()
            && let [descendant] = new_commander().get_heads_among(&picked)?.as_slice()
        {
            let ancestor = if descendant == x { y } else { x };
            return self.execute_insert_new(
                vec![ancestor.clone()],
                std::slice::from_ref(descendant).to_vec(),
            );
        }

        self.pick_state = PickState::InsertBefore {
            moving: None,
            after: picked,
        };
        self.update_pick_title();
        Ok(ComponentInputResult::Handled)
    }

    /// Insert a brand-new change between the anchors and put the cursor on it.
    fn execute_insert_new(
        &mut self,
        after: Vec<CommitId>,
        before: Vec<CommitId>,
    ) -> Result<ComponentInputResult> {
        match new_commander().run_new_insert(&after, &before) {
            Err(err) => Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                Some(Box::new(MessagePopup::new("Insert", format!("{err:#}")))),
            ))),
            Ok(inserted) => {
                self.set_head(inserted);
                Ok(ComponentInputResult::HandledAction(AppAction::ChangeHead(
                    self.head.clone(),
                )))
            }
        }
    }

    /// Pick up the change to move; its `-A` and `-B` anchors are picked next.
    fn start_insert_move(&mut self) -> Result<ComponentInputResult> {
        let picked = self.take_picked_commits();
        let [moving] = picked.as_slice() else {
            return Self::message_popup(
                "Insert",
                "Mark exactly one change to move, or none to move the change under the cursor.",
            );
        };
        self.pick_state = PickState::InsertAfter {
            moving: moving.clone(),
        };
        self.update_pick_title();
        Ok(ComponentInputResult::Handled)
    }

    fn cancel_pick(&mut self) {
        self.log_panel.extract_and_clear_head_marks();
        self.pick_state = PickState::Idle;
        self.log_panel.title_override = None;
    }

    fn update_pick_title(&mut self) {
        let hint = match &self.pick_state {
            PickState::Idle => None,
            PickState::RebaseDestinations { .. } => Some(
                " Rebase: pick destination(s) (space: mark several, enter: confirm, esc: cancel) "
                    .to_owned(),
            ),
            PickState::SquashDestination { .. } => {
                Some(" Squash: pick destination (enter: confirm, esc: cancel) ".to_owned())
            }
            PickState::InsertAfter { .. } => Some(
                " Move: pick AFTER-anchors (space: mark several, enter: next, esc: cancel) "
                    .to_owned(),
            ),
            PickState::InsertBefore {
                moving: Some(_), ..
            } => Some(
                " Move: pick BEFORE-anchors (space: mark several, enter: confirm, esc: cancel) "
                    .to_owned(),
            ),
            PickState::InsertBefore { moving: None, .. } => Some(
                " Insert: pick BEFORE-anchors (space: mark several, enter: confirm, esc: cancel) "
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
            PickState::RebaseDestinations { sources } => {
                let targets = self.take_picked_commits();
                if targets.iter().any(|target| sources.contains(target)) {
                    return Self::message_popup(
                        "Rebase",
                        "The destination is one of the picked-up changes.",
                    );
                }
                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;
                // Resolve the first source before the rebase rewrites it, so
                // the selection can follow the moved change afterwards
                self.rebase_follow = sources
                    .first()
                    .and_then(|source| new_commander().get_head(source.as_str()).ok());
                self.rebase_popup = Some(RebasePopup::new(sources, targets));
                Ok(ComponentInputResult::Handled)
            }
            PickState::SquashDestination {
                sources,
                ignore_immutable,
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
            PickState::InsertAfter { moving } => {
                let after = self.take_picked_commits();
                self.pick_state = PickState::InsertBefore {
                    moving: Some(moving),
                    after,
                };
                self.update_pick_title();
                Ok(ComponentInputResult::Handled)
            }
            PickState::InsertBefore { moving, after } => {
                let before = self.take_picked_commits();
                self.pick_state = PickState::Idle;
                self.log_panel.title_override = None;

                let Some(moving) = &moving else {
                    return self.execute_insert_new(after, before);
                };

                // Resolve before the rebase rewrites the moved change, so the
                // cursor can follow it afterwards
                let landed = new_commander()
                    .get_head(moving.as_str())
                    .and_then(|moving_head| {
                        new_commander().run_rebase_insert(moving.as_str(), &after, &before)?;
                        new_commander().get_head_latest(&moving_head)
                    });
                match landed {
                    Err(err) => Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new("Insert", format!("{err:#}")))),
                    ))),
                    Ok(landed) => {
                        self.set_head(landed);
                        Ok(ComponentInputResult::HandledAction(AppAction::ChangeHead(
                            self.head.clone(),
                        )))
                    }
                }
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
        new_commander().run_abandon(&commit_id_list)?;
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
        match log_tab_event {
            LogTabEvent::ScrollDown
            | LogTabEvent::ScrollUp
            | LogTabEvent::ScrollDownHalf
            | LogTabEvent::ScrollUpHalf
            | LogTabEvent::ScrollToBottom
            | LogTabEvent::ScrollToTop
            | LogTabEvent::ToggleHeadMark => {
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
                self.refresh_log_output();
            }

            LogTabEvent::Duplicate => {
                let _ = new_commander().run_duplicate(&self.head.change_id.to_string());
                self.refresh_log_output();
            }

            LogTabEvent::CreateNew { describe } => {
                return self.handle_new(describe);
            }
            LogTabEvent::InsertNew => {
                return self.start_insert_new();
            }
            LogTabEvent::InsertMove => {
                return self.start_insert_move();
            }
            LogTabEvent::Rebase => {
                self.start_rebase();
            }
            LogTabEvent::Squash { ignore_immutable } => {
                self.start_squash(ignore_immutable);
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

                let mut lines = vec![
                    Line::from("Are you sure you want to edit an existing change?"),
                    Line::from(format!("Change: {}", self.head.change_id.as_str())),
                ];
                if ignore_immutable {
                    lines.push(Line::from("This change is immutable."))
                }
                self.popup = ConfirmDialogState::new(
                    EDIT_POPUP_ID,
                    Span::styled(" Edit ", Style::new().bold().cyan()),
                    Text::from(lines).fg(Color::default()),
                );
                self.popup
                    .with_yes_button(ButtonLabel::YES.clone())
                    .with_no_button(ButtonLabel::NO.clone())
                    .with_listener(Some(self.popup_tx.clone()))
                    .open();
                self.edit_ignore_immutable = ignore_immutable;
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
            LogTabEvent::Absorb => {
                let outcome = new_commander().run_absorb(self.head.commit_id.as_str())?;

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
                new_commander().run_undo()?;
                return Ok(ComponentInputResult::HandledAction(AppAction::Multiple(
                    vec![
                        AppAction::RefreshTab(),
                        AppAction::SetStatusMessage(
                            "Undid last operation | shift+u: redo".to_owned(),
                        ),
                    ],
                )));
            }
            LogTabEvent::Redo => {
                new_commander().run_redo()?;
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
                } else {
                    let mut textarea = TextArea::new(
                        new_commander()
                            .get_commit_description(&self.head.commit_id)?
                            .split("\n")
                            .map(|line| line.to_string())
                            .collect(),
                    );
                    textarea.move_cursor(CursorMove::End);
                    self.describe_textarea = Some(textarea);
                    return Ok(ComponentInputResult::Handled);
                }
            }
            LogTabEvent::EditRevset => {
                let mut textarea = TextArea::new(
                    self.log_panel
                        .log_revset
                        .as_ref()
                        .unwrap_or(&"".to_owned())
                        .lines()
                        .map(String::from)
                        .collect(),
                );
                textarea.move_cursor(CursorMove::End);
                self.log_revset_textarea = Some(textarea);
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
            LogTabEvent::OpenFiles => {
                return Ok(ComponentInputResult::HandledAction(AppAction::ViewFiles(
                    self.head.clone(),
                )));
            }
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
            LogTabEvent::Push { all_bookmarks } => {
                let commit_id = self.head.commit_id.clone();

                let loader = LoaderPopup::new("Pushing".to_string(), move || {
                    new_commander().git_push(all_bookmarks, &commit_id)
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
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(HelpPopup::new(
                        self.keybinds.make_main_panel_help(),
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
                EDIT_POPUP_ID => {
                    new_commander()
                        .run_edit(self.head.commit_id.as_str(), self.edit_ignore_immutable)?;
                    self.refresh_log_output();
                    return Ok(Some(AppAction::ChangeHead(self.head.clone())));
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

        // Draw describe textarea
        {
            if let Some(describe_textarea) = self.describe_textarea.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(" Describe ", Style::new().bold().cyan()))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                // Text target size
                const MAX_COMMIT_WIDTH: u16 = 72; // git recommended max width
                const MIN_COMMIT_HEIGHT: u16 = 5; // heading + blank + 3 lines
                // Include margin and help text to get size
                let area = centered_rect_fixed(
                    area,
                    /* width */ MAX_COMMIT_WIDTH + 2,
                    /* height */ max(MIN_COMMIT_HEIGHT + 4, area.height / 2),
                );
                f.render_widget(Clear, area);
                f.render_widget(&block, area);

                let popup_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Fill(1), Constraint::Length(2)])
                    .split(block.inner(area));

                f.render_widget(&*describe_textarea, popup_chunks[0]);

                let help = Paragraph::new(vec!["Ctrl+s: save | Escape: cancel".into()])
                    .fg(Color::DarkGray)
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    );

                f.render_widget(help, popup_chunks[1]);
            }
        }

        // Draw revset textarea
        {
            if let Some(log_revset_textarea) = self.log_revset_textarea.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(" Revset ", Style::new().bold().cyan()))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                let area = centered_rect_line_height(area, 30, 7);
                f.render_widget(Clear, area);
                f.render_widget(&block, area);

                let popup_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Fill(1), Constraint::Length(2)])
                    .split(block.inner(area));

                f.render_widget(&*log_revset_textarea, popup_chunks[0]);

                let help = Paragraph::new(vec!["Ctrl+s: save | Escape: cancel".into()])
                    .fg(Color::DarkGray)
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    );

                f.render_widget(help, popup_chunks[1]);
            }
        }

        // Draw rebase popup
        {
            if let Some(log_rebase_popup) = &mut self.rebase_popup {
                log_rebase_popup.render_widget(f)
            }
        }

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        if let Some(describe_textarea) = self.describe_textarea.as_mut() {
            if let Event::Key(key) = event {
                match self.keybinds.match_event(key) {
                    LogTabEvent::Save => {
                        // TODO: Handle error
                        new_commander().run_describe(
                            self.head.commit_id.as_str(),
                            &describe_textarea.lines().join("\n"),
                        )?;
                        self.set_head(new_commander().get_head_latest(&self.head)?);
                        self.describe_textarea = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::Cancel => {
                        self.describe_textarea = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => (),
                }
            }
            describe_textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(log_revset_textarea) = self.log_revset_textarea.as_mut() {
            if let Event::Key(key) = event {
                match self.keybinds.match_event(key) {
                    LogTabEvent::Save => {
                        let log_revset = log_revset_textarea.lines().join("\n");
                        self.log_panel.log_revset = if log_revset.trim().is_empty() {
                            None
                        } else {
                            Some(log_revset)
                        };
                        self.refresh_log_output();
                        self.log_revset_textarea = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    LogTabEvent::Cancel => {
                        self.log_revset_textarea = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => (),
                }
            }
            log_revset_textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(rebase_popup) = &mut self.rebase_popup {
            match rebase_popup.handle_input(event.clone()) {
                Err(msg) => {
                    // Close popup and show error message
                    self.rebase_popup = None;
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(MessagePopup::new("Error", format!("{msg:#}")))),
                    )));
                }
                Ok(RebasePopupExit::Executed) => {
                    self.rebase_popup = None;
                    // The rebased change was rewritten; follow it
                    let follow = self
                        .rebase_follow
                        .take()
                        .unwrap_or_else(|| self.head.clone());
                    self.set_head(new_commander().get_head_latest(&follow)?);
                    return Ok(ComponentInputResult::HandledAction(AppAction::ChangeHead(
                        self.head.clone(),
                    )));
                }
                Ok(RebasePopupExit::Cancelled) => {
                    self.rebase_popup = None;
                    self.rebase_follow = None;
                    return Ok(ComponentInputResult::Handled);
                }
                Ok(RebasePopupExit::KeepOpen) => {
                    return Ok(ComponentInputResult::Handled);
                }
            }
        }

        if let Event::Key(key) = &event {
            let key = *key;
            if key.kind != KeyEventKind::Press {
                return Ok(ComponentInputResult::Handled);
            }

            // Clear the absorb highlight on the next keypress, mirroring how
            // App::status_message clears (see LogTabEvent::Absorb).
            self.log_panel.clear_absorbed_heads();

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

            if !matches!(self.pick_state, PickState::Idle) {
                match self.keybinds.match_event(key) {
                    LogTabEvent::OpenFiles => {
                        // "enter" advances the pick gesture instead of opening
                        // files while a pick is being collected.
                        return self.advance_pick();
                    }
                    LogTabEvent::Cancel => {
                        self.cancel_pick();
                        return Ok(ComponentInputResult::Handled);
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
