//! Text helpers: cleaning untrusted text, display width and wrapping.

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// One line before wrapping: `indent` starts the first row, `hang` starts each
/// wrapped row after it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LogicalLine {
    pub indent: Vec<Span<'static>>,
    pub hang: Vec<Span<'static>>,
    pub body: Vec<Span<'static>>,
    /// The last columns of `indent` that a copy keeps with the text, such as
    /// a list item's bullet. The rest of the indent and the hang are left out.
    pub marker: usize,
}

impl LogicalLine {
    /// Wrapped rows line up under the first row's text.
    pub fn new(indent: Vec<Span<'static>>, body: Vec<Span<'static>>) -> Self {
        let hang = vec![Span::raw(" ".repeat(spans_width(&indent)))];
        Self {
            indent,
            hang,
            body,
            marker: 0,
        }
    }

    /// Adds `first` before the indent and `rest` before the hang.
    pub fn prefixed(mut self, first: Span<'static>, rest: Span<'static>) -> Self {
        self.indent.insert(0, first);
        self.hang.insert(0, rest);
        self
    }
}

/// A row of the conversation, and what a copy takes from it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub line: Line<'static>,
    /// The columns a copy takes: the marks, borders and indents before them
    /// and the status after them are left out. `None` for a row with nothing
    /// to copy, such as `… 3 more lines · Ctrl+O`.
    pub text: Option<Range<usize>>,
    /// On a row that wrapping broke off the row above: how many spaces it
    /// dropped at the break. A copy joins the two rows again with them.
    pub joined: Option<usize>,
}

impl Row {
    /// A row whose text starts at column `from`.
    pub fn text_from(line: Line<'static>, from: usize) -> Self {
        Self {
            line,
            text: Some(from..usize::MAX),
            joined: None,
        }
    }

    /// A row with nothing to copy.
    pub fn marks(line: Line<'static>) -> Self {
        Self {
            line,
            text: None,
            joined: None,
        }
    }

    /// An empty row; a copy keeps it as an empty line.
    pub fn blank() -> Self {
        Self::from(Line::default())
    }

    pub fn is_blank(&self) -> bool {
        self.line.width() == 0
    }
}

impl From<Line<'static>> for Row {
    fn from(line: Line<'static>) -> Self {
        Self::text_from(line, 0)
    }
}

pub(crate) fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

pub(crate) fn str_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

pub(crate) fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|span| str_width(&span.content)).sum()
}

/// Makes server text safe to draw: drops escape sequences and control
/// characters, expands tabs, folds `\r\n` to `\n`, and treats a lone `\r` as
/// "start the line again" (progress bars).
pub(crate) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line_start = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\n' => {
                out.push('\n');
                line_start = out.len();
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => out.truncate(line_start),
            '\t' => {
                let column = str_width(&out[line_start..]);
                out.push_str(&" ".repeat(4 - column % 4));
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Draws text as it is, for what the user is asked to approve: control
/// characters show as escapes (`\r`, `\x1b`, `\u{9b}`) instead of acting or
/// vanishing, so nothing can hide part of a command or diff. Bidi controls,
/// which reorder what a terminal draws, show as escapes too. Newlines still
/// break lines and tabs still become spaces.
pub(crate) fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line_start = 0;
    for c in text.chars() {
        match c {
            '\n' => {
                out.push('\n');
                line_start = out.len();
            }
            '\t' => {
                let column = str_width(&out[line_start..]);
                out.push_str(&" ".repeat(4 - column % 4));
            }
            '\r' => out.push_str("\\r"),
            c if c.is_ascii_control() => out.push_str(&format!("\\x{:02x}", u32::from(c))),
            c if c.is_control() || is_bidi_control(c) => {
                out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars>) {
    match chars.next() {
        // CSI: parameters, then one final byte in @..~.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC, DCS, SOS, PM, APC: up to BEL or ESC \.
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' {
                    chars.next_if_eq(&'\\');
                    break;
                }
            }
        }
        _ => {}
    }
}

/// Cuts spans to `width` columns, ending with `…` when something was cut.
pub(crate) fn truncate(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    let budget = width.saturating_sub(1);
    for span in spans {
        let mut kept = String::new();
        for c in span.content.chars() {
            let w = char_width(c);
            if used + w > budget {
                break;
            }
            used += w;
            kept.push(c);
        }
        let full = kept.len() == span.content.len();
        if !kept.is_empty() {
            out.push(Span::styled(kept, span.style));
        }
        if !full {
            out.push(Span::styled("…", span.style));
            break;
        }
    }
    out
}

/// Wraps one logical line into rows no wider than `width`. Breaks after
/// spaces or next to wide (CJK) characters, and mid-word only when a word is
/// longer than a row.
pub(crate) fn wrap(line: &LogicalLine, width: u16) -> Vec<Line<'static>> {
    wrap_rows(line, width)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// [`wrap`], keeping what a copy needs: the text starts after the indent
/// (but takes the list marker) and after the hang, and wrapped rows join the
/// row above.
pub(crate) fn wrap_rows(line: &LogicalLine, width: u16) -> Vec<Row> {
    let width = usize::from(width);
    let cells: Vec<(char, Style)> = line
        .body
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut start = 0;
    let mut dropped = 0;
    loop {
        let first = rows.is_empty();
        let prefix = if first { &line.indent } else { &line.hang };
        let prefix_width = spans_width(prefix);
        let room = width.saturating_sub(prefix_width).max(1);
        let (len, next) = break_row(&cells[start..], room);
        let mut spans = prefix.clone();
        spans.extend(group(&cells[start..start + len]));
        let line_out = Line::from(spans);
        rows.push(if first {
            Row::text_from(line_out, prefix_width.saturating_sub(line.marker))
        } else {
            Row {
                joined: Some(dropped),
                ..Row::text_from(line_out, prefix_width)
            }
        });
        dropped = next - len;
        start += next;
        if start >= cells.len() {
            return rows;
        }
    }
}

/// Makes text safe to put on the clipboard while keeping it as it was: drops
/// escape sequences and control characters other than newlines and tabs.
pub(crate) fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Returns how many cells go on this row and how many to consume (the
/// consumed count also skips the spaces at the break).
fn break_row(cells: &[(char, Style)], room: usize) -> (usize, usize) {
    let mut used = 0;
    let mut last_break = None;
    for (i, &(c, _)) in cells.iter().enumerate() {
        let w = char_width(c);
        if i > 0 && (w > 1 || char_width(cells[i - 1].0) > 1) {
            last_break = Some(i);
        }
        if used + w > room {
            if c == ' ' {
                return (trim_end(cells, i), skip_spaces(cells, i));
            }
            return match last_break {
                Some(at) if at > 0 => (trim_end(cells, at), skip_spaces(cells, at)),
                _ => (i.max(1), i.max(1)),
            };
        }
        used += w;
        if c == ' ' {
            last_break = Some(i + 1);
        }
    }
    (cells.len(), cells.len())
}

fn trim_end(cells: &[(char, Style)], mut end: usize) -> usize {
    while end > 0 && cells[end - 1].0 == ' ' {
        end -= 1;
    }
    end
}

fn skip_spaces(cells: &[(char, Style)], mut at: usize) -> usize {
    while at < cells.len() && cells[at].0 == ' ' {
        at += 1;
    }
    at
}

fn group(cells: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for &(c, style) in cells {
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    spans
}
