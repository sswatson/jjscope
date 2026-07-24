/*! Shared vim-style search used by the log and bookmarks tabs.

Both tabs offer an identical `/` search: type a query (matches highlight live
as you type), press Enter to jump the selection to the first match, then
`n`/`N` to step to the next/previous match (wrapping). `Esc` clears it.
Matching is case-insensitive and covers only the text shown in the pane.

This module holds the pieces that make that behavior identical across panes:

* [SearchState] — the active query, set as the user types.
* [highlight_matches] — restyle matched substrings in a rendered [Line].
* [match_indices] / [next_match_index] / [first_match_index_at_or_after] —
  index math for navigating a list of items by their displayed text.
*/

use ratatui::prelude::*;

/// The active search query for a pane. `None` means no search is in effect;
/// otherwise the query is stored already lowercased for case-insensitive
/// matching.
#[derive(Debug, Default, Clone)]
pub struct SearchState {
    query: Option<String>,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a (non-empty) search is currently active.
    pub fn is_active(&self) -> bool {
        self.query.is_some()
    }

    /// The active query (lowercased), or `None`.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Set the query from raw user input. An empty or whitespace-only string
    /// clears the search.
    pub fn set_query(&mut self, query: &str) {
        let trimmed = query.trim();
        self.query = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_lowercase())
        };
    }

    /// Clear the search.
    pub fn clear(&mut self) {
        self.query = None;
    }
}

/// Style applied to search-match substrings. A yellow background with black
/// text, mirroring the common editor "Search" highlight, chosen to stand out
/// against the selection background rather than blend into it.
pub fn search_match_style() -> Style {
    Style::default().bg(Color::Yellow).fg(Color::Black)
}

/// The indices of `items` that match `query` (already lowercased), in order.
/// `text_of` yields the displayed text of an item; matching is a
/// case-insensitive substring test. An empty query yields no matches.
pub fn match_indices<T>(items: &[T], query: &str, text_of: impl Fn(&T) -> String) -> Vec<usize> {
    if query.is_empty() {
        return vec![];
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| text_of(item).to_lowercase().contains(query))
        .map(|(i, _)| i)
        .collect()
}

/// Given the ordered list of matching positions and the current selection
/// position (an index into the same space the matches live in), return the
/// position of the next (`forward`) or previous match, wrapping around.
/// Returns `None` if there are no matches.
///
/// If the selection is itself a match, navigation moves off it. If it isn't,
/// the nearest match in the given direction is chosen (wrapping).
pub fn next_match_index(matches: &[usize], current: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    match matches.iter().position(|m| *m == current) {
        Some(pos) => {
            let len = matches.len() as isize;
            let step = if forward { 1 } else { -1 };
            let next = (pos as isize + step).rem_euclid(len) as usize;
            Some(matches[next])
        }
        None => {
            if forward {
                matches
                    .iter()
                    .find(|m| **m > current)
                    .or_else(|| matches.first())
                    .copied()
            } else {
                matches
                    .iter()
                    .rfind(|m| **m < current)
                    .or_else(|| matches.last())
                    .copied()
            }
        }
    }
}

/// The first match at or after `current` (wrapping to the first match).
/// Used on Enter so the initial jump can land on an already-selected match.
/// Returns `None` if there are no matches.
pub fn first_match_index_at_or_after(matches: &[usize], current: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    matches
        .iter()
        .find(|m| **m >= current)
        .or_else(|| matches.first())
        .copied()
}

