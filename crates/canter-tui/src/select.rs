//! Selecting conversation text with the mouse: which characters a selection
//! covers, what a copy of it holds, the word a double click takes, and how a
//! selection stays on its text when a part of the conversation changes.
//!
//! A selection runs from one cell to another, the end not included; a cell
//! is a conversation row (counted as the view counts them) and a column.
//! Only a row's text is selected, never the marks, borders and indents
//! around it (see [`Row::text`]). Columns are counted the way ratatui draws
//! them, by grapheme: an emoji made of several characters, or a letter with
//! a combining accent, goes whole, and half of a wide character takes all of
//! it.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::text::{Row, grapheme_width};

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

impl<'a> Rows<'a> {
    pub fn len(&self) -> usize {
        (self.history.len() + self.live.len()).saturating_sub(self.lead)
    }

    pub fn get(&self, index: usize) -> Option<&'a Row> {
        let index = index + self.lead;
        match index.checked_sub(self.history.len()) {
            None => self.history.get(index),
            Some(live) => self.live.get(live),
        }
    }

    /// The rows in `range`, such as a part's.
    pub fn part(&self, range: Range<usize>) -> Vec<&'a Row> {
        range.map_while(|index| self.get(index)).collect()
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
    let mut spans: Vec<(usize, Span<'static>)> = Vec::new();
    for (place, grapheme, (from, style)) in cells(&row.line) {
        let style = if place.within(text, &columns) {
            if style.add_modifier.contains(Modifier::REVERSED) {
                style.remove_modifier(Modifier::REVERSED)
            } else {
                style.add_modifier(Modifier::REVERSED)
            }
        } else {
            style
        };
        // Pieces of different spans stay apart, so they are drawn as before.
        match spans.last_mut() {
            Some((span_from, span)) if *span_from == from && span.style == style => {
                span.content.to_mut().push_str(grapheme);
            }
            _ => spans.push((from, Span::styled(grapheme.to_owned(), style))),
        }
    }
    line.spans = spans.into_iter().map(|(_, span)| span).collect();
    line
}

/// The text of `row` in `columns`.
fn row_text(row: &Row, columns: Range<usize>) -> String {
    let Some(text) = &row.text else {
        return String::new();
    };
    cells(&row.line)
        .filter(|(place, _, _)| place.within(text, &columns))
        .map(|(_, grapheme, _)| grapheme)
        .collect()
}

/// The columns of the word at `column`: letters, digits and `_-./~` (so
/// paths and flags are one word), a run of blanks, or any other single
/// grapheme. A grapheme is what its first character is, so an accent goes
/// with its letter. `None` off the row's text.
fn word(row: &Row, column: usize) -> Option<Range<usize>> {
    let graphemes = text_cells(row);
    let at = graphemes
        .iter()
        .position(|(place, _)| place.covers(column))?;
    let class = |grapheme: &str| {
        let c = grapheme.chars().next()?;
        if c.is_alphanumeric() || "_-./~".contains(c) {
            Some(0)
        } else if c.is_whitespace() {
            Some(1)
        } else {
            None
        }
    };
    let same =
        |grapheme: &str| class(grapheme).is_some() && class(grapheme) == class(graphemes[at].1);
    let mut first = at;
    while first > 0 && same(graphemes[first - 1].1) {
        first -= 1;
    }
    let mut last = at;
    while last + 1 < graphemes.len() && same(graphemes[last + 1].1) {
        last += 1;
    }
    let end = graphemes[last].0;
    Some(graphemes[first].0.column..end.column + end.width.max(1))
}

/// Which line of a part a row shows: for command output, the line of the
/// output (see [`Row::source`]); else how many lines of the part come before
/// it. A row that wrapping broke off shows the line of the row above.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Key {
    Source(usize),
    Line(usize),
}

/// Where a selection starts or ends in a part, kept on the text rather than
/// on the rows: a line of the part and how many graphemes into it. It finds
/// the same text after the part is laid out again (a stream wraps its last
/// line again, a running command shows its newest lines), and nothing once
/// that text is gone or changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mark {
    key: Key,
    at: usize,
    /// Where a selection ends: the grapheme at `at` is not in it.
    end: bool,
    /// How many of the line's first graphemes were seen, and what they were:
    /// up to `at` and the one at it, so a line that goes on growing still
    /// matches. When they change, or a line that was empty is not, so did
    /// the text.
    seen: (usize, String),
}

