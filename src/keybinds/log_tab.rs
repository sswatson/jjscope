use std::str::FromStr;

use ratatui::crossterm::event::KeyEvent;

use super::Shortcut;
use super::config::KeybindsConfig;
use super::config::LogTabKeybindsConfig;
use super::keybinds_store::KeybindsStore;
use crate::make_keybinds_help;
use crate::set_keybinds;
use crate::update_keybinds;

#[derive(Debug)]
pub struct LogTabKeybinds {
    // todo: probably split keys for different contexts, e.g when describe_textarea is opened
    keys: KeybindsStore<LogTabEvent>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum LogTabEvent {
    Save,
    Cancel,

    ClosePopup,

    ScrollDown,
    ScrollUp,
    ScrollDownHalf,
    ScrollUpHalf,
    ScrollToBottom,
    ScrollToTop,

    FocusCurrent,
    ToggleHeadMark,
    ToggleDiffFormat,

    Refresh,
    CreateNew { describe: bool },
    InsertNew,
    InsertMove,
    Duplicate,
    Rebase,
    RebaseBranch,
    Squash { ignore_immutable: bool },
    Split,
    DiffEdit,
    EditChange { ignore_immutable: bool },
    Abandon,
    Absorb,
    SimplifyParents { include_descendants: bool },
    ResolveConflicts { keep_destination: bool },
    ResolveInEditor,
    Undo,
    Redo,
    MetaeditUpdateChangeId { ignore_immutable: bool },
    Describe,
    EditRevset,
    SetBookmark,
    OpenFiles,
    CopyChangeId,
    CopyRev,

    Push,
    Fetch { all_remotes: bool },

    OpenHelp,

    Unbound,
}

impl Default for LogTabKeybinds {
    fn default() -> Self {
        let mut keys = KeybindsStore::<LogTabEvent>::default();
        set_keybinds!(
            keys,
            LogTabEvent::Save => "ctrl+s",
            LogTabEvent::Cancel => "esc",
            LogTabEvent::ClosePopup => "q",
            LogTabEvent::ScrollDown => "j",
            LogTabEvent::ScrollDown => "down",
            LogTabEvent::ScrollUp => "k",
            LogTabEvent::ScrollUp => "up",
            LogTabEvent::ScrollDownHalf => "shift+j",
            LogTabEvent::ScrollUpHalf => "shift+k",
            LogTabEvent::ScrollToBottom => "ctrl+end",
            LogTabEvent::ScrollToTop => "ctrl+home",
            LogTabEvent::FocusCurrent => "@",
            LogTabEvent::ToggleHeadMark => "space",
            // todo: move to DetailsKeybindings
            LogTabEvent::ToggleDiffFormat => "w",
            LogTabEvent::Refresh => "shift+r",
            LogTabEvent::Refresh => "f5",
            LogTabEvent::Duplicate => "shift+d",
            LogTabEvent::CreateNew { describe: false } => "n",
            LogTabEvent::CreateNew { describe: true } => "shift+n",
            LogTabEvent::InsertNew => "i",
            LogTabEvent::InsertMove => "shift+i",
            LogTabEvent::Rebase => "r",
            LogTabEvent::RebaseBranch => "shift+b",
            LogTabEvent::Squash { ignore_immutable: false } => "s",
            LogTabEvent::Squash { ignore_immutable: true } => "shift+s",
            LogTabEvent::Split => "-",
            LogTabEvent::DiffEdit => "=",
            LogTabEvent::EditChange { ignore_immutable: false } => "e",
            LogTabEvent::EditChange { ignore_immutable: true } => "shift+e",
            LogTabEvent::Abandon => "a",
            LogTabEvent::Absorb => "shift+a",
            LogTabEvent::SimplifyParents { include_descendants: false } => "x",
            LogTabEvent::SimplifyParents { include_descendants: true } => "shift+x",
            LogTabEvent::ResolveConflicts { keep_destination: false } => "v",
            LogTabEvent::ResolveConflicts { keep_destination: true } => "shift+v",
            LogTabEvent::ResolveInEditor => "m",
            LogTabEvent::Undo => "u",
            LogTabEvent::Redo => "shift+u",
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: false } => "c",
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: true } => "shift+c",
            LogTabEvent::Describe => "d",
            LogTabEvent::EditRevset => "ctrl+r",
            LogTabEvent::SetBookmark => "b",
            LogTabEvent::OpenFiles => "enter",
            LogTabEvent::CopyChangeId => "y",
            LogTabEvent::CopyRev => "shift+y",
            LogTabEvent::Push => "p",
            LogTabEvent::Fetch { all_remotes: false } => "f",
            LogTabEvent::Fetch { all_remotes: true } => "shift+f",
            LogTabEvent::OpenHelp => "?",
        );

