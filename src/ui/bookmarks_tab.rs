#![expect(clippy::borrow_interior_mutable_const)]

use ansi_to_tui::IntoText;
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
use crate::commander::bookmarks::BookmarkLine;
use crate::commander::ids::ChangeId;
use crate::commander::new_commander;
use crate::env::DiffFormat;
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
use crate::ui::utils::centered_rect;
use crate::ui::utils::centered_rect_line_height;
use crate::ui::utils::tabs_to_spaces;

struct CreateBookmark<'a> {
    textarea: TextArea<'a>,
    error: Option<anyhow::Error>,
}

struct RenameBookmark<'a> {
    textarea: TextArea<'a>,
    name: String,
    error: Option<anyhow::Error>,
}

struct DeleteBookmark {
    name: String,
}

struct ForgetBookmark {
    name: String,
}

const DELETE_BRANCH_POPUP_ID: u16 = 1;
const FORGET_BRANCH_POPUP_ID: u16 = 2;
const NEW_POPUP_ID: u16 = 3;

/// Bookmarks tab. Shows bookmarks in main panel and selected bookmark current change in details panel.
pub struct BookmarksTab<'a> {
    bookmarks_output: Result<Vec<BookmarkLine>, CommandError>,
    bookmarks_list_state: ListState,
    bookmarks_height: u16,

    show_all: bool,
    filter_query: String,
    filter: Option<TextArea<'a>>,

    bookmark: Option<BookmarkLine>,

    bookmark_panel: DetailsPanel,
    bookmark_output: Option<Result<String, CommandError>>,

    create: Option<CreateBookmark<'a>>,
    rename: Option<RenameBookmark<'a>>,
    delete: Option<DeleteBookmark>,
    forget: Option<ForgetBookmark>,

    describe_textarea: Option<TextArea<'a>>,
    describe_after_new: bool,
    describe_after_new_change: Option<ChangeId>,

    popup: ConfirmDialogState,
    popup_tx: std::sync::mpsc::Sender<Listener>,
    popup_rx: std::sync::mpsc::Receiver<Listener>,

    diff_format: DiffFormat,

    config: JjConfig,
    pane_divider: PaneDivider,
}

fn bookmark_lines_match(current_bookmark: &BookmarkLine, bookmark: &BookmarkLine) -> bool {
    match (current_bookmark, bookmark) {
        (
            BookmarkLine::Parsed {
                bookmark: current_bookmark,
                ..
            },
            BookmarkLine::Parsed { bookmark, .. },
        ) => current_bookmark.name == bookmark.name && current_bookmark.remote == bookmark.remote,
        (BookmarkLine::Unparsable(current_bookmark), BookmarkLine::Unparsable(bookmark)) => {
            current_bookmark == bookmark
        }
        _ => false,
    }
}

fn bookmark_matches_filter(bookmark: &BookmarkLine, filter_query: &str) -> bool {
    if filter_query.is_empty() {
        return true;
    }

    let filter_query = filter_query.to_lowercase();
    match bookmark {
        BookmarkLine::Parsed { bookmark, .. } => {
            bookmark.to_string().to_lowercase().contains(&filter_query)
        }
        BookmarkLine::Unparsable(text) => text.to_lowercase().contains(&filter_query),
    }
}

fn get_current_bookmark_index_in_list(
    current_bookmark: Option<&BookmarkLine>,
    bookmarks: &[&BookmarkLine],
) -> Option<usize> {
    current_bookmark.and_then(|current_bookmark| {
        bookmarks
            .iter()
            .position(|bookmark| bookmark_lines_match(current_bookmark, bookmark))
    })
}

fn get_current_bookmark_index(
    current_bookmark: Option<&BookmarkLine>,
    bookmarks_output: &Result<Vec<BookmarkLine>, CommandError>,
) -> Option<usize> {
    match bookmarks_output {
        Ok(bookmarks_output) => {
            let bookmarks: Vec<&BookmarkLine> = bookmarks_output.iter().collect();
            get_current_bookmark_index_in_list(current_bookmark, &bookmarks)
        }
        Err(_) => None,
    }
}

