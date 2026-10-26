//! Selecting conversation text with the mouse: which characters a selection
//! covers, what a copy of it holds, and the word a double click takes.
//!
//! A selection runs from one cell to another, the end not included; a cell
//! is a conversation row (counted as the view counts them) and a column.
//! Only a row's text is selected, never the marks, borders and indents
//! around it (see [`Row::text`]); half of a wide character takes all of it.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text::{Row, char_width};

/// A conversation row and a column on it.
pub(crate) type Cell = (usize, usize);

/// What a press and a drag take: a click drags by characters, a double
/// click by words, a triple click by rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Unit {
    Char,
    Word,
    Row,
}

/// The finished rows, then the rows in progress, without the leading blank row.
pub(crate) struct Rows<'a> {
    pub history: &'a [Row],
    pub live: &'a [Row],
    pub lead: usize,
}

impl Rows<'_> {
    pub fn len(&self) -> usize {
        (self.history.len() + self.live.len()).saturating_sub(self.lead)
    }

    pub fn get(&self, index: usize) -> Option<&Row> {
        let index = index + self.lead;
        match index.checked_sub(self.history.len()) {
            None => self.history.get(index),
            Some(live) => self.live.get(live),
        }
    }

    /// What `unit` takes at `cell`: the character, the word or the row's text.
    pub fn unit_at(&self, unit: Unit, (index, column): Cell) -> (Cell, Cell) {
        let row = self.get(index);
        let range = match unit {
            Unit::Char => Some(column..column.saturating_add(1)),
            Unit::Word => row.and_then(|row| word(row, column)),
            Unit::Row => row
                .and_then(|row| row.text.as_ref())
                .map(|text| text.start..usize::MAX),
        };
        let range = range.unwrap_or(column..column);
        ((index, range.start), (index, range.end))
    }

    /// The text from `start` to `end`. Rows that wrapping broke join again;
    /// each line loses its trailing blanks, and the copy its blank lines at
    /// either end.
    pub fn copied(&self, start: Cell, end: Cell) -> String {
        let mut lines: Vec<String> = Vec::new();
        for index in start.0..=end.0 {
            let Some(row) = self.get(index) else {
                break;
            };
            if row.text.is_none() {
                continue;
            }
            let piece = row_text(row, columns(index, start, end));
            match (row.joined, lines.last_mut()) {
                (Some(spaces), Some(last)) if index > start.0 => {
                    last.push_str(&" ".repeat(spaces));
                    last.push_str(&piece);
                }
                _ => lines.push(piece),
            }
        }
        let lines: Vec<&str> = lines.iter().map(|line| line.trim_end()).collect();
        let first = lines.iter().position(|line| !line.is_empty());
        let last = lines.iter().rposition(|line| !line.is_empty());
        match (first, last) {
            (Some(first), Some(last)) => lines[first..=last].join("\n"),
            _ => String::new(),
        }
    }
}

/// The columns of row `index` from `start` to `end`.
pub(crate) fn columns(index: usize, start: Cell, end: Cell) -> Range<usize> {
    let from = if index == start.0 { start.1 } else { 0 };
    let to = if index == end.0 { end.1 } else { usize::MAX };
    from..to
}

