#![expect(clippy::borrow_interior_mutable_const)]

//! Tags tab. Lists the repo's tags in the main panel and shows the tagged
//! revision in the details panel.
//!
//! Deliberately smaller than the bookmarks tab: tags are set from the log tab
//! (where you can see which revision you are tagging), so this tab covers
//! browsing, deleting, and the remote tracking that jj 0.44 added.

use ansi_to_tui::IntoText;
use anyhow::Result;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::prelude::*;
use ratatui::widgets::*;
use tracing::instrument;
use tui_confirm_dialog::ButtonLabel;
use tui_confirm_dialog::ConfirmDialog;
use tui_confirm_dialog::ConfirmDialogState;
use tui_confirm_dialog::Listener;

use crate::commander::CommandError;
use crate::commander::new_commander;
use crate::commander::tags::TagLine;
use crate::env::DiffFormat;
use crate::env::JjConfig;
use crate::env::get_env;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::dialog::HelpPopup;
use crate::ui::dialog::MessagePopup;
use crate::ui::panel::DetailsPanel;
use crate::ui::panel::TextContent;
use crate::ui::utils::PaneDivider;
use crate::ui::utils::tabs_to_spaces;

const DELETE_POPUP_ID: u16 = 1;

pub struct TagsTab {
    tags_output: Result<Vec<TagLine>, CommandError>,
    tags_list_state: ListState,
    tags_height: u16,

    /// Whether remote tags are listed alongside local ones (`--all-remotes`).
    show_all: bool,

    tag: Option<TagLine>,
    tag_panel: DetailsPanel,
    tag_output: Option<Result<String, CommandError>>,

    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    diff_format: DiffFormat,
    config: JjConfig,
    pane_divider: PaneDivider,
}

/// Two tag lines refer to the same tag when their name and remote agree.
fn tag_lines_match(a: &TagLine, b: &TagLine) -> bool {
    match (a, b) {
        (TagLine::Parsed { tag: a, .. }, TagLine::Parsed { tag: b, .. }) => {
            a.name == b.name && a.remote == b.remote
        }
        _ => false,
    }
}

fn current_tag_index(
    current: Option<&TagLine>,
    tags_output: &Result<Vec<TagLine>, CommandError>,
) -> Option<usize> {
    let current = current?;
    let tags = tags_output.as_ref().ok()?;
    tags.iter().position(|tag| tag_lines_match(current, tag))
}

impl TagsTab {
    #[instrument(level = "info", name = "Initializing tags tab", parent = None, skip())]
    pub fn new() -> Result<Self> {
        let diff_format = get_env().jj_config.diff_format();
        let show_all = false;

        let tags_output = new_commander().get_tags(show_all);
        let tag = tags_output
            .as_ref()
            .ok()
            .and_then(|tags| tags.first())
            .cloned();

        let tags_list_state =
            ListState::default().with_selected(current_tag_index(tag.as_ref(), &tags_output));

        let (popup_tx, popup_rx) = std::sync::mpsc::channel();
        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        let mut tab = Self {
            tags_output,
            tags_list_state,
            tags_height: 0,
            show_all,
            tag,
            tag_panel: DetailsPanel::new(),
            tag_output: None,
            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,
            diff_format,
            config,
            pane_divider,
        };
        tab.refresh_tag();
        Ok(tab)
    }

    pub fn refresh_tags(&mut self) {
        self.tags_output = new_commander().get_tags(self.show_all);
        // The selected tag may have been deleted or filtered out; fall back to
        // the first one so the details panel never shows a stale revision.
        if current_tag_index(self.tag.as_ref(), &self.tags_output).is_none() {
            self.tag = self
                .tags_output
                .as_ref()
                .ok()
                .and_then(|tags| tags.first())
                .cloned();
        }
    }

    pub fn refresh_tag(&mut self) {
        let mut commander = new_commander();
        let inner_width = self.tag_panel.columns() as usize;
        commander.limit_width(inner_width);
        self.tag_output = self.tag.as_ref().and_then(|tag| match tag {
            TagLine::Parsed { tag, .. } => Some(
                commander
                    .get_tag_show(tag, &self.diff_format, true)
                    .map(|show| tabs_to_spaces(&show)),
            ),
            _ => None,
        });
        self.tag_panel.scroll_to(0);
    }