/// The mark of `cell` among a part's `rows`, its row counted from the
/// part's first: where a selection starts there, or with `end`, ends.
pub(crate) fn mark(rows: &[&Row], (index, column): Cell, end: bool) -> Option<Mark> {
    let keys = keys(rows);
    let key = *keys.get(index)?;
    let (pieces, text) = line(rows, &keys, key);
    let piece = pieces.iter().find(|piece| piece.row == index)?;
    let before = piece
        .graphemes
        .iter()
        .take_while(|(place, _)| {
            if end {
                place.column < column
            } else {
                place.column + place.width.max(1) <= column
            }
        })
        .count();
    let at = piece.from + before;
    let seen = (at + 1).min(text.len());
    Some(Mark {
        key,
        at,
        end,
        seen: (seen, text[..seen].concat()),
    })
}

/// Where `mark` is among a part's `rows` now, its row counted from the
/// part's first; `None` once its line is gone or the text it saw changed.
pub(crate) fn place(rows: &[&Row], mark: &Mark) -> Option<Cell> {
    let keys = keys(rows);
    let (pieces, text) = line(rows, &keys, mark.key);
    let (count, seen) = &mark.seen;
    if text.len() < *count || text[..*count].concat() != *seen || (*count == 0 && !text.is_empty())
    {
        return None;
    }
    let text_start = |piece: &Piece| rows[piece.row].text.as_ref().map_or(0, |text| text.start);
    let first = pieces.first()?;
    if mark.end {
        // After the last grapheme before the mark.
        let mut cell = (first.row, text_start(first));
        for piece in pieces.iter().take_while(|piece| piece.from < mark.at) {
            let before = mark.at - piece.from;
            let column = match piece.graphemes.get(before - 1) {
                Some((place, _)) if before < piece.graphemes.len() => {
                    place.column + place.width.max(1)
                }
                _ => usize::MAX,
            };
            cell = (piece.row, column);
        }
        return Some(cell);
    }
    // On the grapheme at the mark; in the spaces wrapping dropped, on the
    // first one after them.
    for piece in &pieces {
        if mark.at < piece.from + piece.graphemes.len() {
            let column = match mark.at.checked_sub(piece.from) {
                Some(before) => piece.graphemes[before].0.column,
                None => text_start(piece),
            };
            return Some((piece.row, column));
        }
    }
    Some((pieces.last()?.row, usize::MAX))
}

/// The line each of a part's `rows` shows.
fn keys(rows: &[&Row]) -> Vec<Key> {
    let mut keys: Vec<Key> = Vec::with_capacity(rows.len());
    let mut lines = 0;
    for row in rows {
        let key = match (row.source, row.joined, keys.last()) {
            (Some(source), _, _) => Key::Source(source),
            (None, Some(_), Some(&above)) => above,
            _ => {
                lines += 1;
                Key::Line(lines - 1)
            }
        };
        keys.push(key);
    }
    keys
}

/// A row of a line: which of the part's rows, where its text starts in the
/// line, and its graphemes.
struct Piece<'a> {
    row: usize,
    from: usize,
    graphemes: Vec<(Place, &'a str)>,
}

/// The rows showing the line `key`, and the line's graphemes: the text of
/// its rows, joined again with the spaces wrapping dropped.
fn line<'a>(rows: &[&'a Row], keys: &[Key], key: Key) -> (Vec<Piece<'a>>, Vec<&'a str>) {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut text = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if keys[index] != key {
            continue;
        }
        if !pieces.is_empty() {
            text.extend(std::iter::repeat_n(" ", row.joined.unwrap_or(0)));
        }
        let graphemes = text_cells(row);
        let from = text.len();
        text.extend(graphemes.iter().map(|(_, grapheme)| *grapheme));
        pieces.push(Piece {
            row: index,
            from,
            graphemes,
        });
    }
    (pieces, text)
}

/// Where a grapheme is drawn.
#[derive(Debug, Clone, Copy)]
struct Place {
    column: usize,
    width: usize,
}

impl Place {
    fn covers(&self, column: usize) -> bool {
        (self.column..self.column + self.width.max(1)).contains(&column)
    }

