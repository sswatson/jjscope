#![expect(clippy::borrow_interior_mutable_const)]

//! Remotes tab. Lists the repo's Git remotes with their URLs and how their
//! bookmarks compare to the local ones, and offers fetch, push, and remote
//! management. The details panel shows the selected remote's bookmarks with
//! jj's own ahead/behind annotations.
//!
//! Push is deliberately narrow: it pushes the tracked bookmarks (`jj git push
//! --tracked`), previews what that would do with `--dry-run` first, and asks
//! before doing it. Anything more specific belongs on the bookmarks tab, where
//! you can see which bookmark you are pushing.
//!
//! See [crate::commander::remotes] for what the statistics mean.

use anyhow::Result;
use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::event::KeyModifiers;
use ratatui::prelude::*;
use ratatui::widgets::*;
use ratatui_textarea::CursorMove;
use ratatui_textarea::TextArea;
use tracing::instrument;
use tui_confirm_dialog::ButtonLabel;
use tui_confirm_dialog::ConfirmDialog;
use tui_confirm_dialog::ConfirmDialogState;
use tui_confirm_dialog::Listener;

use crate::commander::CommandError;
use crate::commander::gh_account::PushIdentity;
use crate::commander::new_commander;
use crate::commander::remotes::Remote;
use crate::commander::remotes::bookmarks_in_push_preview;
use crate::env::JjConfig;
use crate::env::get_env;
use crate::ui::AppAction;
use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::dialog::HelpPopup;
use crate::ui::dialog::LoaderPopup;
use crate::ui::dialog::MessagePopup;
use crate::ui::panel::DetailsPanel;
use crate::ui::panel::TextContent;
use crate::ui::utils::PaneDivider;
use crate::ui::utils::error_text;
use crate::ui::utils::summarize_names;

const REMOVE_POPUP_ID: u16 = 1;
const PUSH_POPUP_ID: u16 = 2;

/// What a confirmed dialog will act on, captured when it opened so that a
/// refresh cannot change the target while the dialog is up.
enum Pending {
    Remove(String),
    /// The remote, the account, and the bookmarks the preview said would move.
    Push(String, PushIdentity, Vec<String>),
}

#[derive(Clone, Copy, PartialEq)]
enum PromptKind {
    Add,
    Rename,
    SetUrl,
}

impl PromptKind {
    fn title(self) -> &'static str {
        match self {
            PromptKind::Add => "Add remote",
            PromptKind::Rename => "Rename remote",
            PromptKind::SetUrl => "Set remote URL",
        }
    }

    fn label(self) -> &'static str {
        match self {
            PromptKind::Add => "Name and URL, separated by a space:",
            PromptKind::Rename => "New name for",
            PromptKind::SetUrl => "New URL for",
        }
    }
}

/// A text prompt shown over the remote list.
struct Prompt<'a> {
    kind: PromptKind,
    /// The remote the prompt acts on (empty when adding).
    remote: String,
    textarea: TextArea<'a>,
    error: Option<String>,
}

/// Remotes tab. Shows remotes in the main panel and the selected remote's
/// bookmarks in the details panel.
pub struct RemotesTab<'a> {
    remotes_output: Result<Vec<Remote>, CommandError>,
    remotes_list_state: ListState,
    remotes_height: u16,

    /// The selected remote, by name so the selection survives a refresh.
    selected: Option<String>,
    remote_panel: DetailsPanel,
    remote_output: Option<Result<String, CommandError>>,

    prompt: Option<Prompt<'a>>,

    pending: Option<Pending>,
    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    config: JjConfig,
    pane_divider: PaneDivider,
}

fn remote_index(selected: Option<&str>, remotes: &[Remote]) -> Option<usize> {
    let selected = selected?;
    remotes.iter().position(|remote| remote.name == selected)
}

