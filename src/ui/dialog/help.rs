use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::KeyEventKind;
use ratatui::crossterm::event::{self};
use ratatui::layout::Constraint;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Cell;
use ratatui::widgets::Clear;
use ratatui::widgets::Row;
use ratatui::widgets::Table;
use ratatui_textarea::TextArea;

use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::search::SearchState;
use crate::ui::search::first_match_index_at_or_after;
use crate::ui::search::highlight_matches;
use crate::ui::search::match_indices;
use crate::ui::search::next_match_index;
use crate::ui::styles::create_popup_block;
use crate::ui::utils::centered_rect;

pub struct HelpPopup<'a> {
    main_items: Vec<(String, String)>,
    details_items: Vec<(String, String)>,
    /// Number of table rows at the last draw, for clamping the scroll.
    row_count: usize,
    // Can't use TableState as it's broken: https://github.com/ratatui-org/ratatui/issues/1179
    scroll: usize,
    /// Active vim-style search, shared with the tabs via [crate::ui::search].
    /// While set, matching rows are highlighted.
    search: SearchState,
    /// The `/` search input bar shown at the bottom of the popup while typing
    /// a query. `None` when not searching.
    search_textarea: Option<TextArea<'a>>,
}

/// Greedy word-wrap by character count. Words longer than `width` overflow
/// their line and get truncated by the table rather than split.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
}

impl<'a> HelpPopup<'a> {
    pub fn new(main_items: Vec<(String, String)>, details_items: Vec<(String, String)>) -> Self {
        Self {
            main_items,
            details_items,
            row_count: 0,
            scroll: 0,
            search: SearchState::new(),
            search_textarea: None,
        }
    }

    /// The searchable text of every table row, in draw order: the section
    /// titles, the keybinding rows (key plus description) and the blank
    /// spacer, so a `/` search matches what the user sees and the match
    /// indices line up with [Self::scroll].
    fn search_texts(&self) -> Vec<String> {
        let mut texts = vec!["Main panel".to_owned()];
        texts.extend(
            self.main_items
                .iter()
                .map(|(key, description)| format!("{key} {description}")),
        );
        texts.push(String::new());
        texts.push("Details panel".to_owned());
        texts.extend(
            self.details_items
                .iter()
                .map(|(key, description)| format!("{key} {description}")),
        );
        texts
    }

    /// Open the `/` search bar. Highlighting updates live as the user types;
    /// the scroll only jumps on Enter (mirrors the log tab).
    fn open_search(&mut self) {
        self.search.set_query("");
        self.search_textarea = Some(TextArea::default());
    }

    /// Scroll to the first search match at or after the current scroll
    /// position (wrapping). Returns the match count. Used on Enter.
    fn scroll_to_first_match(&mut self) -> usize {
        self.scroll_to_match(first_match_index_at_or_after)
    }

    /// Scroll to the next/previous match, wrapping. Returns the match count.
    /// Used by n/N.
    fn scroll_to_adjacent_match(&mut self, forward: bool) -> usize {
        self.scroll_to_match(|matches, current| next_match_index(matches, current, forward))
    }

    /// Shared match-navigation: compute matches over the table rows, ask
    /// `pick` for the target row, and scroll it to the top of the popup.
    fn scroll_to_match(&mut self, pick: impl Fn(&[usize], usize) -> Option<usize>) -> usize {
        let Some(query) = self.search.query() else {
            return 0;
        };
        let texts = self.search_texts();
        let matches = match_indices(&texts, query, |text| text.clone());
        if matches.is_empty() {
            return 0;
        }
        if let Some(idx) = pick(&matches, self.scroll) {
            self.scroll = idx.min(texts.len().saturating_sub(1));
        }
        matches.len()
    }

    /// Build the section rows: a bold section title, then one row per
    /// keybinding with the description wrapped to `desc_width`. Search matches
    /// are highlighted in both columns.
    fn section_rows<'r>(
        title: &'r str,
        items: &'r [(String, String)],
        desc_width: usize,
        query: Option<&str>,
    ) -> Vec<Row<'r>> {
        let highlighted = |line: Line<'r>| -> Line<'r> {
            let mut line = line;
            if let Some(query) = query {
                highlight_matches(&mut line, query);
            }
            line
        };

        let mut rows = vec![Row::new([
            Cell::from(highlighted(Line::from(Span::from(title).bold()))),
            Cell::from(""),
        ])];
        for (key, description) in items {
            let lines = wrap_text(description, desc_width);
            let height = lines.len() as u16;
            rows.push(
                Row::new([
                    Cell::from(highlighted(Line::from(key.as_str()))),
                    Cell::from(Text::from(
                        lines
                            .into_iter()
                            .map(|line| highlighted(Line::from(line)))
                            .collect::<Vec<_>>(),
                    )),
                ])
                .height(height),
            );
        }
        rows
    }
}