        Self { keys }
    }
}

impl LogTabKeybinds {
    pub fn match_event(&self, event: KeyEvent) -> LogTabEvent {
        if let Some(action) = self.keys.match_event(event) {
            action
        } else {
            LogTabEvent::Unbound
        }
    }
    pub fn extend_from_config(&mut self, config: &KeybindsConfig) {
        update_keybinds!(
            self.keys,
            LogTabEvent::ScrollDown => config.scroll_down,
            LogTabEvent::ScrollUp => config.scroll_up,
            LogTabEvent::ScrollDownHalf => config.scroll_down_half,
            LogTabEvent::ScrollUpHalf => config.scroll_up_half,
        );
        if let Some(ref log_tab) = config.log_tab {
            self.extend_from_log_tab_config(log_tab);
        }
    }

    fn extend_from_log_tab_config(&mut self, config: &LogTabKeybindsConfig) {
        update_keybinds!(
            self.keys,
            LogTabEvent::Save => config.save,
            LogTabEvent::Cancel => config.cancel,
            LogTabEvent::ClosePopup => config.close_popup,
            LogTabEvent::ScrollDown => config.scroll_down,
            LogTabEvent::ScrollUp => config.scroll_up,
            LogTabEvent::ScrollDownHalf => config.scroll_down_half,
            LogTabEvent::ScrollUpHalf => config.scroll_up_half,
            LogTabEvent::FocusCurrent => config.focus_current,
            LogTabEvent::ToggleDiffFormat => config.toggle_diff_format,
            LogTabEvent::Refresh => config.refresh,
            LogTabEvent::Duplicate => config.duplicate,
            LogTabEvent::CreateNew { describe: false } => config.create_new,
            LogTabEvent::CreateNew { describe: true } => config.create_new_describe,
            LogTabEvent::InsertNew => config.insert_new,
            LogTabEvent::InsertMove => config.insert_move,
            LogTabEvent::Squash { ignore_immutable: false } => config.squash,
            LogTabEvent::Squash { ignore_immutable: true } => config.squash_ignore_immutable,
            LogTabEvent::Split => config.split,
            LogTabEvent::DiffEdit => config.diffedit,
            LogTabEvent::EditChange { ignore_immutable: false } => config.edit_change,
            LogTabEvent::EditChange { ignore_immutable: true } => config.edit_change_ignore_immutable,
            LogTabEvent::Abandon => config.abandon,
            LogTabEvent::Absorb => config.absorb,
            LogTabEvent::SimplifyParents { include_descendants: false } => config.simplify_parents,
            LogTabEvent::SimplifyParents { include_descendants: true } => config.simplify_parents_descendants,
            LogTabEvent::ResolveConflicts { keep_destination: false } => config.resolve,
            LogTabEvent::ResolveConflicts { keep_destination: true } => config.resolve_destination,
            LogTabEvent::ResolveInEditor => config.resolve_in_editor,
            LogTabEvent::Undo => config.undo,
            LogTabEvent::Redo => config.redo,
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: false } => config.metaedit_update_change_id,
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: true } => config.metaedit_update_change_id_ignore_immutable,
            LogTabEvent::Describe => config.describe,
            LogTabEvent::EditRevset => config.edit_revset,
            LogTabEvent::SetBookmark => config.set_bookmark,
            LogTabEvent::OpenFiles => config.open_files,
            LogTabEvent::CopyChangeId => config.copy_change_id,
            LogTabEvent::CopyRev => config.copy_rev,
            LogTabEvent::Rebase => config.rebase,
            LogTabEvent::RebaseBranch => config.rebase_branch,
            LogTabEvent::Push => config.push,
            LogTabEvent::Fetch { all_remotes: false } => config.fetch,
            LogTabEvent::Fetch { all_remotes: true } => config.fetch_all,
            LogTabEvent::OpenHelp => config.open_help,
        );
    }
    pub fn make_main_panel_help(&self) -> Vec<(String, String)> {
        make_keybinds_help!(
            self.keys,
            LogTabEvent::ScrollDown => "scroll down",
            LogTabEvent::ScrollUp => "scroll up",
            LogTabEvent::ScrollDownHalf => "scroll down by ½ page",
            LogTabEvent::ScrollUpHalf => "scroll up by ½ page",
            LogTabEvent::OpenFiles => "see files",
            LogTabEvent::FocusCurrent => "current change",
            LogTabEvent::Refresh => "refresh the view",
            LogTabEvent::EditRevset => "set revset",
            LogTabEvent::Describe => "describe change",
            LogTabEvent::Duplicate => "duplicate change",
            LogTabEvent::EditChange { ignore_immutable: false } => "edit change",
            LogTabEvent::EditChange { ignore_immutable: true } => "edit change ignoring immutability",
            LogTabEvent::CreateNew { describe: false } => "new change",
            LogTabEvent::CreateNew { describe: true } => "new with message",
            LogTabEvent::InsertNew => "insert a new change",
            LogTabEvent::InsertMove => "insert an existing change",
            LogTabEvent::Abandon => "abandon change",
            LogTabEvent::Absorb => "absorb selected change into its mutable ancestors",
            LogTabEvent::SimplifyParents { include_descendants: false } => "simplify parents of the marked/selected change(s)",
            LogTabEvent::SimplifyParents { include_descendants: true } => "simplify parents of the marked/selected change(s) and descendants",
            LogTabEvent::ResolveConflicts { keep_destination: false } => "resolve conflicts keeping the rebased/squashed revision's version",
            LogTabEvent::ResolveConflicts { keep_destination: true } => "resolve conflicts keeping the rebase/squash destination's version",
            LogTabEvent::ResolveInEditor => "resolve conflicts in the merge editor, file by file",
            LogTabEvent::Undo => "undo last operation",
            LogTabEvent::Redo => "redo last undone operation",
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: false } => "generate a new change id (resolve divergence)",
            LogTabEvent::MetaeditUpdateChangeId { ignore_immutable: true } => "generate a new change id (resolve divergence) ignoring immutability",
            LogTabEvent::Rebase => "rebase change",
            LogTabEvent::RebaseBranch => "rebase branch",
            LogTabEvent::Squash { ignore_immutable: false } => "squash (press again for interactive)",
            LogTabEvent::Squash { ignore_immutable: true } => "squash ignoring immutability",
            LogTabEvent::Split => "split change in two, picking hunks in the diff editor",
            LogTabEvent::DiffEdit => "edit the change's diff in the diff editor",
            LogTabEvent::SetBookmark => "set bookmark",
            LogTabEvent::CopyChangeId => "yank change id to clipboard",
            LogTabEvent::CopyRev => "yank revision to clipboard",
            LogTabEvent::Fetch { all_remotes: false } => "git fetch",
            LogTabEvent::Fetch { all_remotes: true } => "git fetch all remotes",
            LogTabEvent::Push => "git push",
        )
    }
}

#[test]
fn test_log_tab_keybinds_default() {
    let _ = LogTabKeybinds::default();
}
