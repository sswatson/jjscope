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
use crate::ui::search::SearchState;
use crate::ui::search::first_match_index_at_or_after;
use crate::ui::search::highlight_matches;
use crate::ui::search::match_indices;
use crate::ui::search::next_match_index;
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

    /// Active vim-style search, shared with the log tab via
    /// [crate::ui::search]. While set, matching bookmark lines are highlighted
    /// and n/N navigate between them. Unlike the old filter, non-matching
    /// bookmarks stay visible.
    search: SearchState,
    /// The `/` search input bar shown at the bottom of the bookmarks list
    /// while typing a query. `None` when not searching.
    search_textarea: Option<TextArea<'a>>,

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

/// The searchable text of a bookmark line: exactly what is displayed, so a
/// `/` search matches what the user sees (mirrors the log tab).
fn bookmark_search_text(bookmark: &BookmarkLine) -> String {
    match bookmark {
        BookmarkLine::Parsed { bookmark, .. } => bookmark.to_string(),
        BookmarkLine::Unparsable(text) => text.clone(),
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
            search: SearchState::new(),
            search_textarea: None,

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

    /// All bookmarks currently shown in the list. Search no longer hides
    /// non-matching bookmarks (it highlights and navigates instead), so this
    /// is simply the fetched list.
    fn all_bookmarks(&self) -> Vec<BookmarkLine> {
        match self.bookmarks_output.as_ref() {
            Ok(bookmarks_output) => bookmarks_output.clone(),
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
        let bookmarks = self.all_bookmarks();
        let bookmark_refs: Vec<&BookmarkLine> = bookmarks.iter().collect();
        self.bookmark = match bookmarks.first() {
            None => None,
            Some(_)
                if get_current_bookmark_index_in_list(self.bookmark.as_ref(), &bookmark_refs)
                    .is_some() =>
            {
                self.bookmark.clone()
            }
            Some(first_bookmark) => Some(first_bookmark.clone()),
        };

        self.refresh_bookmark();
    }

    /// Open the `/` search bar. Highlighting updates live as the user types;
    /// the selection only jumps on Enter (mirrors the log tab).
    fn open_search(&mut self) {
        let textarea = TextArea::default();
        self.search.set_query("");
        self.search_textarea = Some(textarea);
    }

    /// Move the selection to the first search match at or after the current
    /// selection (wrapping). Returns the match count. Used on Enter.
    fn select_first_match(&mut self) -> usize {
        self.select_match(first_match_index_at_or_after)
    }

    /// Move the selection to the next/previous match, wrapping. Returns the
    /// match count. Used by n/N.
    fn select_adjacent_match(&mut self, forward: bool) -> usize {
        self.select_match(|matches, current| next_match_index(matches, current, forward))
    }

    /// Shared match-navigation: compute matches over the visible list, ask
    /// `pick` for the target index, and move the selection there.
    fn select_match(
        &mut self,
        pick: impl Fn(&[usize], usize) -> Option<usize>,
    ) -> usize {
        let Some(query) = self.search.query() else {
            return 0;
        };
        let bookmarks = self.all_bookmarks();
        let matches = match_indices(&bookmarks, query, bookmark_search_text);
        if matches.is_empty() {
            return 0;
        }
        let bookmark_refs: Vec<&BookmarkLine> = bookmarks.iter().collect();
        let current =
            get_current_bookmark_index_in_list(self.bookmark.as_ref(), &bookmark_refs).unwrap_or(0);
        if let Some(idx) = pick(&matches, current)
            && let Some(bookmark) = bookmarks.get(idx)
        {
            self.bookmark = Some(bookmark.clone());
            self.refresh_bookmark();
        }
        matches.len()
    }

    fn scroll_bookmarks(&mut self, scroll: isize) {
        let bookmarks = self.all_bookmarks();
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
                        new_commander().run_new([bookmark.to_string().as_str()])?;
                        let head = new_commander().get_current_head()?;
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
            let all_bookmarks = self.all_bookmarks();
            let bookmark_refs: Vec<&BookmarkLine> = all_bookmarks.iter().collect();
            let current_bookmark_index =
                get_current_bookmark_index_in_list(self.bookmark.as_ref(), &bookmark_refs);
            let search_query = self.search.query();
            let bookmark_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0)])
                .split(chunks[0]);

            let bookmark_lines: Vec<Line> = match self.bookmarks_output.as_ref() {
                Ok(_) => all_bookmarks
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

                                // Highlight the search query wherever it
                                // appears (applied after the selection bg so
                                // matches stay legible on the selected line).
                                if let Some(query) = search_query {
                                    highlight_matches(&mut line, query);
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
                vec![Line::from(" No bookmarks").fg(Color::DarkGray).italic()]
            } else {
                bookmark_lines
            };

            let bookmarks_block = Block::bordered()
                .title(" Bookmarks ")
                .border_type(BorderType::Rounded);
            self.bookmarks_height = bookmarks_block.inner(bookmark_chunks[0]).height;
            let bookmark_count = all_bookmarks.len();
            let bookmarks = List::new(lines).block(bookmarks_block).scroll_padding(3);
            *self.bookmarks_list_state.selected_mut() = current_bookmark_index;
            f.render_stateful_widget(
                bookmarks,
                bookmark_chunks[0],
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
                    bookmark_chunks[0].inner(Margin {
                        vertical: 1,
                        horizontal: 0,
                    }),
                    &mut scrollbar_state,
                );
            }

            // Draw the vim-style search bar over the bottom row of the list,
            // identical to the log tab.
            if let Some(search_textarea) = self.search_textarea.as_mut() {
                let list_area = bookmark_chunks[0];
                let bar = Rect {
                    x: list_area.x + 1,
                    y: list_area.y + list_area.height.saturating_sub(1),
                    width: list_area.width.saturating_sub(2),
                    height: 1,
                };
                f.render_widget(Clear, bar);
                let prompt_width = 1u16;
                let prompt = Rect {
                    width: prompt_width.min(bar.width),
                    ..bar
                };
                f.render_widget(
                    Span::styled("/", Style::new().fg(Color::Yellow).bold()),
                    prompt,
                );
                let input = Rect {
                    x: bar.x + prompt_width,
                    width: bar.width.saturating_sub(prompt_width),
                    ..bar
                };
                f.render_widget(&*search_textarea, input);
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

        if let Some(search_textarea) = self.search_textarea.as_mut() {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return Ok(ComponentInputResult::Handled);
                }

                // Enter confirms the search (vim-style); Esc cancels it.
                match key.code {
                    KeyCode::Enter => {
                        let query = search_textarea.lines().join("");
                        self.search_textarea = None;
                        self.search.set_query(&query);
                        if self.search.is_active() {
                            let count = self.select_first_match();
                            if count == 0 {
                                return Ok(ComponentInputResult::HandledAction(
                                    AppAction::SetStatusMessage(format!(
                                        "No matches for \"{query}\""
                                    )),
                                ));
                            }
                        }
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.search_textarea = None;
                        self.search.clear();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }
            // Any other key edits the query; update the live highlight.
            search_textarea.input(event);
            let query = search_textarea.lines().join("");
            self.search.set_query(&query);
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

            // While a search is active, n/N navigate matches instead of
            // creating changes, and Esc clears the search. Context-sensitive,
            // identical to the log tab.
            if self.search.is_active() {
                match key.code {
                    KeyCode::Char('n') => {
                        self.select_adjacent_match(true);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Char('N') => {
                        self.select_adjacent_match(false);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.search.clear();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
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
                    self.open_search();
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
                                (
                                    "/".to_owned(),
                                    "search bookmarks (n/N: next/previous match)".to_owned(),
                                ),
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
    use crate::ui::search::match_indices;

    use super::bookmark_search_text;

    fn parsed(name: &str, remote: Option<&str>) -> BookmarkLine {
        BookmarkLine::Parsed {
            text: String::new(),
            bookmark: Bookmark {
                name: name.into(),
                remote: remote.map(Into::into),
                present: true,
                timestamp: 0,
            },
        }
    }

    #[test]
    fn search_text_includes_name_and_remote() {
        assert_eq!(bookmark_search_text(&parsed("feature/login", None)), "feature/login");
        assert_eq!(
            bookmark_search_text(&parsed("release", Some("origin"))),
            "release@origin"
        );
        assert_eq!(
            bookmark_search_text(&BookmarkLine::Unparsable("weird line".into())),
            "weird line"
        );
    }

    #[test]
    fn search_matches_name_case_insensitively() {
        let bookmarks = vec![parsed("feature/login", None), parsed("main", None)];
        // Query is pre-lowercased by SearchState; pass lowercase here.
        let matches = match_indices(&bookmarks, "login", bookmark_search_text);
        assert_eq!(matches, vec![0]);
    }

    #[test]
    fn search_matches_remote_name() {
        let bookmarks = vec![
            parsed("release", Some("origin")),
            parsed("dev", Some("upstream")),
        ];
        let matches = match_indices(&bookmarks, "origin", bookmark_search_text);
        assert_eq!(matches, vec![0]);
    }

    #[test]
    fn search_matches_multiple_bookmarks() {
        let bookmarks = vec![
            parsed("apple-pie", None),
            parsed("banana", None),
            parsed("apple-cart", None),
        ];
        let matches = match_indices(&bookmarks, "apple", bookmark_search_text);
        assert_eq!(matches, vec![0, 2]);
    }
}
