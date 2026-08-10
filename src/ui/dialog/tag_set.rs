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

/// Prompt for a tag name and set it on a revision.
///
/// Unlike bookmarks there is no name to generate: a tag name is a release
/// marker the user chooses, so this is just a text field. A name that already
/// exists needs `--allow-move`, which is confirmed rather than assumed, since
/// moving a published tag is not something to do by accident.
pub struct TagSetPopup<'a> {
    commit_id: CommitId,
    name: TextArea<'a>,
    /// Set once the typed name is found to exist; holds that name while the
    /// user confirms moving it.
    confirming_move: Option<String>,
    error: Option<String>,
    tx: std::sync::mpsc::Sender<bool>,
}

impl TagSetPopup<'_> {
    pub fn new(commit_id: CommitId, tx: std::sync::mpsc::Sender<bool>) -> Self {
        // Pre-fill with a tag already on this revision, if any: re-tagging is
        // usually correcting or bumping the existing one, and it saves retyping
        // a version string. The text is selected-as-typed, so typing replaces
        // nothing unexpectedly — the user can edit or clear it.
        let mut name = TextArea::default();
        if let Ok(tags) = new_commander().get_tags_at(commit_id.as_str())
            && let Some(existing) = tags.iter().find(|tag| tag.remote.is_none())
        {
            name.insert_str(&existing.name);
        }

        Self {
            commit_id,
            name,
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
        let area = centered_rect_line_height(area, 40, if self.error.is_some() { 7 } else { 5 });
        f.render_widget(Clear, area);
        f.render_widget(&block, area);

        let constraints = if self.error.is_some() {
            vec![
                Constraint::Fill(1),
                Constraint::Length(2),
                Constraint::Length(2),
            ]
        } else {
            vec![Constraint::Fill(1), Constraint::Length(2)]
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(block.inner(area));

        f.render_widget(&self.name, chunks[0]);

        if let Some(error) = self.error.as_ref() {
            f.render_widget(
                Paragraph::new(error.as_str()).fg(Color::Red).block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray)),
                ),
                chunks[1],
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