/// The two lines that describe a remote in the list.
fn remote_item(remote: &Remote) -> ListItem<'static> {
    let stats = &remote.stats;
    let mut summary = vec![Span::raw("   ")];
    if stats.bookmarks == 0 {
        summary.push(
            Span::raw("no bookmarks fetched")
                .fg(Color::DarkGray)
                .italic(),
        );
    } else {
        let mut counts = format!(
            "{} {}, {} tracked",
            stats.bookmarks,
            if stats.bookmarks == 1 {
                "bookmark"
            } else {
                "bookmarks"
            },
            stats.tracked
        );
        if remote.untracked() > 0 {
            counts.push_str(&format!(", {} untracked", remote.untracked()));
        }
        summary.push(Span::raw(counts).fg(Color::DarkGray));
        if stats.tracked > 0 {
            summary.push(Span::raw("  "));
            if stats.out_of_sync == 0 {
                summary.push(Span::raw("✓ in sync").fg(Color::Green));
            } else {
                summary.push(
                    Span::raw(format!("{} out of sync", stats.out_of_sync)).fg(Color::Yellow),
                );
                if !stats.ahead.is_zero() {
                    summary.push(Span::raw(format!(" ↑{}", stats.ahead)).fg(Color::Green));
                }
                if !stats.behind.is_zero() {
                    summary.push(Span::raw(format!(" ↓{}", stats.behind)).fg(Color::Red));
                }
            }
        }
    }

    ListItem::new(Text::from(vec![
        Line::from(vec![
            Span::raw(" "),
            Span::raw(remote.name.clone()).bold().cyan(),
            Span::raw("  "),
            Span::raw(remote.url.clone()),
        ]),
        Line::from(summary),
    ]))
}