    fn all_tags(&self) -> Vec<TagLine> {
        self.tags_output.as_ref().cloned().unwrap_or_default()
    }

    fn scroll_tags(&mut self, scroll: isize) {
        let tags = self.all_tags();
        if tags.is_empty() {
            return;
        }
        let index = match current_tag_index(self.tag.as_ref(), &self.tags_output) {
            Some(index) => index
                .saturating_add_signed(scroll)
                .min(tags.len().saturating_sub(1)),
            None => 0,
        };
        self.tag = tags.get(index).cloned();
        self.refresh_tag();
    }

    /// The selected tag's data, if the line was parsable.
    fn selected_tag(&self) -> Option<&crate::commander::tags::Tag> {
        match self.tag.as_ref()? {
            TagLine::Parsed { tag, .. } => Some(tag),
            TagLine::Unparsable(_) => None,
        }
    }

    fn confirm_delete(&mut self) -> ComponentInputResult {
        let Some(tag) = self.selected_tag() else {
            return ComponentInputResult::Handled;
        };
        if tag.remote.is_some() {
            return ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new(
                    "Delete tag",
                    "Only local tags can be deleted. Use untrack (T) to stop following a remote tag.",
                ),
            ))));
        }

        let text = Text::from(vec![
            Line::from(format!("Delete the tag \"{}\"?", tag.name)),
            Line::from(""),
            Line::from("The tagged revision itself is kept."),
        ])
        .fg(Color::default());

        self.popup = ConfirmDialogState::new(
            DELETE_POPUP_ID,
            Span::styled(" Delete tag ", Style::new().bold().cyan()),
            text,
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
        ComponentInputResult::Handled
    }

    fn execute_delete(&mut self) -> Result<Option<AppAction>> {
        let Some(name) = self.selected_tag().map(|tag| tag.name.clone()) else {
            return Ok(None);
        };
        match new_commander().delete_tag(&name) {
            Ok(()) => {
                self.refresh_tags();
                self.refresh_tag();
                Ok(Some(AppAction::SetStatusMessage(format!(
                    "Deleted tag {name} | u: undo"
                ))))
            }
            Err(err) => Ok(Some(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new("Delete tag", format!("{err}")),
            ))))),
        }
    }

    /// Start or stop tracking the selected remote tag.
    fn set_tracking(&mut self, track: bool) -> ComponentInputResult {
        let Some(tag) = self.selected_tag() else {
            return ComponentInputResult::Handled;
        };
        let Some(remote_ref) = tag.remote_ref() else {
            return ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new(
                    if track { "Track tag" } else { "Untrack tag" },
                    "Select a remote tag (shown with `a`) to change its tracking.",
                ),
            ))));
        };

        let commander = new_commander();
        let result = if track {
            commander.track_tag(&remote_ref)
        } else {
            commander.untrack_tag(&remote_ref)
        };

        match result {
            Ok(_) => {
                self.refresh_tags();
                self.refresh_tag();
                let verb = if track {
                    "Tracking"
                } else {
                    "No longer tracking"
                };
                ComponentInputResult::HandledAction(AppAction::SetStatusMessage(format!(
                    "{verb} {remote_ref}"
                )))
            }
            Err(err) => ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(
                MessagePopup::new(
                    if track { "Track tag" } else { "Untrack tag" },
                    format!("{err}"),
                ),
            )))),
        }
    }
}

