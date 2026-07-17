/*! The rebase popup allows the user to pick a rebase configuration and
 start rebase, or cancel the opreation.

 The source is the change selected in the log panel; the targets are the
 marked changes. The UI looks like this
 ~~~
    Source zsztoxlv 093ab72d
    ( ) -s this and descendants
    ( ) -b whole branch
    (*) -r only one change moves
    Target umrpslui 45a99ab4
    (*) -d rebase onto target as new branch
    ( ) -A rebase after target
    ( ) -B rebase before target

    Esc: Cancel    Enter: Rebase
~~~
It has keyboard shortcuts s, b, r, d, shift+a, shift+b for selecting
a radiobutton, and shortcuts Enter, Esc, q for closing the popup.


*/

use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::Event;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::prelude::Buffer;
use ratatui::prelude::Constraint;
use ratatui::prelude::Direction;
use ratatui::prelude::Layout;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidget;

use crate::commander::ids::CommitId;
use crate::commander::log::Head;
use crate::commander::new_commander;
use crate::keybinds::rebase_popup::CutOption;
use crate::keybinds::rebase_popup::PasteOption;
use crate::keybinds::rebase_popup::PopupAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::utils::centered_rect_fixed;

type Keybinds = crate::keybinds::rebase_popup::Keybinds;

/// How the rebase popup was closed, so the caller knows whether the marked
/// changes were consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebasePopupExit {
    /// The popup handled the input and stays open.
    KeepOpen,
    /// The popup was dismissed without rebasing.
    Cancelled,
    /// The rebase ran.
    Executed,
}

/// A transient popup for configuring a rebase command
pub struct RebasePopup {
    pub keybinds: Keybinds,

    pub source_rev: Head,
    pub target_revs: Vec<CommitId>,

    pub source_mode: CutOption,
    pub target_mode: PasteOption,
}

impl RebasePopup {
    pub fn new(source_rev: Head, target_revs: Vec<CommitId>) -> Self {
        Self {
            keybinds: Keybinds::default(),
            source_rev,
            target_revs,
            source_mode: CutOption::SingleRevision,
            target_mode: PasteOption::NewBranch,
        }
    }

    /// Collect all the rendering code that would have been in
    /// log_tab.rs/draw
    pub fn render_widget(&mut self, frame: &mut Frame) {
        let area = centered_rect_fixed(frame.area(), 32, 12);
        self.draw(frame, area)
            .expect("Expected drawing without failues");
    }

    /// Map an event to a popup action
    // TODO: This should be done by a custom keybinds object
    fn match_event(&self, event: Event) -> PopupAction {
        if let Event::Key(key) = event {
            return self.keybinds.match_event(key);
        }
        PopupAction::None
    }

    /// Run the command that the popup is currently configured to do
    fn run_command(&self) -> Result<()> {
        let src_rev = self.source_rev.commit_id.as_str();
        let src_mode = match self.source_mode {
            CutOption::IncludeDescendants => "-s",
            CutOption::IncludeBranch => "-b",
            CutOption::SingleRevision => "-r",
        };
        let tgt_mode = match self.target_mode {
            PasteOption::NewBranch => "-d",
            PasteOption::InsertAfter => "-A",
            PasteOption::InsertBefore => "-B",
        };
        new_commander().run_rebase(src_mode, src_rev, tgt_mode, &self.target_revs)?;
        Ok(())
    }

    /// Process the input event. On [RebasePopupExit::Cancelled] or
    /// [RebasePopupExit::Executed] the popup should be closed; on
    /// [RebasePopupExit::KeepOpen] the input was handled (possibly changing a
    /// radio button) and the popup stays up.
    /// Err(_) will be returned if the jj command failed.
    pub fn handle_input(&mut self, event: Event) -> Result<RebasePopupExit> {
        match self.match_event(event) {
            PopupAction::Ok => {
                self.run_command()?;
                return Ok(RebasePopupExit::Executed);
            }
            PopupAction::Cancel => return Ok(RebasePopupExit::Cancelled),
            PopupAction::SetSourceMode(m) => self.source_mode = m,
            PopupAction::SetTargetMode(m) => self.target_mode = m,
            PopupAction::None => (),
        }
        Ok(RebasePopupExit::KeepOpen)
    }
}