impl RemotesTab<'_> {
    #[instrument(level = "info", name = "Initializing remotes tab", parent = None, skip())]
    pub fn new() -> Result<Self> {
        let (popup_tx, popup_rx) = std::sync::mpsc::channel();
        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        let mut tab = Self {
            remotes_output: Ok(vec![]),
            remotes_list_state: ListState::default(),
            remotes_height: 0,
            selected: None,
            remote_panel: DetailsPanel::new(),
            remote_output: None,
            prompt: None,
            pending: None,
            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,
            config,
            pane_divider,
        };
        tab.refresh_remotes();
        tab.refresh_remote();
        Ok(tab)
    }

    fn remotes(&self) -> &[Remote] {
        self.remotes_output.as_deref().unwrap_or_default()
    }

    fn selected_remote(&self) -> Option<&Remote> {
        let remotes = self.remotes();
        remote_index(self.selected.as_deref(), remotes).map(|i| &remotes[i])
    }

    /// Reload the list. The selection stays on the same remote when it still
    /// exists, and falls back to the first one otherwise.
    pub fn refresh_remotes(&mut self) {
        self.remotes_output = new_commander().get_remotes();
        if remote_index(self.selected.as_deref(), self.remotes()).is_none() {
            self.selected = self.remotes().first().map(|remote| remote.name.clone());
        }
    }

    pub fn refresh_remote(&mut self) {
        self.remote_output = self
            .selected_remote()
            .map(|remote| new_commander().get_remote_bookmarks(&remote.name));
        self.remote_panel.scroll_to(0);
    }

    fn scroll_remotes(&mut self, scroll: isize) {
        let remotes = self.remotes();
        if remotes.is_empty() {
            return;
        }
        let index = match remote_index(self.selected.as_deref(), remotes) {
            Some(index) => index
                .saturating_add_signed(scroll)
                .min(remotes.len().saturating_sub(1)),
            None => 0,
        };
        self.selected = Some(remotes[index].name.clone());
        self.refresh_remote();
    }

    fn popup_message(title: &'static str, message: impl Into<String>) -> ComponentInputResult {
        ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(MessagePopup::new(
            title,
            message.into(),
        )))))
    }

    /// Fetch from one remote, or from all of them, with a spinner. The loader
    /// refreshes the tab when it finishes.
    fn fetch(&self, all: bool) -> ComponentInputResult {
        if all {
            let loader =
                LoaderPopup::new("Fetching".to_owned(), || new_commander().git_fetch(true));
            return ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(
                loader,
            ))));
        }
        let Some(name) = self.selected_remote().map(|remote| remote.name.clone()) else {
            return ComponentInputResult::Handled;
        };
        let loader = LoaderPopup::new(format!("Fetching {name}"), move || {
            new_commander().git_fetch_remote(&name)
        });
        ComponentInputResult::HandledAction(AppAction::SetPopup(Some(Box::new(loader))))
    }

    /// Preview a push of the selected remote's tracked bookmarks and ask
    /// before doing it.
    fn confirm_push(&mut self) -> ComponentInputResult {
        let Some(name) = self.selected_remote().map(|remote| remote.name.clone()) else {
            return ComponentInputResult::Handled;
        };
        let commander = new_commander();
        let identity = commander.push_identity(&name);
        let preview = match commander.git_push_remote(&name, true, &identity) {
            Ok(preview) => preview,
            Err(err) => return Self::popup_message("Push", format!("{err}")),
        };
        // jj's own words say why: a plain "Nothing changed." or warnings such
        // as an undescribed commit it refuses to push.
        if preview.contains("Nothing changed") {
            return Self::popup_message("Push", preview);
        }

        let mut lines = vec![
            Line::from(format!(
                "{}?",
                identity.label(&format!("Push tracked bookmarks to {name}"))
            )),
            Line::from(""),
        ];
        // The trailer is jj announcing the dry run, which the dialog makes moot.
        let preview = error_text(
            preview
                .replace("Dry-run requested, not pushing.", "")
                .trim_end(),
        );
        let plain = preview
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let bookmarks = bookmarks_in_push_preview(&plain);
        lines.extend(preview.lines);
        self.pending = Some(Pending::Push(name, identity, bookmarks));
        self.open_popup(PUSH_POPUP_ID, " Push ", Text::from(lines));
        ComponentInputResult::Handled
    }

    fn confirm_remove(&mut self) -> ComponentInputResult {
        let Some(remote) = self.selected_remote() else {
            return ComponentInputResult::Handled;
        };
        let mut lines = vec![
            Line::from(format!("Remove the remote \"{}\"?", remote.name)),
            Line::from(""),
        ];
        if remote.stats.bookmarks > 0 {
            lines.push(Line::from(format!(
                "Its {} remote bookmark(s) are forgotten here.",
                remote.stats.bookmarks
            )));
        }
        lines.push(Line::from("Nothing on the server is touched."));
        let name = remote.name.clone();
        self.pending = Some(Pending::Remove(name));
        self.open_popup(REMOVE_POPUP_ID, " Remove ", Text::from(lines));
        ComponentInputResult::Handled
    }

    fn open_popup(&mut self, id: u16, title: &str, text: Text<'static>) {
        self.popup = ConfirmDialogState::new(
            id,
            Span::styled(title.to_owned(), Style::new().bold().cyan()),
            text.fg(Color::default()),
        );
        self.popup
            .with_yes_button(ButtonLabel::YES.clone())
            .with_no_button(ButtonLabel::NO.clone())
            .with_listener(Some(self.popup_tx.clone()))
            .open();
    }

    fn start_prompt(&mut self, kind: PromptKind) -> ComponentInputResult {
        let mut textarea = TextArea::default();
        let remote = if kind == PromptKind::Add {
            String::new()
        } else {
            let Some(remote) = self.selected_remote() else {
                return ComponentInputResult::Handled;
            };
            textarea.insert_str(match kind {
                PromptKind::Rename => &remote.name,
                _ => &remote.url,
            });
            textarea.move_cursor(CursorMove::End);
            remote.name.clone()
        };
        self.prompt = Some(Prompt {
            kind,
            remote,
            textarea,
            error: None,
        });
        ComponentInputResult::Handled
    }

    /// Apply the open prompt. Returns the status message on success.
    fn submit_prompt(&mut self) -> Option<String> {
        let prompt = self.prompt.as_mut()?;
        let input = prompt.textarea.lines().join(" ");
        let input = input.trim().to_owned();
        let kind = prompt.kind;
        let remote = prompt.remote.clone();
        let commander = new_commander();

        // `select` is the remote to land on afterwards.
        let (result, select) = match kind {
            PromptKind::Add => {
                let mut words = input.split_whitespace();
                let (Some(name), Some(url), None) = (words.next(), words.next(), words.next())
                else {
                    prompt.error = Some("Enter a name and a URL, e.g. origin https://…".to_owned());
                    return None;
                };
                (
                    commander
                        .add_remote(name, url)
                        .map(|()| format!("Added remote {name} | u: undo")),
                    name.to_owned(),
                )
            }
            PromptKind::Rename => {
                if input.is_empty() || input.contains(char::is_whitespace) {
                    prompt.error =
                        Some("A remote name cannot be empty or contain spaces".to_owned());
                    return None;
                }
                (
                    commander
                        .rename_remote(&remote, &input)
                        .map(|()| format!("Renamed remote {remote} to {input} | u: undo")),
                    input.clone(),
                )
            }
            PromptKind::SetUrl => {
                if input.is_empty() {
                    prompt.error = Some("A URL cannot be empty".to_owned());
                    return None;
                }
                (
                    commander
                        .set_remote_url(&remote, &input)
                        .map(|()| format!("Set the URL of {remote} | u: undo")),
                    remote.clone(),
                )
            }
        };

        match result {
            Ok(message) => {
                self.prompt = None;
                self.selected = Some(select);
                self.refresh_remotes();
                self.refresh_remote();
                Some(message)
            }
            Err(err) => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.error = Some(format!("{err}"));
                }
                None
            }
        }
    }

    fn execute_remove(&mut self, name: &str) -> Option<AppAction> {
        match new_commander().remove_remote(name) {
            Ok(()) => {
                self.refresh_remotes();
                self.refresh_remote();
                Some(AppAction::SetStatusMessage(format!(
                    "Removed remote {name} | u: undo"
                )))
            }
            Err(err) => Some(AppAction::SetPopup(Some(Box::new(MessagePopup::new(
                "Remove remote",
                format!("{err}"),
            ))))),
        }
    }
}