impl BookmarksTab<'_> {
    #[instrument(level = "info", name = "Initializing bookmarks tab", parent = None, skip())]
    pub fn new() -> Result<Self> {
        let diff_format = get_env().jj_config.diff_format();

        let show_all = false;

        let bookmarks_output = new_commander().get_bookmarks(show_all);
        let bookmark = bookmarks_output
            .as_ref()
            .ok()
            .and_then(|bookmarks_output| bookmarks_output.first())
            .map(|bookmarks_output| bookmarks_output.to_owned());

        let bookmarks_list_state = ListState::default().with_selected(get_current_bookmark_index(
            bookmark.as_ref(),
            &bookmarks_output,
        ));

        let bookmark_output = bookmark.as_ref().and_then(|bookmark| match bookmark {
            BookmarkLine::Parsed { bookmark, .. } => Some(
                new_commander()
                    .get_bookmark_show(bookmark, &diff_format, true)
                    .map(|diff| tabs_to_spaces(&diff)),
            ),
            _ => None,
        });

        let (popup_tx, popup_rx) = std::sync::mpsc::channel();

        let config = get_env().jj_config.clone();
        let pane_divider = PaneDivider::new(config.layout_percent());

        Ok(Self {
            bookmarks_output,
            bookmark,
            bookmarks_list_state,
            bookmarks_height: 0,

            show_all,
            filter_query: String::new(),
            filter: None,

            bookmark_panel: DetailsPanel::new(),
            bookmark_output,

            create: None,
            rename: None,
            delete: None,
            forget: None,

            describe_after_new: false,
            describe_textarea: None,
            describe_after_new_change: None,

            popup: ConfirmDialogState::default(),
            popup_tx,
            popup_rx,

            diff_format,

            config,
            pane_divider,
        })
    }

    pub fn refresh_bookmarks(&mut self) {
        self.bookmarks_output = new_commander().get_bookmarks(self.show_all);
    }

    fn filtered_bookmarks(&self) -> Vec<BookmarkLine> {
        match self.bookmarks_output.as_ref() {
            Ok(bookmarks_output) => bookmarks_output
                .iter()
                .filter(|bookmark| bookmark_matches_filter(bookmark, &self.filter_query))
                .cloned()
                .collect(),
            Err(_) => vec![],
        }
    }

    pub fn refresh_bookmark(&mut self) {
        let mut commander = new_commander();
        let inner_width = self.bookmark_panel.columns() as usize;
        commander.limit_width(inner_width);
        self.bookmark_output = self.bookmark.as_ref().and_then(|bookmark| match bookmark {
            BookmarkLine::Parsed { bookmark, .. } => Some(
                commander
                    .get_bookmark_show(bookmark, &self.diff_format, true)
                    .map(|diff| tabs_to_spaces(&diff)),
            ),
            _ => None,
        });

        self.bookmark_panel.scroll_to(0);
    }

    fn sync_selected_bookmark(&mut self) {
        let filtered_bookmarks = self.filtered_bookmarks();
        let filtered_bookmark_refs: Vec<&BookmarkLine> = filtered_bookmarks.iter().collect();
        self.bookmark = match filtered_bookmarks.first() {
            None => None,
            Some(_)
                if get_current_bookmark_index_in_list(
                    self.bookmark.as_ref(),
                    &filtered_bookmark_refs,
                )
                .is_some() =>
            {
                self.bookmark.clone()
            }
            Some(first_bookmark) => Some(first_bookmark.clone()),
        };

        self.refresh_bookmark();
    }

    fn open_filter(&mut self) {
        let mut textarea = TextArea::new(vec![self.filter_query.clone()]);
        textarea.move_cursor(CursorMove::End);
        self.filter = Some(textarea);
    }

    fn scroll_bookmarks(&mut self, scroll: isize) {
        let bookmarks = self.filtered_bookmarks();
        if bookmarks.is_empty() {
            return;
        }

        let bookmark_refs: Vec<&BookmarkLine> = bookmarks.iter().collect();
        let current_bookmark_index =
            get_current_bookmark_index_in_list(self.bookmark.as_ref(), &bookmark_refs);
        let next_bookmark = match current_bookmark_index {
            Some(current_bookmark_index) => bookmarks.get(
                current_bookmark_index
                    .saturating_add_signed(scroll)
                    .min(bookmarks.len() - 1),
            ),
            None => bookmarks.first(),
        }
        .cloned();

        if let Some(next_bookmark) = next_bookmark {
            self.bookmark = Some(next_bookmark);
            self.refresh_bookmark();
        }
    }
}

