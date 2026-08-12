mod large_string;
use ansi_to_tui::IntoText;
pub use large_string::LargeString;
use ratatui::crossterm::event::MouseButton;
use ratatui::crossterm::event::MouseEvent;
use ratatui::crossterm::event::MouseEventKind;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Text;

use crate::env::JJLayout;

/// Tracks the split position between two panes and handles drag-to-resize mouse events.
pub struct PaneDivider {
    init_percent: u16,
    size: Option<u16>,
    dragging: bool,
    rects: [Rect; 2],
}

impl PaneDivider {
    pub fn new(percent: u16) -> Self {
        Self {
            init_percent: percent.min(100),
            size: None,
            dragging: false,
            rects: [Rect::ZERO, Rect::ZERO],
        }
    }

    /// Split `area` into two panes at the current divider position and remember
    /// the resulting rects for hit-testing in `handle_mouse`.
    pub fn split(&mut self, area: Rect, layout: JJLayout) -> [Rect; 2] {
        let total = match layout {
            JJLayout::Horizontal => area.width,
            JJLayout::Vertical => area.height,
        };
        let size = match self.size {
            None => {
                let s = ((total as u32 * self.init_percent as u32) / 100) as u16;
                self.size = Some(s);
                s
            }
            Some(s) => s,
        };
        let size = size.min(total);

        let chunks = Layout::default()
            .direction(layout.into())
            .constraints([Constraint::Length(size), Constraint::Fill(1)])
            .split(area);
        self.rects = [chunks[0], chunks[1]];
        self.rects
    }

    /// Handle a mouse event. Returns true if the event was consumed.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, layout: JJLayout) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.dragging = false;
                if self.on_border(mouse.column, mouse.row, layout) {
                    self.dragging = true;
                    self.update_size(mouse.column, mouse.row, layout);
                    true
                } else {
                    false
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                self.update_size(mouse.column, mouse.row, layout);
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.dragging = false;
                true
            }
            _ => false,
        }
    }

    fn on_border(&self, col: u16, row: u16, layout: JJLayout) -> bool {
        let [r0, r1] = self.rects;
        match layout {
            JJLayout::Horizontal => {
                let in_row = row >= r0.top() && row < r0.bottom();
                // Right border of r0 and left border of r1 are adjacent columns.
                let on_col = col == r0.right().saturating_sub(1) || col == r1.left();
                in_row && on_col
            }
            JJLayout::Vertical => {
                let in_col = col >= r0.left() && col < r0.right();
                let on_row = row == r0.bottom().saturating_sub(1) || row == r1.top();
                in_col && on_row
            }
        }
    }

    fn update_size(&mut self, col: u16, row: u16, layout: JJLayout) {
        let [r0, r1] = self.rects;
        let (pos, total) = match layout {
            JJLayout::Horizontal => (
                col.saturating_sub(r0.left()),
                r1.right().saturating_sub(r0.left()),
            ),
            JJLayout::Vertical => (
                row.saturating_sub(r0.top()),
                r1.bottom().saturating_sub(r0.top()),
            ),
        };
        // pos is a 0-based cell index, so it tops out at total-1; snap to
        // total when the mouse reaches the far edge so the first pane can
        // expand to full size. Enforce a minimum of 1 so the pane stays visible.
        let size = if pos >= total.saturating_sub(1) {
            total
        } else {
            pos.max(1)
        };
        self.size = Some(size);
    }
}