impl Component for RemotesTab<'_> {
    fn focus(&mut self) -> Result<()> {
        self.refresh_remotes();
        self.refresh_remote();
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        let Ok(res) = self.popup_rx.try_recv() else {
            return Ok(None);
        };
        let pending = self.pending.take();
        if !res.1.unwrap_or(false) {
            return Ok(None);
        }
        Ok(match (res.0, pending) {
            (REMOVE_POPUP_ID, Some(Pending::Remove(name))) => self.execute_remove(&name),
            (PUSH_POPUP_ID, Some(Pending::Push(name, identity, bookmarks))) => {
                let what = if bookmarks.is_empty() {
                    String::new()
                } else {
                    format!(" {}", summarize_names(&bookmarks))
                };
                let label = identity.label(&format!("Pushing{what} to {name}"));
                let loader = LoaderPopup::new(format!("Pushing to {name}"), move || {
                    new_commander().git_push_remote(&name, false, &identity)
                })
                .with_label(label);
                Some(AppAction::SetPopup(Some(Box::new(loader))))
            }
            _ => None,
        })
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        {
            let index = remote_index(self.selected.as_deref(), self.remotes());
            let block = Block::bordered()
                .title(" Remotes ")
                .border_type(BorderType::Rounded);
            self.remotes_height = block.inner(chunks[0]).height;

            let list = match self.remotes_output.as_ref() {
                Ok(remotes) if remotes.is_empty() => List::new(vec![ListItem::new(
                    Line::from(" No remotes (a: add one)")
                        .fg(Color::DarkGray)
                        .italic(),
                )]),
                Ok(remotes) => List::new(remotes.iter().map(remote_item)),
                Err(err) => List::new(err.into_text("Error getting remotes")?.lines),
            }
            .block(block)
            .highlight_style(Style::default().bg(self.config.highlight_color()))
            .scroll_padding(2);
            *self.remotes_list_state.selected_mut() = index;
            f.render_stateful_widget(&list, chunks[0], &mut self.remotes_list_state);
        }

        {
            let title = match self.selected_remote() {
                Some(remote) => format!(" Remote {} ", remote.name),
                None => " Remote ".to_owned(),
            };
            let mut lines: Vec<Line> = vec![];
            if let Some(remote) = self.selected_remote() {
                let stats = &remote.stats;
                lines.push(Line::from(vec![
                    Span::raw("URL: ").fg(Color::DarkGray),
                    Span::raw(remote.url.clone()),
                ]));
                if stats.tracked > 0 {
                    lines.push(Line::from(vec![
                        Span::raw("To push: ").fg(Color::DarkGray),
                        Span::raw(format!("↑{}", stats.ahead)).fg(Color::Green),
                        Span::raw("   local bookmarks to update: ").fg(Color::DarkGray),
                        Span::raw(format!("↓{}", stats.behind)).fg(Color::Red),
                    ]));
                }
                lines.push(
                    Line::from("As of the last fetch; f fetches this remote.").fg(Color::DarkGray),
                );
                lines.push(Line::from(""));
            }
            match self.remote_output.as_ref() {
                Some(Ok(bookmarks)) if bookmarks.is_empty() => {
                    lines.push(Line::from("No bookmarks on this remote.").fg(Color::DarkGray));
                }
                Some(Ok(bookmarks)) => lines.extend(error_text(bookmarks).lines),
                Some(Err(err)) => lines.extend(err.into_text("Error getting bookmarks")?.lines),
                None => {}
            }
            self.remote_panel
                .render_context::<TextContent>(Text::from(lines))
                .title(title)
                .draw(f, chunks[1]);
        }

        if let Some(prompt) = self.prompt.as_ref() {
            let block = crate::ui::styles::create_popup_block(prompt.kind.title());
            let error_lines = prompt
                .error
                .as_deref()
                .map(|error| error.lines().count().clamp(1, 4) as u16 + 1)
                .unwrap_or(0);
            let prompt_area =
                crate::ui::utils::centered_rect_line_height(area, 60, 6 + error_lines);
            f.render_widget(Clear, prompt_area);
            f.render_widget(&block, prompt_area);

            let mut constraints = vec![Constraint::Length(1), Constraint::Fill(1)];
            if error_lines > 0 {
                constraints.push(Constraint::Length(error_lines));
            }
            constraints.push(Constraint::Length(2));
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(block.inner(prompt_area));

            let label = if prompt.remote.is_empty() {
                prompt.kind.label().to_owned()
            } else {
                format!("{} {}", prompt.kind.label(), prompt.remote)
            };
            f.render_widget(Paragraph::new(label).fg(Color::DarkGray), chunks[0]);
            f.render_widget(&prompt.textarea, chunks[1]);
            if let Some(error) = prompt.error.as_ref() {
                f.render_widget(
                    Paragraph::new(error_text(error)).block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    ),
                    chunks[2],
                );
            }
            f.render_widget(
                Paragraph::new("Ctrl+s/Enter: apply | Escape: cancel")
                    .fg(Color::DarkGray)
                    .alignment(Alignment::Center)
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    ),
                chunks[chunks.len() - 1],
            );
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
        let key = match event {
            Event::Key(key) => key,
            Event::Mouse(mouse) => {
                if self.pane_divider.handle_mouse(mouse, self.config.layout())
                    || self.remote_panel.input_mouse(mouse)
                {
                    return Ok(ComponentInputResult::Handled);
                }
                return Ok(ComponentInputResult::NotHandled);
            }
            _ => return Ok(ComponentInputResult::NotHandled),
        };
        if key.kind != KeyEventKind::Press {
            return Ok(ComponentInputResult::Handled);
        }

        if self.prompt.is_some() {
            let submit = key.code == KeyCode::Enter
                || (key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL));
            if key.code == KeyCode::Esc {
                self.prompt = None;
            } else if submit {
                if let Some(message) = self.submit_prompt() {
                    return Ok(ComponentInputResult::HandledAction(
                        AppAction::SetStatusMessage(message),
                    ));
                }
            } else if let Some(prompt) = self.prompt.as_mut() {
                prompt.textarea.input(key);
            }
            return Ok(ComponentInputResult::Handled);
        }

        if self.popup.is_opened() {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.popup = ConfirmDialogState::default();
                self.pending = None;
            } else {
                self.popup.handle(&key);
            }
            return Ok(ComponentInputResult::Handled);
        }

        if self.remote_panel.input(key) {
            return Ok(ComponentInputResult::Handled);
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll_remotes(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_remotes(-1),
            KeyCode::Char('J') => self.scroll_remotes(self.remotes_height as isize / 2),
            KeyCode::Char('K') => {
                self.scroll_remotes((self.remotes_height as isize / 2).saturating_neg())
            }
            KeyCode::Char('R') | KeyCode::F(5) => {
                self.refresh_remotes();
                self.refresh_remote();
            }
            KeyCode::Char('f') => return Ok(self.fetch(false)),
            KeyCode::Char('F') => return Ok(self.fetch(true)),
            KeyCode::Char('p') => return Ok(self.confirm_push()),
            KeyCode::Char('a') => return Ok(self.start_prompt(PromptKind::Add)),
            KeyCode::Char('r') => return Ok(self.start_prompt(PromptKind::Rename)),
            KeyCode::Char('e') => return Ok(self.start_prompt(PromptKind::SetUrl)),
            KeyCode::Char('d') => return Ok(self.confirm_remove()),
            KeyCode::Char('?') => {
                return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                    Some(Box::new(HelpPopup::new(
                        vec![
                            ("j/k".to_owned(), "scroll down/up".to_owned()),
                            ("J/K".to_owned(), "scroll down/up by ½ page".to_owned()),
                            ("f".to_owned(), "fetch the selected remote".to_owned()),
                            ("F".to_owned(), "fetch every remote".to_owned()),
                            (
                                "p".to_owned(),
                                "push the remote's tracked bookmarks (previews first)".to_owned(),
                            ),
                            ("a".to_owned(), "add a remote".to_owned()),
                            ("r".to_owned(), "rename the selected remote".to_owned()),
                            ("e".to_owned(), "edit the selected remote's URL".to_owned()),
                            (
                                "d".to_owned(),
                                "remove the selected remote and forget its bookmarks".to_owned(),
                            ),
                            ("R".to_owned(), "refresh the view".to_owned()),
                        ],
                        vec![
                            ("Ctrl+e/Ctrl+y".to_owned(), "scroll down/up".to_owned()),
                            (
                                "Ctrl+d/Ctrl+u".to_owned(),
                                "scroll down/up by ½ page".to_owned(),
                            ),
                        ],
                    ))),
                )));
            }
            _ => return Ok(ComponentInputResult::NotHandled),
        }

        Ok(ComponentInputResult::Handled)
    }
}