impl Component for RebasePopup {
    /// Render the dialog into the area.
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) -> Result<()> {
        // The border of the dialog
        let block = Block::bordered()
            .title(Span::styled(" Rebase ", Style::new().bold().cyan()))
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Green));
        frame.render_widget(Clear, area);
        frame.render_widget(&block, area);

        // Split area into chunks. Even though the area size is constant,
        // we pretend it can change in the future.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(1)
            .horizontal_margin(2)
            .constraints(
                [
                    Constraint::Length(1), // title "Source"
                    Constraint::Min(3),    // buttons for source mode
                    Constraint::Length(1), // title "Target"
                    Constraint::Min(3),    // buttons for target mode
                    Constraint::Length(2), // help text
                ]
                .as_ref(),
            )
            .split(area);

        // Radio buttons for source
        let src_change_id: String = self.source_rev.change_id.as_str().chars().take(8).collect();
        let src_commit_id: String = self.source_rev.commit_id.as_str().chars().take(8).collect();
        let src_options = vec![
            "-s this and descendants",
            "-b whole branch",
            "-r only one change moves",
        ];
        let mut src_select: usize = match self.source_mode {
            CutOption::IncludeDescendants => 0,
            CutOption::IncludeBranch => 1,
            CutOption::SingleRevision => 2,
        };
        frame.render_widget(
            Paragraph::new(Span::raw(format!("Source {src_change_id} {src_commit_id}"))),
            chunks[0],
        );
        frame.render_stateful_widget(RadioButton::new(src_options), chunks[1], &mut src_select);

        // Radio buttons for target
        let target_label = match self.target_revs.as_slice() {
            [single] => {
                let commit_id: String = single.as_str().chars().take(8).collect();
                format!("Target {commit_id}")
            }
            targets => format!("Target {} marked changes", targets.len()),
        };
        let tgt_options = vec![
            "-d rebase as new branch",
            "-A rebase after",
            "-B rebase before",
        ];
        let mut tgt_select: usize = match self.target_mode {
            PasteOption::NewBranch => 0,
            PasteOption::InsertAfter => 1,
            PasteOption::InsertBefore => 2,
        };
        frame.render_widget(Paragraph::new(Span::raw(target_label)), chunks[2]);
        frame.render_stateful_widget(RadioButton::new(tgt_options), chunks[3], &mut tgt_select);

        // Help on terminating dialog
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::raw(""),
                Line::raw("Esc: Cancel    Enter: Rebase"),
            ])),
            chunks[4],
        );

        Ok(())
    }

    fn input(&mut self, _event: Event) -> Result<ComponentInputResult> {
        unreachable!();
        //return Ok(ComponentInputResult::Handled);
    }
}

/****************************************************************/
// TODO(@peso): Move this widget to a separate file

/** A widget for a group of radio buttons.

It is a stateful widget.
The state is an usize number that indicates which label is
selected.

Example:
~~~
( ) apples
( ) bananas
(*) lemons
~~~
*/
struct RadioButton {
    /// Button labels
    pub labels: Vec<String>,
    /// Button style can be modified before drawing
    pub button_style: Style,
    /// Label style can be modified before drawing
    pub label_style: Style,
}

impl RadioButton {
    pub fn new(labels: Vec<&str>) -> Self {
        let button_style = Style::default().fg(Color::White);
        let label_style = Style::default().fg(Color::White);
        Self {
            labels: labels.iter().map(|s| s.to_string()).collect(),
            button_style,
            label_style,
        }
    }
}

impl StatefulWidget for RadioButton {
    type State = usize;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        for (row, label) in self.labels.iter().enumerate() {
            let button = if row == *state { "(*)" } else { "( )" };
            buf.set_string(
                area.left(),
                area.top() + row as u16,
                button,
                self.button_style,
            );
            buf.set_string(
                area.left() + 4_u16,
                area.top() + row as u16,
                label,
                self.label_style,
            );
        }
    }
}
