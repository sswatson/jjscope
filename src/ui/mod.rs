pub mod bookmarks_tab;
pub mod commit_show_cache;
pub mod dialog;
pub mod files_tab;
pub mod log_tab;
pub mod panel;
pub mod styles;
pub mod utils;
use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::Event;
use ratatui::layout::Rect;

use crate::commander::log::Head;

pub enum ComponentAction {
    ViewFiles(Head),
    ViewLog(Head),
    ChangeHead(Head),
    SetPopup(Option<Box<dyn Component>>),
    Multiple(Vec<ComponentAction>),
    RefreshTab(),
}

/// When a Component process an input event, it returns an ComponentInputResult
/// which tells the app what to do.
pub enum ComponentInputResult {
    /// The app should stop processing the event
    Handled,
    /// The app should perform the specified ComponentAction.
    HandledAction(ComponentAction),
    /// The app should ask the next component in z-order to handle the event
    NotHandled,
}

impl ComponentInputResult {
    pub fn is_handled(&self) -> bool {
        match self {
            Self::Handled => true,
            Self::HandledAction(_) => true,
            Self::NotHandled => false,
        }
    }
}
pub trait Component {
    // Called when switching to tab
    fn focus(&mut self) -> Result<()> {
        Ok(())
    }

    fn update(&mut self) -> Result<Option<ComponentAction>> {
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()>;

    fn input(&mut self, event: Event) -> Result<ComponentInputResult>;
}
