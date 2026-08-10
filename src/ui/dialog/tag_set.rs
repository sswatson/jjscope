use anyhow::Result;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyModifiers;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui_textarea::TextArea;

use crate::commander::ids::CommitId;
use crate::commander::new_commander;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::styles::create_popup_block;
use crate::ui::utils::centered_rect_line_height;
use crate::ui::utils::error_text;

/// Prompt for a tag name and set it on a revision.
///
/// Unlike bookmarks there is no name to generate: a tag name is a release
/// marker the user chooses, so this is just a text field. A name that already
/// exists needs `--allow-move`, which is confirmed rather than assumed, since
/// moving a published tag is not something to do by accident.
pub struct TagSetPopup<'a> {
    commit_id: CommitId,
    name: TextArea<'a>,
    /// Local tags already on this revision, listed as context. A revision can
    /// carry any number of tags, so these are shown rather than pre-filled:
    /// pre-filling one would make Enter silently *move* it, and would hide the
    /// others entirely.
    existing: Vec<String>,
    /// Set once the typed name is found to exist; holds that name while the
    /// user confirms moving it.
    confirming_move: Option<String>,
    error: Option<String>,
    tx: std::sync::mpsc::Sender<bool>,
}

impl TagSetPopup<'_> {
    pub fn new(commit_id: CommitId, tx: std::sync::mpsc::Sender<bool>) -> Self {
        let existing = new_commander()
            .get_tags_at(commit_id.as_str())
            .map(|tags| {
                tags.into_iter()
                    .filter(|tag| tag.remote.is_none())
                    .map(|tag| tag.name)
                    .collect()
            })
            .unwrap_or_default();

        Self {
            commit_id,
            name: TextArea::default(),
            existing,
            confirming_move: None,
            error: None,
            tx,
        }
    }

    /// Whether a local tag of this name already exists, and so would have to be
    /// moved rather than created.
    fn tag_exists(name: &str) -> bool {
        new_commander()
            .get_tags_list(false)
            .map(|tags| {
                tags.iter()
                    .any(|tag| tag.remote.is_none() && tag.name == name)
            })
            .unwrap_or(false)
    }

    fn set_tag(&mut self, name: &str, allow_move: bool) -> Result<ComponentInputResult> {
        match new_commander().set_tag(name, self.commit_id.as_str(), allow_move) {
            Ok(()) => {
                self.tx.send(true)?;
                // Closing is the popup's own job: the channel only asks the log
                // tab to refresh.
                Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    None,
                )))
            }
            Err(err) => {
                self.error = Some(format!("{err}"));
                self.confirming_move = None;
                Ok(ComponentInputResult::Handled)
            }
        }
    }
}

impl Component for TagSetPopup<'_> {
    fn draw(&mut self, f: &mut ratatui::prelude::Frame<'_>, area: Rect) -> Result<()> {
        if let Some(name) = self.confirming_move.clone() {
            let block = create_popup_block("Move tag");
            let area = centered_rect_line_height(area, 60, 7);
            f.render_widget(Clear, area);
            f.render_widget(&block, area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Fill(1), Constraint::Length(2)])
                .split(block.inner(area));

            let body = Paragraph::new(vec![
                Line::from(format!("The tag \"{name}\" already exists.")),
                Line::from(""),
                Line::from("Move it to this revision?"),
            ]);
            f.render_widget(body, chunks[0]);

            let help = Paragraph::new(vec!["y: move | Escape: cancel".into()])
                .fg(Color::DarkGray)
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
            f.render_widget(help, chunks[1]);
            return Ok(());
        }

        let block = create_popup_block("Set tag");
        // Grow for whichever optional sections are present.
        let existing_lines = if self.existing.is_empty() { 0 } else { 3 };
        // Size to the message plus its top border. jj errors are often several
        // lines — an "Error:" line, a "Hint:" with the actionable advice, and
        // sometimes a parse trace — and the hint is the half worth reading.
        // Capped so a long trace cannot push the popup past the screen.
        let error_lines = self
            .error
            .as_deref()
            .map(|error| error.lines().count().clamp(1, 6) as u16 + 1)
            .unwrap_or(0);
        let area = centered_rect_line_height(area, 60, 5 + existing_lines + error_lines);
        f.render_widget(Clear, area);
        f.render_widget(&block, area);

        let mut constraints = vec![Constraint::Fill(1)];
        if existing_lines > 0 {
            constraints.push(Constraint::Length(existing_lines));
        }
        if error_lines > 0 {
            constraints.push(Constraint::Length(error_lines));
        }
        constraints.push(Constraint::Length(2));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(block.inner(area));

        f.render_widget(&self.name, chunks[0]);
        let mut next = 1;

        if existing_lines > 0 {
            // Naming one of these moves it here rather than creating a tag,
            // which the confirmation step spells out.
            let listed = self.existing.join("  ");
            f.render_widget(
                Paragraph::new(vec![
                    Line::from("Already on this revision:").fg(Color::DarkGray),
                    Line::from(listed).fg(Color::Magenta),
                ])
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray)),
                ),
                chunks[next],
            );
            next += 1;
        }

        if let Some(error) = self.error.as_ref() {
            f.render_widget(
                Paragraph::new(error_text(error)).block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray)),
                ),
                chunks[next],
            );
        }

        let help = Paragraph::new(vec!["Ctrl+s/Enter: set | Escape: cancel".into()])
            .fg(Color::DarkGray)
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        f.render_widget(help, chunks[chunks.len() - 1]);

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        let Event::Key(key) = event else {
            return Ok(ComponentInputResult::Handled);
        };

        if let Some(name) = self.confirming_move.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    return self.set_tag(&name, true);
                }
                // Back out of the confirmation to the name field, so the name
                // can be corrected rather than retyped from scratch.
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.confirming_move = None;
                }
                KeyCode::Esc => {
                    self.tx.send(false)?;
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        None,
                    )));
                }
                _ => (),
            }
            return Ok(ComponentInputResult::Handled);
        }

        match key.code {
            _ if (key.code == KeyCode::Char('s')
                && key.modifiers.contains(KeyModifiers::CONTROL))
                || key.code == KeyCode::Enter =>
            {
                let name = self.name.lines().join("\n");
                let name = name.trim();
                if name.is_empty() {
                    return Ok(ComponentInputResult::Handled);
                }
                self.error = None;
                if Self::tag_exists(name) {
                    self.confirming_move = Some(name.to_owned());
                    return Ok(ComponentInputResult::Handled);
                }
                return self.set_tag(name, false);
            }
            KeyCode::Esc => {
                self.tx.send(false)?;
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    None,
                )));
            }
            _ => {
                self.name.input(key);
            }
        }

        Ok(ComponentInputResult::Handled)
    }
}