/// Restyle every case-insensitive occurrence of `query` (already lowercased)
/// within `line` with [search_match_style], splitting spans at match
/// boundaries so only the matched characters are highlighted. Empty queries
/// are a no-op.
pub fn highlight_matches(line: &mut Line, query: &str) {
    if query.is_empty() {
        return;
    }

    // Flatten to (char, style) so matches can be found across span boundaries
    // and re-split cleanly. Log/bookmark lines are short, so this is cheap.
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let text: String = chars.iter().map(|(c, _)| *c).collect();
    let lower = text.to_lowercase();

    // If lowercasing changed the byte length (rare Unicode), fall back to a
    // char-window comparison; otherwise map byte offsets back to char indices.
    if lower.len() != text.len() {
        highlight_matches_by_chars(line, &chars, query);
        return;
    }

    let mut byte_to_char = vec![0usize; lower.len() + 1];
    for (char_idx, (byte_idx, _)) in text.char_indices().enumerate() {
        byte_to_char[byte_idx] = char_idx;
    }
    byte_to_char[lower.len()] = chars.len();

    let mut matched = vec![false; chars.len()];
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(query) {
        let start_byte = search_from + rel;
        let end_byte = start_byte + query.len();
        let start_char = byte_to_char[start_byte];
        let end_char = byte_to_char[end_byte];
        for m in matched.iter_mut().take(end_char).skip(start_char) {
            *m = true;
        }
        search_from = end_byte.max(start_byte + 1);
        if search_from > lower.len() {
            break;
        }
    }

    rebuild_line_with_matches(line, &chars, &matched);
}

/// Fallback matcher used when lowercasing changes byte length: compares a
/// sliding window of lowercased chars against the query's chars.
fn highlight_matches_by_chars(line: &mut Line, chars: &[(char, Style)], query: &str) {
    let query_chars: Vec<char> = query.chars().collect();
    let lower_chars: Vec<char> = chars
        .iter()
        .flat_map(|(c, _)| c.to_lowercase())
        .collect::<Vec<_>>();
    // Only safe to index-align when lowercasing was 1:1 per char.
    if lower_chars.len() != chars.len() {
        return;
    }
    let mut matched = vec![false; chars.len()];
    if query_chars.len() <= lower_chars.len() {
        for start in 0..=(lower_chars.len() - query_chars.len()) {
            if lower_chars[start..start + query_chars.len()] == query_chars[..] {
                for m in matched.iter_mut().take(start + query_chars.len()).skip(start) {
                    *m = true;
                }
            }
        }
    }
    rebuild_line_with_matches(line, chars, &matched);
}