impl Component for HelpPopup<'_> {
    fn draw(
        &mut self,
        f: &mut ratatui::prelude::Frame<'_>,
        area: ratatui::prelude::Rect,
    ) -> anyhow::Result<()> {
        let area = centered_rect(area, 80, 80);
        f.render_widget(Clear, area);

        let block = create_popup_block("Help (j/k: scroll, /: search)");
        let block_inner = block.inner(area);
        f.render_widget(&block, area);

        // One full-width table with the sections stacked, so long
        // descriptions get the whole popup width and wrap instead of being
        // cut off at a column boundary.
        let key_width = self
            .main_items
            .iter()
            .chain(self.details_items.iter())
            .map(|(key, _)| key.chars().count())
            .max()
            .unwrap_or(0)
            .max("Details panel".chars().count());
        let desc_width = (block_inner.width as usize)
            .saturating_sub(key_width + 2)
            .max(20);

        let query = self.search.query();
        let mut rows = Self::section_rows("Main panel", &self.main_items, desc_width, query);
        rows.push(Row::new([Cell::from(""), Cell::from("")]));
        rows.extend(Self::section_rows(
            "Details panel",
            &self.details_items,
            desc_width,
            query,
        ));
        self.row_count = rows.len();

        let rows: Vec<Row> = rows.into_iter().skip(self.scroll).collect();
        let widths = [
            Constraint::Length(key_width as u16 + 2),
            Constraint::Fill(1),
        ];
        f.render_widget(Table::new(rows, widths), block_inner);

        // Draw the vim-style search bar over the bottom row of the popup,
        // identical to the log and bookmarks tabs.
        if let Some(search_textarea) = self.search_textarea.as_mut() {
            let bar = Rect {
                x: block_inner.x,
                y: block_inner.y + block_inner.height.saturating_sub(1),
                width: block_inner.width,
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

        Ok(())
    }

    fn input(&mut self, event: Event) -> anyhow::Result<ComponentInputResult> {
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
                            self.scroll_to_first_match();
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

        if let Event::Key(key) = event
            && key.kind == event::KeyEventKind::Press
        {
            // While a search is active, n/N navigate matches and Esc clears
            // the search instead of closing the popup.
            if self.search.is_active() {
                match key.code {
                    KeyCode::Char('n') => {
                        self.scroll_to_adjacent_match(true);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Char('N') => {
                        self.scroll_to_adjacent_match(false);
                        return Ok(ComponentInputResult::Handled);
                    }
                    KeyCode::Esc => {
                        self.search.clear();
                        return Ok(ComponentInputResult::Handled);
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    // Rows can be taller than one line, so this conservatively
                    // allows scrolling until only the last row is visible.
                    self.scroll = (self.scroll + 1).min(self.row_count.saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Char('/') => self.open_search(),
                _ => return Ok(ComponentInputResult::NotHandled),
            }

            return Ok(ComponentInputResult::Handled);
        }

        Ok(ComponentInputResult::NotHandled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn popup() -> HelpPopup<'static> {
        HelpPopup::new(
            vec![
                ("j/k".to_owned(), "scroll down/up".to_owned()),
                ("w".to_owned(), "toggle diff format".to_owned()),
            ],
            vec![("J/K".to_owned(), "scroll the diff".to_owned())],
        )
    }

    #[test]
    fn search_texts_cover_titles_rows_and_spacer() {
        assert_eq!(
            popup().search_texts(),
            vec![
                "Main panel".to_owned(),
                "j/k scroll down/up".to_owned(),
                "w toggle diff format".to_owned(),
                String::new(),
                "Details panel".to_owned(),
                "J/K scroll the diff".to_owned(),
            ]
        );
    }

    #[test]
    fn enter_scrolls_to_the_first_match() {
        let mut popup = popup();
        popup.search.set_query("diff format");
        assert_eq!(popup.scroll_to_first_match(), 1);
        assert_eq!(popup.scroll, 2);
    }

    #[test]
    fn n_and_shift_n_step_through_matches_wrapping() {
        let mut popup = popup();
        popup.search.set_query("scroll");
        // Matches: the two "scroll" descriptions in the main section and the
        // one in the details section.
        assert_eq!(popup.scroll_to_first_match(), 2);
        assert_eq!(popup.scroll, 1);
        popup.scroll_to_adjacent_match(true);
        assert_eq!(popup.scroll, 5);
        popup.scroll_to_adjacent_match(true);
        assert_eq!(popup.scroll, 1); // wrapped
        popup.scroll_to_adjacent_match(false);
        assert_eq!(popup.scroll, 5); // wrapped
    }

    #[test]
    fn search_with_no_matches_leaves_the_scroll_alone() {
        let mut popup = popup();
        popup.scroll = 3;
        popup.search.set_query("zzz");
        assert_eq!(popup.scroll_to_first_match(), 0);
        assert_eq!(popup.scroll, 3);
    }

    #[test]
    fn slash_opens_the_search_bar_and_esc_closes_it() {
        let mut popup = popup();
        popup
            .input(Event::Key(KeyCode::Char('/').into()))
            .expect("input");
        assert!(popup.search_textarea.is_some());
        popup.input(Event::Key(KeyCode::Esc.into())).expect("input");
        assert!(popup.search_textarea.is_none());
        assert!(!popup.search.is_active());
    }

    #[test]
    fn typing_updates_the_live_highlight_query() {
        let mut popup = popup();
        popup
            .input(Event::Key(KeyCode::Char('/').into()))
            .expect("input");
        for c in "Diff".chars() {
            popup
                .input(Event::Key(KeyCode::Char(c).into()))
                .expect("input");
        }
        assert_eq!(popup.search.query(), Some("diff"));
        popup
            .input(Event::Key(KeyCode::Enter.into()))
            .expect("input");
        assert!(popup.search_textarea.is_none());
        assert!(popup.search.is_active());
        assert_eq!(popup.scroll, 2);
    }
}