impl Component for TagsTab {
    fn focus(&mut self) -> Result<()> {
        self.refresh_tags();
        self.refresh_tag();
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        if let Ok(res) = self.popup_rx.try_recv()
            && res.1.unwrap_or(false)
            && res.0 == DELETE_POPUP_ID
        {
            return self.execute_delete();
        }
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        {
            let current_index = current_tag_index(self.tag.as_ref(), &self.tags_output);

            let lines: Vec<Line> = match self.tags_output.as_ref() {
                Ok(tags) if tags.is_empty() => {
                    vec![Line::from(" No tags").fg(Color::DarkGray).italic()]
                }
                Ok(tags) => tags
                    .iter()
                    .enumerate()
                    .flat_map(|(i, tag)| {
                        tag.to_text()
                            .unwrap_or_default()
                            .iter()
                            .map(|line| {
                                let mut line = line.to_owned();
                                line.spans.insert(0, Span::from(" "));
                                if current_index == Some(i) {
                                    line = line.bg(self.config.highlight_color());
                                    line.spans = line
                                        .spans
                                        .iter_mut()
                                        .map(|span| {
                                            span.to_owned().bg(self.config.highlight_color())
                                        })
                                        .collect();
                                }
                                line
                            })
                            .collect::<Vec<Line>>()
                    })
                    .collect(),
                Err(err) => err.into_text("Error getting tags")?.lines,
            };

            let title = if self.show_all {
                " Tags (all remotes) "
            } else {
                " Tags "
            };
            let list = List::new(lines)
                .block(
                    Block::bordered()
                        .title(title)
                        .border_type(BorderType::Rounded),
                )
                .scroll_padding(3);
            *self.tags_list_state.selected_mut() = current_index;
            f.render_stateful_widget(&list, chunks[0], &mut self.tags_list_state);
            self.tags_height = chunks[0].height.saturating_sub(2);
        }

        {
            let content = match self.tag_output.as_ref() {
                Some(Ok(show)) => show.into_text()?,
                Some(Err(err)) => err.into_text("Error getting tag")?,
                None => Text::default(),
            };
            self.tag_panel
                .render_context::<TextContent>(content)
                .title(" Details ")
                .draw(f, chunks[1]);
        }

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

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        let Event::Key(key) = event else {
            return Ok(ComponentInputResult::NotHandled);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(ComponentInputResult::Handled);
        }

        if self.popup.is_opened() {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.popup = ConfirmDialogState::default();
            } else {
                self.popup.handle(&key);
            }
            return Ok(ComponentInputResult::Handled);
        }

        if self.tag_panel.input(key) {
            return Ok(ComponentInputResult::Handled);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll_tags(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_tags(-1),
            KeyCode::Char('J') => self.scroll_tags(self.tags_height as isize / 2),
            KeyCode::Char('K') => {
                self.scroll_tags((self.tags_height as isize / 2).saturating_neg())
            }
            KeyCode::Char('a') => {
                self.show_all = !self.show_all;
                self.refresh_tags();
                self.refresh_tag();
            }
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.refresh_tags();
                self.refresh_tag();
            }
            KeyCode::Char('w') => {
                self.diff_format = self.diff_format.get_next(self.config.diff_tool());
                self.refresh_tag();
            }
            KeyCode::Char('d') => return Ok(self.confirm_delete()),
            KeyCode::Char('t') => return Ok(self.set_tracking(true)),
            KeyCode::Char('T') => return Ok(self.set_tracking(false)),
            KeyCode::Enter => {
                // Jump to the tagged revision on the log tab, where it can
                // actually be seen in context.
                if let Some(tag) = self.selected_tag() {
                    match new_commander().get_head(&tag.to_string()) {
                        Ok(head) => {
                            return Ok(ComponentInputResult::HandledAction(AppAction::ViewLog(
                                head,
                            )));
                        }
                        Err(err) => {
                            return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                                Some(Box::new(MessagePopup::new("Show tag", format!("{err}")))),
                            )));
                        }
                    }
                }
            }
            KeyCode::Char('?') => {
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(HelpPopup::new(
                        vec![
                            ("j/k".to_owned(), "scroll down/up".to_owned()),
                            ("J/K".to_owned(), "scroll down/up by ½ page".to_owned()),
                            (
                                "Enter".to_owned(),
                                "show the tagged revision on the log tab".to_owned(),
                            ),
                            (
                                "a".to_owned(),
                                "toggle showing remote tags as well as local ones".to_owned(),
                            ),
                            (
                                "d".to_owned(),
                                "delete the selected local tag (the revision is kept)".to_owned(),
                            ),
                            (
                                "t".to_owned(),
                                "track the selected remote tag with a local tag".to_owned(),
                            ),
                            (
                                "T".to_owned(),
                                "stop tracking the selected remote tag".to_owned(),
                            ),
                            ("R".to_owned(), "refresh the view".to_owned()),
                        ],
                        vec![
                            ("Ctrl+e/Ctrl+y".to_owned(), "scroll down/up".to_owned()),
                            (
                                "Ctrl+d/Ctrl+u".to_owned(),
                                "scroll down/up by ½ page".to_owned(),
                            ),
                            ("w".to_owned(), "toggle diff format".to_owned()),
                        ],
                    ))),
                )));
            }
            _ => return Ok(ComponentInputResult::NotHandled),
        }

        Ok(ComponentInputResult::Handled)
    }
}