/// Rebuild `line.spans` from `chars`, applying [search_match_style] to the
/// runs where `matched` is true and preserving each char's original style
/// elsewhere. Adjacent chars sharing (style, matched) are merged into one span.
fn rebuild_line_with_matches(line: &mut Line, chars: &[(char, Style)], matched: &[bool]) {
    if !matched.iter().any(|m| *m) {
        return;
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut current = String::new();
    let mut current_style: Option<Style> = None;

    for (i, (c, base_style)) in chars.iter().enumerate() {
        let style = if matched[i] {
            base_style.patch(search_match_style())
        } else {
            *base_style
        };
        if current_style == Some(style) {
            current.push(*c);
        } else {
            if let Some(prev) = current_style {
                spans.push(Span::styled(std::mem::take(&mut current), prev));
            }
            current.push(*c);
            current_style = Some(style);
        }
    }
    if let Some(prev) = current_style {
        spans.push(Span::styled(current, prev));
    }
    line.spans = spans;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn is_highlighted(line: &Line, idx: usize) -> bool {
        let mut seen = 0;
        for span in &line.spans {
            for _ in span.content.chars() {
                if seen == idx {
                    return span.style.bg == Some(Color::Yellow);
                }
                seen += 1;
            }
        }
        false
    }

    #[test]
    fn highlight_matches_marks_only_the_query() {
        let mut line = Line::from("zeta apple cart");
        highlight_matches(&mut line, "apple");
        assert_eq!(line_text(&line), "zeta apple cart");
        for i in 0..15 {
            assert_eq!(is_highlighted(&line, i), (5..10).contains(&i), "char {i}");
        }
    }

    #[test]
    fn highlight_matches_is_case_insensitive() {
        let mut line = Line::from("Delta APPLE pie");
        highlight_matches(&mut line, "apple");
        for i in 0..15 {
            assert_eq!(is_highlighted(&line, i), (6..11).contains(&i), "char {i}");
        }
    }

    #[test]
    fn highlight_matches_multiple_occurrences() {
        let mut line = Line::from("aXaXa");
        highlight_matches(&mut line, "a");
        for i in 0..5 {
            assert_eq!(is_highlighted(&line, i), i % 2 == 0, "char {i}");
        }
    }

    #[test]
    fn highlight_matches_no_match_leaves_line_untouched() {
        let mut line = Line::from("nothing here");
        let before = line.clone();
        highlight_matches(&mut line, "zzz");
        assert_eq!(line_text(&line), line_text(&before));
        for i in 0..12 {
            assert!(!is_highlighted(&line, i), "char {i}");
        }
    }

    #[test]
    fn highlight_matches_preserves_existing_span_styles() {
        let mut line = Line::from(vec![
            Span::styled("foo", Style::default().fg(Color::Red)),
            Span::styled("bar", Style::default().fg(Color::Blue)),
        ]);
        highlight_matches(&mut line, "oba");
        assert_eq!(line_text(&line), "foobar");
        for i in 0..6 {
            assert_eq!(is_highlighted(&line, i), (2..5).contains(&i), "char {i}");
        }
        assert_eq!(line.spans.first().unwrap().style.fg, Some(Color::Red));
        assert_eq!(line.spans.last().unwrap().style.fg, Some(Color::Blue));
    }

    #[test]
    fn highlight_matches_empty_query_is_noop() {
        let mut line = Line::from("anything");
        let before = line.clone();
        highlight_matches(&mut line, "");
        assert_eq!(line_text(&line), line_text(&before));
        for i in 0..8 {
            assert!(!is_highlighted(&line, i));
        }
    }

    #[test]
    fn match_indices_finds_case_insensitive_substrings() {
        let items = vec!["alpha", "Beta apple", "gamma", "zeta APPLE"];
        let matches = match_indices(&items, "apple", |s| s.to_string());
        assert_eq!(matches, vec![1, 3]);
    }

    #[test]
    fn match_indices_empty_query_is_empty() {
        let items = vec!["a", "b"];
        assert!(match_indices(&items, "", |s| s.to_string()).is_empty());
    }

    #[test]
    fn next_match_index_wraps_forward_and_back() {
        let matches = vec![1, 3, 5];
        // On a match: step off it.
        assert_eq!(next_match_index(&matches, 1, true), Some(3));
        assert_eq!(next_match_index(&matches, 5, true), Some(1)); // wrap
        assert_eq!(next_match_index(&matches, 3, false), Some(1));
        assert_eq!(next_match_index(&matches, 1, false), Some(5)); // wrap
    }

    #[test]
    fn next_match_index_from_non_match_picks_nearest_in_direction() {
        let matches = vec![1, 3, 5];
        // Selection at 2 (not a match): forward -> first match > 2 = 3.
        assert_eq!(next_match_index(&matches, 2, true), Some(3));
        // backward -> last match < 2 = 1.
        assert_eq!(next_match_index(&matches, 2, false), Some(1));
        // Past the end forward wraps to first.
        assert_eq!(next_match_index(&matches, 9, true), Some(1));
        // Before the start backward wraps to last.
        assert_eq!(next_match_index(&matches, 0, false), Some(5));
    }

    #[test]
    fn first_match_at_or_after_prefers_current_then_wraps() {
        let matches = vec![2, 4, 6];
        assert_eq!(first_match_index_at_or_after(&matches, 0), Some(2));
        assert_eq!(first_match_index_at_or_after(&matches, 4), Some(4)); // inclusive
        assert_eq!(first_match_index_at_or_after(&matches, 5), Some(6));
        assert_eq!(first_match_index_at_or_after(&matches, 9), Some(2)); // wrap
    }

    #[test]
    fn empty_matches_yield_none() {
        assert_eq!(next_match_index(&[], 0, true), None);
        assert_eq!(first_match_index_at_or_after(&[], 0), None);
    }
}
