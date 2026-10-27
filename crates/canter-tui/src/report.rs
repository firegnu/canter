//! A report above the input box until Esc or Enter: `/context`, `/files`,
//! `/diff`, `/mcp` and `/skills`. The first line is the title; the body rows scroll
//! with Up and Down when they do not all fit, with how many more there are
//! above and below; the notes and `Esc close` stay.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::diff::Diff;
use crate::text::{LogicalLine, sanitize, str_width, truncate, wrap};
use crate::theme;

const KEYS: &str = " Esc close";

/// Server or file system text, safe to draw on one row.
pub(crate) fn clean(text: &str) -> String {
    sanitize(text).replace('\n', " ")
}

/// A part of the body, laid out for the window's width when drawn.
pub(crate) enum Part {
    /// One row, cut to the width.
    Line(Vec<Span<'static>>),
    /// Words two spaces apart, `indent` columns in, filling rows; a word
    /// longer than a row takes a row of its own, cut.
    Flow { indent: usize, words: Vec<String> },
    /// Text wrapped `indent` columns in, at most `rows` rows, the last ending
    /// with `…` when there is more.
    Wrap {
        indent: usize,
        text: String,
        style: Style,
        rows: usize,
    },
    /// A diff as the conversation draws it, each row after `indent`.
    Diff { indent: &'static str, diff: Diff },
}

impl Part {
    /// A blank row.
    pub fn blank() -> Self {
        Self::Line(Vec::new())
    }

    fn rows(&self, width: usize) -> Vec<Line<'static>> {
        match self {
            Self::Line(spans) => vec![Line::from(truncate(spans.clone(), width))],
            Self::Flow { indent, words } => flow(*indent, words, width),
            Self::Diff { indent, diff } => diff
                .rows(indent, u16::try_from(width).unwrap_or(u16::MAX), None)
                .into_iter()
                .map(|row| row.line)
                .collect(),
            Self::Wrap {
                indent,
                text,
                style,
                rows,
            } => {
                let line = LogicalLine::new(
                    vec![Span::raw(" ".repeat(*indent))],
                    vec![Span::styled(text.clone(), *style)],
                );
                let mut lines = wrap(&line, u16::try_from(width).unwrap_or(u16::MAX));
                if lines.len() > *rows {
                    lines.truncate(*rows);
                    if let Some(last) = lines.last_mut() {
                        let mut spans = last.spans.clone();
                        spans.push(Span::styled("…", *style));
                        *last = Line::from(truncate(spans, width));
                    }
                }
                lines
            }
        }
    }
}

fn flow(indent: usize, words: &[String], width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(indent);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in words {
        let fits = str_width(&row) + 2 + str_width(word) <= room;
        if !row.is_empty() && !fits {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push_str("  ");
        }
        row.push_str(word);
        if str_width(word) > room {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let pad = " ".repeat(indent);
    rows.into_iter()
        .map(|row| Line::from(truncate(vec![Span::raw(format!("{pad}{row}"))], width)))
        .collect()
}

pub(crate) struct Report {
    title: Vec<Span<'static>>,
    body: Vec<Part>,
    notes: Vec<Vec<Span<'static>>>,
    /// The first body row shown.
    top: usize,
    /// The largest `top`, for the room the last frame had.
    last_top: usize,
    /// The body laid out for a width, so a long one is not laid out every frame.
    laid_out: Option<(usize, Vec<Line<'static>>)>,
}

impl Report {
    pub fn new(title: Vec<Span<'static>>, body: Vec<Part>, notes: Vec<Vec<Span<'static>>>) -> Self {
        Self {
            title,
            body,
            notes,
            top: 0,
            last_top: 0,
            laid_out: None,
        }
    }

    pub fn up(&mut self) {
        self.top = self.top.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.top = (self.top + 1).min(self.last_top);
    }

    /// At most `max` rows; nothing when not even one body row fits.
    pub fn rows(&mut self, width: u16, max: usize) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, width));
        let mut rows = vec![cut(self.title.clone()), Line::default()];
        let mut tail = vec![Line::default()];
        tail.extend(self.notes.iter().map(|note| cut(note.clone())));
        tail.push(cut(vec![Span::styled(KEYS, theme::secondary())]));
        let room = max.saturating_sub(rows.len() + tail.len());
        if room == 0 {
            return Vec::new();
        }
        if self.laid_out.as_ref().is_none_or(|(at, _)| *at != width) {
            let body = self.body.iter().flat_map(|part| part.rows(width)).collect();
            self.laid_out = Some((width, body));
        }
        let total = self.laid_out.as_ref().map_or(0, |(_, body)| body.len());
        // With three rows or more, a row each for how many more there are.
        self.last_top = match total.checked_sub(room) {
            None | Some(0) => 0,
            Some(over) if room < 3 => over,
            Some(over) => over + 1,
        };
        self.top = self.top.min(self.last_top);
        let first = self.top;
        let shown = if total <= room || room < 3 {
            room
        } else if first == 0 || first == self.last_top {
            room - 1
        } else {
            room - 2
        };
        let more = |arrow: char, count: usize| {
            Line::from(vec![Span::styled(
                format!("   {arrow} {count} more"),
                theme::secondary(),
            )])
        };
        if first > 0 && room >= 3 {
            rows.push(more('↑', first));
        }
        if let Some((_, body)) = &self.laid_out {
            rows.extend(body.iter().skip(first).take(shown).cloned());
        }
        let below = total.saturating_sub(first + shown);
        if below > 0 && room >= 3 {
            rows.push(more('↓', below));
        }
        rows.extend(tail);
        rows
    }
}