impl Component for BookmarksTab<'_> {
    fn focus(&mut self) -> Result<()> {
        self.refresh_bookmarks();
        self.sync_selected_bookmark();
        Ok(())
    }

    fn update(&mut self) -> Result<Option<AppAction>> {
        // Check for popup action
        if let Ok(res) = self.popup_rx.try_recv()
            && res.1.unwrap_or(false)
        {
            match res.0 {
                DELETE_BRANCH_POPUP_ID => {
                    if let Some(delete) = self.delete.as_ref() {
                        match new_commander().delete_bookmark(&delete.name) {
                            Ok(_) => {
                                self.refresh_bookmarks();
                                self.sync_selected_bookmark();
                            }
                            Err(err) => {
                                return Ok(Some(AppAction::SetPopup(Some(Box::new(
                                    MessagePopup::new("Delete error", err.to_string()),
                                )))));
                            }
                        }
                    }
                }
                FORGET_BRANCH_POPUP_ID => {
                    if let Some(forget) = self.forget.as_ref() {
                        match new_commander().forget_bookmark(&forget.name) {
                            Ok(_) => {
                                self.refresh_bookmarks();
                                self.sync_selected_bookmark();
                            }
                            Err(err) => {
                                return Ok(Some(AppAction::SetPopup(Some(Box::new(
                                    MessagePopup::new("Forget error", err.to_string()),
                                )))));
                            }
                        }
                    }
                }
                NEW_POPUP_ID => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref() {
                        // `--no-edit` keeps @ where it is; the log tab opens
                        // with the cursor on the new change instead
                        let head =
                            new_commander().run_new_no_edit([bookmark.to_string().as_str()])?;
                        if self.describe_after_new {
                            self.describe_after_new_change = Some(head.change_id);
                            self.describe_after_new = false;
                            let textarea = TextArea::default();
                            self.describe_textarea = Some(textarea);
                            return Ok(None);
                        } else {
                            return Ok(Some(AppAction::ViewLog(head)));
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(None)
    }

    fn draw(
        &mut self,
        f: &mut ratatui::prelude::Frame<'_>,
        area: ratatui::prelude::Rect,
    ) -> Result<()> {
        let chunks = self.pane_divider.split(area, self.config.layout());

        // Draw bookmarks
        {
            let filtered_bookmarks = self.filtered_bookmarks();
            let filtered_bookmark_refs: Vec<&BookmarkLine> = filtered_bookmarks.iter().collect();
            let current_bookmark_index =
                get_current_bookmark_index_in_list(self.bookmark.as_ref(), &filtered_bookmark_refs);
            let show_filter = self.filter.is_some() || !self.filter_query.is_empty();
            let bookmark_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(if show_filter { 3 } else { 0 }),
                    Constraint::Min(0),
                ])
                .split(chunks[0]);

            if show_filter {
                let filter_block = Block::bordered()
                    .title(Span::styled(
                        " Filter bookmarks ",
                        Style::new().bold().cyan(),
                    ))
                    .border_type(BorderType::Rounded)
                    .border_style(if self.filter.is_some() {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    });
                let filter_inner = filter_block.inner(bookmark_chunks[0]);
                f.render_widget(filter_block, bookmark_chunks[0]);
                if let Some(filter) = self.filter.as_ref() {
                    f.render_widget(filter, filter_inner);
                } else {
                    f.render_widget(Paragraph::new(self.filter_query.as_str()), filter_inner);
                }
            }

            let bookmark_lines: Vec<Line> = match self.bookmarks_output.as_ref() {
                Ok(_) => filtered_bookmarks
                    .iter()
                    .enumerate()
                    .map(|(i, bookmark)| -> Result<Vec<Line>, ansi_to_tui::Error> {
                        let bookmark_text = bookmark.to_text()?;
                        Ok(bookmark_text
                            .iter()
                            .map(|line| {
                                let mut line = line.to_owned();

                                // Add padding at start
                                line.spans.insert(0, Span::from(" "));

                                if current_bookmark_index == Some(i) {
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
                            .collect::<Vec<Line>>())
                    })
                    .collect::<Result<Vec<Vec<Line>>, ansi_to_tui::Error>>()?
                    .into_iter()
                    .flatten()
                    .collect(),
                Err(err) => [
                    vec![Line::raw("Error getting bookmarks").bold().fg(Color::Red)],
                    // TODO: Remove when jj 0.20 is released
                    if let CommandError::Status(output, _) = err {
                        if output.contains("unexpected argument '-T' found") {
                            vec![
                                Line::raw(""),
                                Line::raw("Please update jj to >0.18 for -T support to bookmarks")
                                    .bold()
                                    .fg(Color::Red),
                            ]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    },
                    vec![Line::raw(""), Line::raw("")],
                    err.to_string().into_text()?.lines,
                ]
                .concat(),
            };

            let lines = if bookmark_lines.is_empty() {
                vec![
                    Line::from(if self.filter_query.is_empty() {
                        " No bookmarks"
                    } else {
                        " No bookmarks matching filter"
                    })
                    .fg(Color::DarkGray)
                    .italic(),
                ]
            } else {
                bookmark_lines
            };

            let bookmarks_title = if self.filter_query.is_empty() {
                " Bookmarks ".to_owned()
            } else {
                format!(" Bookmarks [{}] ", self.filter_query)
            };
            let bookmarks_block = Block::bordered()
                .title(bookmarks_title)
                .border_type(BorderType::Rounded);
            self.bookmarks_height = bookmarks_block.inner(bookmark_chunks[1]).height;
            let bookmark_count = filtered_bookmarks.len();
            let bookmarks = List::new(lines).block(bookmarks_block).scroll_padding(3);
            *self.bookmarks_list_state.selected_mut() = current_bookmark_index;
            f.render_stateful_widget(
                bookmarks,
                bookmark_chunks[1],
                &mut self.bookmarks_list_state,
            );

            // Draw scrollbar on left panel
            if bookmark_count > self.bookmarks_height.into() {
                let index = current_bookmark_index.unwrap_or(0);
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
                let mut scrollbar_state = ScrollbarState::default()
                    .content_length(bookmark_count)
                    .position(index);

                f.render_stateful_widget(
                    scrollbar,
                    bookmark_chunks[1].inner(Margin {
                        vertical: 1,
                        horizontal: 0,
                    }),
                    &mut scrollbar_state,
                );
            }
        }

        // Draw bookmark
        {
            let title = if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
            {
                format!(" Bookmark {bookmark} ")
            } else {
                " Bookmark ".to_owned()
            };
            let bookmark_content: Vec<Line> = match self.bookmark_output.as_ref() {
                Some(Ok(bookmark_output)) => bookmark_output.into_text()?.lines,
                Some(Err(err)) => err.into_text("Error getting bookmark")?.lines,
                None => vec![],
            };
            self.bookmark_panel
                .render_context::<TextContent>(bookmark_content)
                .title(title)
                .draw(f, chunks[1]);
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

        // Draw create textarea
        {
            if let Some(create) = self.create.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(
                        " Create bookmark ",
                        Style::new().bold().cyan(),
                    ))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                let error_lines = create
                    .error
                    .as_ref()
                    .map(|error| error.to_string().into_text().unwrap().lines);
                let error_height = if let Some(error_lines) = error_lines.as_ref() {
                    error_lines.len() + 1
                } else {
                    0
                };
                let area = centered_rect_line_height(area, 30, 5 + error_height as u16);
                f.render_widget(Clear, area);
                f.render_widget(&block, area);

                let popup_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Fill(1),
                        Constraint::Length(error_height as u16),
                        Constraint::Length(2),
                    ])
                    .split(block.inner(area));

                f.render_widget(&create.textarea, popup_chunks[0]);

                if let Some(error_lines) = error_lines {
                    let help = Paragraph::new(error_lines).block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    );

                    f.render_widget(help, popup_chunks[1]);
                }

                let help = Paragraph::new(vec!["Ctrl+s: save | Escape: cancel".into()])
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

        // Draw rename textarea
        {
            if let Some(rename) = self.rename.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(
                        " Rename bookmark ",
                        Style::new().bold().cyan(),
                    ))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                let error_lines = rename
                    .error
                    .as_ref()
                    .map(|error| error.to_string().into_text().unwrap().lines);
                let error_height = if let Some(error_lines) = error_lines.as_ref() {
                    error_lines.len() + 1
                } else {
                    0
                };
                let area = centered_rect_line_height(area, 30, 5 + error_height as u16);
                f.render_widget(Clear, area);
                f.render_widget(&block, area);

                let popup_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Fill(1),
                        Constraint::Length(error_height as u16),
                        Constraint::Length(2),
                    ])
                    .split(block.inner(area));

                f.render_widget(&rename.textarea, popup_chunks[0]);

                if let Some(error_lines) = error_lines {
                    let help = Paragraph::new(error_lines).block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    );

                    f.render_widget(help, popup_chunks[1]);
                }

                let help = Paragraph::new(vec!["Ctrl+s: save | Escape: cancel".into()])
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

        // Draw describe textarea
        {
            if let Some(describe_textarea) = self.describe_textarea.as_mut() {
                let block = Block::bordered()
                    .title(Span::styled(" Describe ", Style::new().bold().cyan()))
                    .title_alignment(Alignment::Center)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green));
                let area = centered_rect(area, 50, 50);
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

        Ok(())
    }

    fn input(&mut self, event: Event) -> Result<ComponentInputResult> {
        if let Some(create) = self.create.as_mut() {
            if let Event::Key(key) = event {
                match key.code {
                    _ if (key.code == KeyCode::Char('s')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                        || (key.code == KeyCode::Enter) =>
                    {
                        let name = create.textarea.lines().join("\n");

                        if name.trim().is_empty() {
                            create.error =
                                Some(anyhow::Error::msg("Bookmark name cannot be empty"));
                            return Ok(ComponentInputResult::Handled);
                        }

                        if let Err(err) = new_commander().create_bookmark(&name) {
                            create.error = Some(anyhow::Error::new(err));
                            return Ok(ComponentInputResult::Handled);
                        }

                        self.create = None;
                        self.refresh_bookmarks();

                        // Select new bookmark
                        if let Some(bookmark) =
                            self.bookmarks_output
                                .as_ref()
                                .ok()
                                .and_then(|bookmarks_output| {
                                    bookmarks_output.iter().find(|bookmark| match bookmark {
                                        BookmarkLine::Unparsable(_) => false,
                                        BookmarkLine::Parsed { bookmark, .. } => {
                                            bookmark.name == name
                                        }
                                    })
                                })
                        {
                            self.bookmark = Some(bookmark.clone());
                        }

                        self.sync_selected_bookmark();

                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.create = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            create.textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(rename) = self.rename.as_mut() {
            if let Event::Key(key) = event {
                match key.code {
                    _ if (key.code == KeyCode::Char('s')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                        || (key.code == KeyCode::Enter) =>
                    {
                        let new = rename.textarea.lines().join("\n");

                        if new.trim().is_empty() {
                            rename.error =
                                Some(anyhow::Error::msg("Bookmark name cannot be empty"));
                            return Ok(ComponentInputResult::Handled);
                        }

                        let old = rename.name.clone();

                        if let Err(err) = new_commander().rename_bookmark(&old, &new) {
                            rename.error = Some(anyhow::Error::new(err));
                            return Ok(ComponentInputResult::Handled);
                        }
                        self.rename = None;
                        self.refresh_bookmarks();

                        // Select new bookmark
                        if let Some(bookmark) =
                            self.bookmarks_output
                                .as_ref()
                                .ok()
                                .and_then(|bookmarks_output| {
                                    bookmarks_output.iter().find(|bookmark| match bookmark {
                                        BookmarkLine::Unparsable(_) => false,
                                        BookmarkLine::Parsed { bookmark, .. } => {
                                            bookmark.name == new
                                        }
                                    })
                                })
                        {
                            self.bookmark = Some(bookmark.clone());
                        }

                        self.sync_selected_bookmark();

                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.rename = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            rename.textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let (Some(describe_textarea), Some(describe_after_new_change)) = (
            self.describe_textarea.as_mut(),
            self.describe_after_new_change.as_ref(),
        ) {
            if let Event::Key(key) = event {
                match key.code {
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // TODO: Handle error
                        new_commander().run_describe(
                            describe_after_new_change.as_str(),
                            &describe_textarea.lines().join("\n"),
                        )?;
                        self.describe_textarea = None;
                        self.describe_after_new_change = None;
                        return Ok(ComponentInputResult::HandledAction(AppAction::ViewLog(
                            new_commander().get_current_head()?,
                        )));
                    }
                    KeyCode::Esc => {
                        self.describe_textarea = None;
                        self.describe_after_new_change = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            describe_textarea.input(event);
            return Ok(ComponentInputResult::Handled);
        }

        if let Some(filter) = self.filter.as_mut() {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return Ok(ComponentInputResult::Handled);
                }

                match key.code {
                    KeyCode::Up => {
                        self.scroll_bookmarks(-1);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Down => {
                        self.scroll_bookmarks(1);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Enter => {
                        self.filter = None;
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.filter_query.clear();
                        self.filter = None;
                        self.sync_selected_bookmark();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }

            filter.input(event);
            self.filter_query = filter.lines().join("");
            self.sync_selected_bookmark();
            return Ok(ComponentInputResult::Handled);
        }

        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Ok(ComponentInputResult::Handled);
            }
            if self.popup.is_opened() {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                    self.popup = ConfirmDialogState::default();
                } else {
                    self.popup.handle(&key);
                }

                return Ok(ComponentInputResult::Handled);
            }

            if self.bookmark_panel.input(key) {
                return Ok(ComponentInputResult::Handled);
            }

            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_bookmarks(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_bookmarks(-1),
                KeyCode::Char('J') => {
                    self.scroll_bookmarks(self.bookmarks_height as isize / 2);
                }
                KeyCode::Char('K') => {
                    self.scroll_bookmarks((self.bookmarks_height as isize / 2).saturating_neg());
                }
                KeyCode::Char('w') => {
                    self.diff_format = self.diff_format.get_next(self.config.diff_tool());
                    self.refresh_bookmark();
                }
                KeyCode::Char('R') | KeyCode::F(5) => {
                    self.refresh_bookmarks();
                    self.sync_selected_bookmark();
                }
                KeyCode::Char('a') => {
                    self.show_all = !self.show_all;
                    self.refresh_bookmarks();
                    self.sync_selected_bookmark();
                }
                KeyCode::Char('/') => {
                    self.open_filter();
                    return Ok(ComponentInputResult::Handled);
                }
                KeyCode::Char('c') => {
                    let textarea = TextArea::default();
                    self.create = Some(CreateBookmark {
                        textarea,
                        error: None,
                    });
                    return Ok(ComponentInputResult::Handled);
                }
                KeyCode::Char('r') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref() {
                        let mut textarea = TextArea::new(vec![bookmark.name.clone()]);
                        textarea.move_cursor(CursorMove::End);
                        self.rename = Some(RenameBookmark {
                            textarea,
                            name: bookmark.name.clone(),
                            error: None,
                        });
                        return Ok(ComponentInputResult::Handled);
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref() {
                        self.delete = Some(DeleteBookmark {
                            name: bookmark.name.clone(),
                        });
                        self.popup = ConfirmDialogState::new(
                            DELETE_BRANCH_POPUP_ID,
                            Span::styled(" Delete ", Style::new().bold().cyan()),
                            Text::from(vec![Line::from(format!(
                                "Are you sure you want to delete the {} bookmark?",
                                bookmark.name
                            ))]),
                        );
                        self.popup
                            .with_yes_button(ButtonLabel::YES.clone())
                            .with_no_button(ButtonLabel::NO.clone())
                            .with_listener(Some(self.popup_tx.clone()))
                            .open();
                    }
                }
                KeyCode::Char('f') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref() {
                        self.forget = Some(ForgetBookmark {
                            name: bookmark.name.clone(),
                        });
                        self.popup = ConfirmDialogState::new(
                            FORGET_BRANCH_POPUP_ID,
                            Span::styled(" Forget ", Style::new().bold().cyan()),
                            Text::from(vec![Line::from(format!(
                                "Are you sure you want to forget the {} bookmark?",
                                bookmark.name
                            ))]),
                        );
                        self.popup
                            .with_yes_button(ButtonLabel::YES.clone())
                            .with_no_button(ButtonLabel::NO.clone())
                            .with_listener(Some(self.popup_tx.clone()))
                            .open();
                    }
                }
                // TODO: Ask for confirmation?
                KeyCode::Char('t') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
                        && bookmark.remote.is_some()
                        && bookmark.present
                    {
                        new_commander().track_bookmark(bookmark)?;
                        self.refresh_bookmarks();
                        self.sync_selected_bookmark();
                    }
                }
                KeyCode::Char('T') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
                        && bookmark.remote.is_some()
                        && bookmark.present
                    {
                        new_commander().untrack_bookmark(bookmark)?;
                        self.refresh_bookmarks();
                        self.sync_selected_bookmark();
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
                        && bookmark.present
                    {
                        self.popup = ConfirmDialogState::new(
                            NEW_POPUP_ID,
                            Span::styled(" New ", Style::new().bold().cyan()),
                            Text::from(vec![
                                Line::from("Are you sure you want to create a new change?"),
                                Line::from(format!("Bookmark: {bookmark}")),
                            ]),
                        );
                        self.popup
                            .with_yes_button(ButtonLabel::YES.clone())
                            .with_no_button(ButtonLabel::NO.clone())
                            .with_listener(Some(self.popup_tx.clone()))
                            .open();

                        self.describe_after_new = key.code == KeyCode::Char('N');
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
                        && bookmark.present
                        && bookmark.remote.is_none()
                    {
                        let name = bookmark.name.clone();

                        let loader = LoaderPopup::new("Pushing".to_string(), move || {
                            new_commander().git_push_bookmark(&name)
                        });

                        return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                            Some(Box::new(loader)),
                        )));
                    }
                }
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref() {
                        let ignore_immutable = key.code == KeyCode::Char('E');
                        if bookmark.present {
                            if new_commander().check_revision_immutable(&bookmark.to_string())?
                                && !ignore_immutable
                            {
                                return Ok(ComponentInputResult::HandledAction(
                                    AppAction::SetPopup(Some(Box::new(MessagePopup::new(
                                        "Edit",
                                        "The change cannot be edited because it is immutable.",
                                    )))),
                                ));
                            }

                            // No confirmation: editing into a change is a
                            // frequent, cheap, and undoable action
                            new_commander().run_edit(&bookmark.to_string(), ignore_immutable)?;
                            let head = new_commander().get_current_head()?;
                            return Ok(ComponentInputResult::HandledAction(AppAction::ViewLog(
                                head,
                            )));
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(BookmarkLine::Parsed { bookmark, .. }) = self.bookmark.as_ref()
                        && bookmark.present
                    {
                        return Ok(ComponentInputResult::HandledAction(AppAction::ViewLog(
                            new_commander().get_bookmark_head(bookmark)?,
                        )));
                    }
                }
                KeyCode::Char('?') => {
                    return Ok(ComponentInputResult::HandledAction(AppAction::SetPopup(
                        Some(Box::new(HelpPopup::new(
                            vec![
                                ("j/k".to_owned(), "scroll down/up".to_owned()),
                                ("J/K".to_owned(), "scroll down by ½ page".to_owned()),
                                ("/".to_owned(), "filter bookmarks".to_owned()),
                                ("a".to_owned(), "show all remotes".to_owned()),
                                ("c".to_owned(), "create bookmark".to_owned()),
                                ("r".to_owned(), "rename bookmark".to_owned()),
                                ("d/f".to_owned(), "delete/forget bookmark".to_owned()),
                                ("t/T".to_owned(), "track/untrack bookmark".to_owned()),
                                ("Enter".to_owned(), "view in log".to_owned()),
                                ("n".to_owned(), "new from bookmark".to_owned()),
                                ("N".to_owned(), "new and describe".to_owned()),
                                ("e".to_owned(), "edit bookmark".to_owned()),
                                ("p".to_owned(), "push bookmark".to_owned()),
                                ("R/F5".to_owned(), "refresh the view".to_owned()),
                            ],
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
                _ => return Ok(ComponentInputResult::NotHandled),
            };
        }

        if let Event::Mouse(mouse) = event {
            if self.pane_divider.handle_mouse(mouse, self.config.layout()) {
                return Ok(ComponentInputResult::Handled);
            }
            if self.bookmark_panel.input_mouse(mouse) {
                return Ok(ComponentInputResult::Handled);
            }
            return Ok(ComponentInputResult::NotHandled);
        }

        Ok(ComponentInputResult::Handled)
    }
}

#[cfg(test)]
mod tests {
    use crate::commander::bookmarks::{Bookmark, BookmarkLine};

    use super::bookmark_matches_filter;

    #[test]
    fn bookmark_filter_matches_name_case_insensitively() {
        let bookmark = BookmarkLine::Parsed {
            text: "feature/login".into(),
            bookmark: Bookmark {
                name: "feature/login".into(),
                remote: None,
                present: true,
                timestamp: 0,
            },
        };

        assert!(bookmark_matches_filter(&bookmark, "LOGIN"));
    }

    #[test]
    fn bookmark_filter_matches_remote_name() {
        let bookmark = BookmarkLine::Parsed {
            text: "release@origin".into(),
            bookmark: Bookmark {
                name: "release".into(),
                remote: Some("origin".into()),
                present: true,
                timestamp: 0,
            },
        };

        assert!(bookmark_matches_filter(&bookmark, "origin"));
    }
}