/// `row` with its text in `columns` reversed (and reversed text, such as a
/// changed word, shown plain).
pub(crate) fn highlight(row: &Row, columns: Range<usize>) -> Line<'static> {
    let mut line = row.line.clone();
    let Some(text) = &row.text else {
        return line;
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (cell, c, style) in cells(&row.line) {
        let style = if cell.within(text, &columns) {
            if style.add_modifier.contains(Modifier::REVERSED) {
                style.remove_modifier(Modifier::REVERSED)
            } else {
                style.add_modifier(Modifier::REVERSED)
            }
        } else {
            style
        };
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    line.spans = spans;
    line
}

/// The text of `row` in `columns`.
fn row_text(row: &Row, columns: Range<usize>) -> String {
    let Some(text) = &row.text else {
        return String::new();
    };
    cells(&row.line)
        .filter(|(cell, _, _)| cell.within(text, &columns))
        .map(|(_, c, _)| c)
        .collect()
}

/// The columns of the word at `column`: letters, digits and `_-./~` (so
/// paths and flags are one word), a run of blanks, or any other single
/// character. `None` off the row's text.
fn word(row: &Row, column: usize) -> Option<Range<usize>> {
    let text = row.text.as_ref()?;
    let chars: Vec<(Place, char)> = cells(&row.line)
        .filter(|(cell, _, _)| text.contains(&cell.column))
        .map(|(cell, c, _)| (cell, c))
        .collect();
    let at = chars.iter().position(|(cell, _)| cell.covers(column))?;
    let class = |c: char| {
        if c.is_alphanumeric() || "_-./~".contains(c) {
            Some(0)
        } else if c.is_whitespace() {
            Some(1)
        } else {
            None
        }
    };
    let same = |c: char| class(c).is_some() && class(c) == class(chars[at].1);
    let mut first = at;
    while first > 0 && same(chars[first - 1].1) {
        first -= 1;
    }
    let mut last = at;
    while last + 1 < chars.len() && same(chars[last + 1].1) {
        last += 1;
    }
    let end = chars[last].0;
    Some(chars[first].0.column..end.column + end.width.max(1))
}

/// Where a character is drawn.
#[derive(Debug, Clone, Copy)]
struct Place {
    column: usize,
    width: usize,
}

impl Place {
    fn covers(&self, column: usize) -> bool {
        (self.column..self.column + self.width.max(1)).contains(&column)
    }

    /// Whether the character is the row's text and in `columns`.
    fn within(&self, text: &Range<usize>, columns: &Range<usize>) -> bool {
        text.contains(&self.column)
            && self.column < columns.end
            && self.column + self.width.max(1) > columns.start
    }
}

/// Each character of `line`, where it is drawn and its style. A character
/// that takes no columns, such as a combining accent, goes with the one
/// before it.
fn cells<'a>(line: &'a Line<'static>) -> impl Iterator<Item = (Place, char, Style)> + 'a {
    let mut column = 0;
    let mut last: Option<Place> = None;
    line.spans.iter().flat_map(move |span| {
        span.content
            .chars()
            .map(|c| {
                let width = char_width(c);
                let place = match last {
                    Some(base) if width == 0 => base,
                    _ => Place { column, width },
                };
                column += width;
                last = Some(place);
                (place, c, span.style)
            })
            .collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_joins_wrapped_rows_and_skips_rows_without_text() {
        let history = vec![
            Row::text_from(Line::from("  ● Run cargo"), 4),
            Row {
                joined: Some(1),
                ..Row::text_from(Line::from("        test"), 8)
            },
            Row::marks(Line::from("    │ … 3 more lines")),
            Row::text_from(Line::from("    │ ok   "), 6),
            Row::blank(),
        ];
        let rows = Rows {
            history: &history,
            live: &[],
            lead: 0,
        };
        assert_eq!(rows.copied((0, 0), (4, usize::MAX)), "Run cargo test\nok");
        assert_eq!(rows.copied((1, 8), (1, 10)), "te");
        assert_eq!(rows.copied((2, 0), (2, usize::MAX)), "");
    }

    #[test]
    fn a_word_is_a_path_a_flag_or_one_mark() {
        let row = Row::text_from(Line::from("  see ./src/main.rs, --all (now)"), 2);
        let at = |text: &str| {
            let column = "  see ./src/main.rs, --all (now)".find(text).unwrap();
            word(&row, column)
        };
        assert_eq!(at("./src"), Some(6..19));
        assert_eq!(at(","), Some(19..20));
        assert_eq!(at("--all"), Some(21..26));
        assert_eq!(at("("), Some(27..28));
        assert_eq!(word(&row, 0), None, "the indent is not text");
        assert_eq!(word(&row, 40), None);
    }

    #[test]
    fn wide_and_combining_characters_go_whole() {
        let row = Row::text_from(Line::from("中文 e\u{301}"), 0);
        assert_eq!(row_text(&row, 1..3), "中文");
        assert_eq!(row_text(&row, 5..6), "e\u{301}");
        let line = highlight(&row, 3..4);
        let reversed: String = line
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(reversed, "文");
    }
}