    /// Whether the grapheme is the row's text and in `columns`.
    fn within(&self, text: &Range<usize>, columns: &Range<usize>) -> bool {
        text.contains(&self.column)
            && self.column < columns.end
            && self.column + self.width.max(1) > columns.start
    }
}

/// The graphemes of `row`'s text and where they are drawn.
fn text_cells(row: &Row) -> Vec<(Place, &str)> {
    let Some(text) = &row.text else {
        return Vec::new();
    };
    cells(&row.line)
        .filter(|(place, _, _)| text.contains(&place.column))
        .map(|(place, grapheme, _)| (place, grapheme))
        .collect()
}

/// Each grapheme of `line` as ratatui draws it, where, and which span it is
/// from with that span's style. Control characters are not drawn and left
/// out; a grapheme that takes no columns goes with the one before it.
fn cells<'a>(
    line: &'a Line<'static>,
) -> impl Iterator<Item = (Place, &'a str, (usize, Style))> + 'a {
    let mut column = 0;
    let mut last: Option<Place> = None;
    line.spans
        .iter()
        .enumerate()
        .flat_map(move |(index, span)| {
            span.styled_graphemes(Style::default())
                .map(|grapheme| {
                    let width = grapheme_width(grapheme.symbol);
                    let place = match last {
                        Some(base) if width == 0 => base,
                        _ => Place { column, width },
                    };
                    column += width;
                    last = Some(place);
                    (place, grapheme.symbol, (index, span.style))
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

    /// The columns are where ratatui draws: an emoji of several characters
    /// takes two, and a word goes on with an accented letter.
    #[test]
    fn graphemes_are_counted_as_drawn() {
        let row = Row::text_from(Line::from("👩\u{200d}💻 done e\u{301}clair"), 0);
        assert_eq!(row_text(&row, 0..2), "👩\u{200d}💻");
        assert_eq!(row_text(&row, 3..7), "done");
        assert_eq!(word(&row, 8), Some(8..14));
        assert_eq!(row_text(&row, 8..14), "e\u{301}clair");
    }

    /// A mark finds its text again after the line wraps another way, and
    /// nothing once the line is gone or its text changed.
    #[test]
    fn a_mark_stays_on_its_text() {
        let wide = [Row::text_from(Line::from("hello abcdef"), 0)];
        let narrow = [
            Row::text_from(Line::from("hello"), 0),
            Row {
                joined: Some(1),
                ..Row::text_from(Line::from("  abcdefgh"), 2)
            },
        ];
        let wide: Vec<&Row> = wide.iter().collect();
        let narrow: Vec<&Row> = narrow.iter().collect();
        let start = mark(&wide, (0, 6), false).unwrap();
        let end = mark(&wide, (0, 12), true).unwrap();
        assert_eq!(place(&narrow, &start), Some((1, 2)));
        assert_eq!(place(&narrow, &end), Some((1, 8)));

        let changed = [Row::text_from(Line::from("hello abXdef"), 0)];
        let changed: Vec<&Row> = changed.iter().collect();
        assert_eq!(place(&changed, &start), Some((0, 6)));
        assert_eq!(place(&changed, &end), None);

        let output = |source: usize, text: &str| Row {
            source: Some(source),
            ..Row::text_from(Line::from(format!("    {text}")), 4)
        };
        let running = [output(1, "line2"), output(2, "line3")];
        let running: Vec<&Row> = running.iter().collect();
        let line3 = mark(&running, (1, 4), false).unwrap();
        let more = [output(2, "line3"), output(3, "line4")];
        assert_eq!(
            place(&more.iter().collect::<Vec<_>>(), &line3),
            Some((0, 4))
        );
        let folded = [output(3, "line4")];
        assert_eq!(place(&folded.iter().collect::<Vec<_>>(), &line3), None);

        // An empty line stays empty: a line that took its place is other text.
        let gap = [Row::blank(), Row::from(Line::from("next"))];
        let gap = mark(&gap.iter().collect::<Vec<_>>(), (0, 0), true).unwrap();
        let moved = [Row::from(Line::from("next"))];
        assert_eq!(place(&moved.iter().collect::<Vec<_>>(), &gap), None);
    }
}
