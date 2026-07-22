use ratatui::crossterm::event::Event;
use ratatui::crossterm::event::KeyCode;
use ratatui::crossterm::event::{self};
use ratatui::layout::Constraint;
use ratatui::style::Stylize;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Cell;
use ratatui::widgets::Clear;
use ratatui::widgets::Row;
use ratatui::widgets::Table;

use crate::ui::Component;
use crate::ui::ComponentInputResult;
use crate::ui::styles::create_popup_block;
use crate::ui::utils::centered_rect;

pub struct HelpPopup {
    main_items: Vec<(String, String)>,
    details_items: Vec<(String, String)>,
    /// Number of table rows at the last draw, for clamping the scroll.
    row_count: usize,
    // Can't use TableState as it's broken: https://github.com/ratatui-org/ratatui/issues/1179
    scroll: usize,
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

impl HelpPopup {
    pub fn new(main_items: Vec<(String, String)>, details_items: Vec<(String, String)>) -> Self {
        Self {
            main_items,
            details_items,
            row_count: 0,
            scroll: 0,
        }
    }

    /// Build the section rows: a bold section title, then one row per
    /// keybinding with the description wrapped to `desc_width`.
    fn section_rows<'a>(
        title: &'a str,
        items: &'a [(String, String)],
        desc_width: usize,
    ) -> Vec<Row<'a>> {
        let mut rows = vec![Row::new([
            Cell::from(Span::from(title).bold()),
            Cell::from(""),
        ])];
        for (key, description) in items {
            let lines = wrap_text(description, desc_width);
            let height = lines.len() as u16;
            rows.push(
                Row::new([
                    Cell::from(key.as_str()),
                    Cell::from(Text::from(lines.join("\n"))),
                ])
                .height(height),
            );
        }
        rows
    }
}

impl Component for HelpPopup {
    fn draw(
        &mut self,
        f: &mut ratatui::prelude::Frame<'_>,
        area: ratatui::prelude::Rect,
    ) -> anyhow::Result<()> {
        let area = centered_rect(area, 80, 80);
        f.render_widget(Clear, area);

        let block = create_popup_block("Help (j/k: scroll)");
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

        let mut rows = Self::section_rows("Main panel", &self.main_items, desc_width);
        rows.push(Row::new([Cell::from(""), Cell::from("")]));
        rows.extend(Self::section_rows(
            "Details panel",
            &self.details_items,
            desc_width,
        ));
        self.row_count = rows.len();

        let rows: Vec<Row> = rows.into_iter().skip(self.scroll).collect();
        let widths = [
            Constraint::Length(key_width as u16 + 2),
            Constraint::Fill(1),
        ];
        f.render_widget(Table::new(rows, widths), block_inner);

        Ok(())
    }

    fn input(&mut self, event: Event) -> anyhow::Result<ComponentInputResult> {
        if let Event::Key(key) = event
            && key.kind == event::KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    // Rows can be taller than one line, so this conservatively
                    // allows scrolling until only the last row is visible.
                    self.scroll = (self.scroll + 1).min(self.row_count.saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                _ => return Ok(ComponentInputResult::NotHandled),
            }

            return Ok(ComponentInputResult::Handled);
        }

        Ok(ComponentInputResult::NotHandled)
    }
}