pub fn centered_rect(r: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn centered_rect_line_height(r: Rect, percent_x: u16, lines_y: u16) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(lines_y),
            Constraint::Fill(1),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Center a rect of fixed width and height within an outside rect
pub fn centered_rect_fixed(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;

    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// replaces tabs in a string by spaces
///
/// ratatui doesn't work well displaying tabs, so any
/// string that is rendered and might contain tabs
/// needs to have the tabs converted to spaces.
///
/// this function aligns tabs in the input string to
/// virtual tab stops 4 spaces apart, taking care
/// to count ansi control sequences as zero width.
///
/// unprintable control characters are also replaced, via
/// [scrub_control_chars], and undecodable runs are collapsed,
/// via [elide_binary_lines]; the passes are combined here
/// because every string that is rendered needs all three.
///
/// order matters: scrubbing runs first so control bytes are
/// resolved before any width is measured, and eliding before
/// tab expansion means a dropped line's tabs are never
/// expanded. the two passes use different substitute
/// characters precisely so scrubbing cannot feed elision.
pub fn tabs_to_spaces(line: &str) -> String {
    tabs_to_spaces_inner(&elide_binary_lines(&scrub_control_chars(line)))
}

fn tabs_to_spaces_inner(line: &str) -> String {
    const TAB_WIDTH: usize = 4;

    enum AnsiState {
        Neutral,
        Escape,
        Csi,
    }

    let mut out = String::new();
    let mut x = 0;
    let mut ansi_state = AnsiState::Neutral;
    for c in line.chars() {
        match ansi_state {
            AnsiState::Neutral => {
                if c == '\t' {
                    loop {
                        out.push(' ');
                        x += 1;
                        if x % TAB_WIDTH == 0 {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                    if c == '\x1b' {
                        ansi_state = AnsiState::Escape;
                    } else {
                        x += 1;
                    }
                }
                if c == '\r' || c == '\n' {
                    x = 0;
                }
            }
            AnsiState::Escape => {
                out.push(c);
                ansi_state = if c == '[' {
                    AnsiState::Csi
                } else {
                    AnsiState::Neutral
                };
            }
            AnsiState::Csi => {
                out.push(c);
                if ('\x40'..='\x7f').contains(&c) {
                    ansi_state = AnsiState::Neutral;
                }
            }
        }
    }
    out
}

/// Fraction of a line that must be undecodable before it is elided by
/// [elide_binary_lines]. Well above what real text hits -- a diff of UTF-8
/// prose decodes cleanly, and even a stray mis-encoded byte is a small share of
/// its line -- and well below the ~20% or more that raw binary produces.
const BINARY_LINE_THRESHOLD: f32 = 0.15;

/// Replaces runs of undecodable content with a short placeholder line.
///
/// The per-file binary check in the files pane cannot help the log pane: `jj
/// show` accepts revsets only, with no fileset argument, so a revision's binary
/// files cannot be filtered out of its diff. Their bytes therefore reach the
/// renderer, and after lossy decoding they arrive as dense `U+FFFD` runs.
///
/// A line is judged binary by the share of it that failed to decode, which
/// separates the two cases cleanly: text stays near zero even when it contains
/// an occasional bad byte, while raw binary is a large fraction. Consecutive
/// binary lines collapse into one placeholder so a large file does not push the
/// real diff off screen.
pub fn elide_binary_lines(text: &str) -> String {
    let mut out = String::new();
    let mut eliding = false;

    for line in text.split_inclusive('\n') {
        let total = line.chars().filter(|c| !c.is_whitespace()).count();
        let undecodable = line.chars().filter(|&c| c == '\u{fffd}').count();
        let is_binary = total > 0 && (undecodable as f32 / total as f32) >= BINARY_LINE_THRESHOLD;

        if is_binary {
            // Collapse a run of binary lines into a single placeholder.
            if !eliding {
                out.push_str("    (binary content omitted)\n");
                eliding = true;
            }
        } else {
            out.push_str(line);
            eliding = false;
        }
    }

    out
}

/// Replaces unprintable C0 control characters with `U+FFFD`.
///
/// jj's binary detection is NUL-byte based, so a file that contains stray
/// control bytes but no NUL is still diffed as text and those bytes reach the
/// renderer. ratatui draws them as zero-width or garbage cells, which corrupts
/// the layout of every following column, so they are made visible instead.
///
/// Tab, carriage return and newline are kept: they are meaningful layout
/// characters, and tabs are expanded separately by [tabs_to_spaces]. Escape is
/// kept too, since jj's output is colored and the escape sequences that carry
/// that styling are parsed downstream by `ansi-to-tui`.
///
/// The substitute is `U+2426`, not the `U+FFFD` that lossy decoding produces:
/// [elide_binary_lines] counts `U+FFFD` to judge a line binary, and a readable
/// line that merely contains control bytes must not be mistaken for one.
pub fn scrub_control_chars(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' | '\x1b' => c,
            // C0 controls plus DEL. Other Unicode control characters are left
            // alone; they are rare in diffs and may be legitimate content.
            c if c.is_control() && (c < '\u{20}' || c == '\u{7f}') => '\u{2426}',
            c => c,
        })
        .collect()
}

/// Render a jj error message as styled text.
///
/// jj is run with `--color always`, so its errors arrive carrying ANSI escapes.
/// Putting that string straight into a widget prints the escapes literally
/// (`[1m[38;5;1mError: ...`), so they are parsed into styles here. A message
/// that fails to parse is shown as-is in red rather than dropped.
pub fn error_text(message: &str) -> Text<'static> {
    match message.into_text() {
        Ok(text) => Text::from(
            text.lines
                .into_iter()
                .map(|line| line.to_owned())
                .collect::<Vec<_>>(),
        ),
        Err(_) => Text::from(message.to_owned()).fg(Color::Red),
    }
}

#[cfg(test)]
mod error_text_tests {
    use super::*;

    #[test]
    fn scrub_control_chars_replaces_unprintables() {
        // jj's binary detection keys on NUL, so a file with other stray control
        // bytes is diffed as text and those bytes reach the renderer.
        assert_eq!(
            scrub_control_chars("ok\x00\x01\x02"),
            "ok\u{2426}\u{2426}\u{2426}"
        );
        assert_eq!(scrub_control_chars("bell\x07"), "bell\u{2426}");
        assert_eq!(scrub_control_chars("del\x7f"), "del\u{2426}");
    }

    #[test]
    fn scrub_control_chars_does_not_trip_binary_elision() {
        // A readable line that merely contains control bytes must survive both
        // passes: the scrubber's marker is deliberately not the one elision
        // counts.
        let text = "ok\n\x00\x00control-bytes\x07here\n";
        let rendered = tabs_to_spaces(text);
        assert!(
            rendered.contains("control-bytes"),
            "line was elided: {rendered:?}"
        );
    }

    #[test]
    fn elide_binary_lines_collapses_undecodable_runs() {
        // Lossy decoding turns binary into dense U+FFFD; consecutive such lines
        // collapse to a single placeholder.
        let text = "keep me\n\u{fffd}\u{fffd}\u{fffd}\u{fffd}\n\u{fffd}\u{fffd}\u{fffd}\u{fffd}\nkeep me too\n";
        let elided = elide_binary_lines(text);
        assert_eq!(
            elided,
            "keep me\n    (binary content omitted)\nkeep me too\n"
        );
    }

    #[test]
    fn elide_binary_lines_keeps_mostly_text_lines() {
        // One bad byte in a line of prose is far below the threshold.
        let text = "a mostly readable line with one \u{fffd} bad byte\n";
        assert_eq!(elide_binary_lines(text), text);
    }

    #[test]
    fn scrub_control_chars_keeps_layout_and_ansi() {
        // Tabs, newlines and carriage returns carry layout, and the escape
        // character introduces the color sequences parsed downstream. Scrubbing
        // any of them would break rendering rather than fix it.
        assert_eq!(scrub_control_chars("a\tb\r\nc"), "a\tb\r\nc");
        let colored = "\x1b[1mbold\x1b[0m";
        assert_eq!(scrub_control_chars(colored), colored);
    }

    #[test]
    fn tabs_to_spaces_scrubs_and_aligns() {
        // Scrubbing runs first, so the substitute character occupies a column
        // and the following tab advances to the next 4-wide stop.
        assert_eq!(tabs_to_spaces("\x00\tx"), "\u{2426}   x");
    }

    #[test]
    fn error_text_parses_ansi_into_styles() {
        // jj runs with `--color always`, so its errors carry escapes. They must
        // become styling, not literal `[1m[38;5;1m` text in the popup.
        let colored = "\x1b[1m\x1b[38;5;1mError: \x1b[39mRefusing to move tag: tags\x1b[0m";
        let text = error_text(colored);

        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered, "Error: Refusing to move tag: tags");
        assert!(!rendered.contains('\x1b'), "escape survived: {rendered:?}");
        assert!(!rendered.contains("[1m"), "escape survived: {rendered:?}");
    }

    #[test]
    fn error_text_keeps_every_line() {
        // The "Hint:" line carries the actionable advice, so it must survive.
        let message =
            "Error: Refusing to move tag: tags\nHint: Use --allow-move to update existing tags.";
        let text = error_text(message);
        assert_eq!(text.lines.len(), 2, "got {:?}", text.lines);
    }

    #[test]
    fn error_text_passes_plain_messages_through() {
        let text = error_text("something went wrong");
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered, "something went wrong");
    }
}
