/*! The dialog module contains all modal dialogs,
previously known as popups.

A Component can launch a dialog by sending
[`ComponentAction::SetPopup(Some(<popup instance>))`](crate::ui::ComponentAction).
Once launched, a dialog will receive all input events from the App,
until it is closed.
*/

mod bookmark_set;
mod command;
mod help;

pub use bookmark_set::BookmarkSetPopup;
pub use command::CommandPopup;
pub use help::HelpPopup;
