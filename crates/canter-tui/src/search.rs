//! Searching the conversation (Ctrl+F): where the words searched for are on
//! a row, and the row with them marked.
//!
//! A row is searched as drawn, all of it (a card's status too), one row at a
//! time: words that wrapping broke onto two rows are not found. Case does
//! not matter: each character is compared in lower case. A match starts and
//! ends on whole graphemes, as [`select`] counts them, so `e` is not found
//! in an accented `é` and half of an emoji is never marked.

use std::ops::Range;

use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::select;

/// What a row is searched for: `query` on one line, in lower case. Empty
/// when there is nothing to search for.
pub(crate) fn needle(query: &str) -> String {
    lower(&query.replace('\n', " "))
}

/// The columns of each match of `needle` on `line`, in order, not overlapping.
pub(crate) fn find(line: &Line<'static>, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    // Most rows have no match: look before taking the row apart. Graphemes
    // with a control character are not drawn, and are not searched.
    let drawn: String = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars())
        .filter(|c| !c.is_control())
        .collect();
    if !lower(&drawn).contains(needle) {
        return Vec::new();
    }
    // The row in lower case, and where each grapheme starts in it and is drawn.
    let mut text = String::new();
    let mut graphemes: Vec<(usize, select::Place)> = Vec::new();
    for (place, grapheme, _) in select::cells(line) {
        graphemes.push((text.len(), place));
        text.push_str(&lower(grapheme));
    }
    let starts_grapheme = |at: usize| graphemes.binary_search_by_key(&at, |(start, _)| *start);
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(needle).map(|at| from + at) {
        let end = at + needle.len();
        match starts_grapheme(at) {
            Ok(first) if end == text.len() || starts_grapheme(end).is_ok() => {
                let last = graphemes.partition_point(|(start, _)| *start < end) - 1;
                let (start, end) = (graphemes[first].1, graphemes[last].1);
                found.push(start.column..end.column + end.width.max(1));
                from = at + needle.len();
            }
            // Inside a grapheme: look again from the next character.
            _ => from = at + text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

/// `line` with each match in `matches` marked: the current one (`true`)
/// reversed, the others underlined.
pub(crate) fn mark(line: &Line<'static>, matches: &[(Range<usize>, bool)]) -> Line<'static> {
    select::restyle(line, |place, style| {
        match matches
            .iter()
            .find(|(columns, _)| columns.contains(&place.column))
        {
            Some((_, true)) => select::reverse(style),
            Some((_, false)) => style.add_modifier(Modifier::UNDERLINED),
            None => style,
        }
    })
}

/// Each character in lower case on its own, so a row and the words
/// searched for are lowered the same way, grapheme by grapheme or whole.
fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}
