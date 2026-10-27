//! The welcome header at the top of the conversation: a pixel pony, three
//! columns, then four rows of text: `canter` and the version line, the
//! session's directory, and two rows of common keys. Only this terminal
//! draws it: it is not in the session, and the model never sees it. A copy
//! takes the text, not the pony ([`Row::text`]); the search skips the pony
//! too. When the pony and the title do not both fit, the text shows alone.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::approval::shorten_home;
use crate::short_path::fit_path;
use crate::text::{Row, sanitize, spans_width, str_width, truncate};
use crate::theme::{self, Pony};

/// 16 × 8 pixels; a cell shows two, one above the other, so the pony takes
/// 16 columns and 4 rows. `.` is empty; see [`part`] for the letters.
const PIXELS: [&str; 8] = [
    "..........b..b..",
    "........mmbbbbb.",
    ".......mmbwebbbb",
    ".......mbbeebbbb",
    ".mm....mbbbbbpcc",
    "m..bbbbbbbbbbcc.",
    "...bbbbbbbbb....",
    "...m.m...m.m....",
];
const PONY_WIDTH: usize = 16;
/// Columns between the pony and the text.
const GAP: usize = 3;
/// The keys on the last two rows; as many as fit, from the first.
const KEYS: [&[&str]; 2] = [
    &[
        "? keys",
        "/help commands",
        "@ files",
        "Ctrl+F find",
        "Ctrl+O more",
    ],
    &["Shift+Tab permissions", "Esc interrupt", "Ctrl+C quit"],
];
const SEPARATOR: &str = " · ";

/// The header's rows for a window `width` wide: `version` after `canter`,
/// then `cwd`, with `home` as `~`.
pub(crate) fn rows(version: &str, cwd: &str, home: Option<&str>, width: u16) -> Vec<Row> {
    let width = usize::from(width);
    let title = vec![
        Span::styled("canter", Style::new().bold()),
        Span::raw(format!(" {}", sanitize(version).replace('\n', " "))),
    ];
    let left = PONY_WIDTH + GAP;
    let pony = width >= left + spans_width(&title);
    let left = if pony { left } else { 0 };
    let room = width - left;
    let dim = theme::secondary();
    let cwd = shorten_home(&sanitize(cwd).replace('\n', " "), home);
    let texts = [
        truncate(title, room),
        vec![Span::styled(fit_path(&cwd, room), dim)],
        keys(KEYS[0], room, dim),
        keys(KEYS[1], room, dim),
    ];
    texts
        .into_iter()
        .enumerate()
        .map(|(y, text)| {
            let mut spans = if pony { pony_row(y) } else { Vec::new() };
            spans.extend(text);
            Row::text_from(Line::from(spans), left)
        })
        .collect()
}

/// What the letters of [`PIXELS`] are; `None` for `.`.
fn part(pixel: u8) -> Option<Pony> {
    Some(match pixel {
        b'b' => Pony::Body,
        b'm' => Pony::Mane,
        b'e' => Pony::Eye,
        b'w' => Pony::Shine,
        b'c' => Pony::Nose,
        b'p' => Pony::Blush,
        _ => return None,
    })
}

/// Row `y` of the pony and the gap after it. A cell's upper pixel is drawn
/// as `▀` in its colour, the lower one as `▄`, both as `█` when they are
/// drawn alike; when they differ, the lower one is the background of `▀`.
fn pony_row(y: usize) -> Vec<Span<'static>> {
    let paint = |row: usize, x: usize| part(PIXELS[row].as_bytes()[x]).and_then(theme::pony);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for x in 0..PONY_WIDTH {
        let (symbol, style) = match (paint(2 * y, x), paint(2 * y + 1, x)) {
            (None, None) => (" ", Style::new()),
            (Some(top), None) => ("▀", top),
            (None, Some(bottom)) => ("▄", bottom),
            (Some(top), Some(bottom)) if top == bottom => ("█", top),
            (Some(top), Some(bottom)) => ("▀", bottom.fg.map_or(top, |color| top.bg(color))),
        };
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push_str(symbol),
            _ => spans.push(Span::styled(symbol, style)),
        }
    }
    spans.push(Span::raw(" ".repeat(GAP)));
    spans
}

/// `keys` joined by ` · `, as many as fit in `room` from the first; the
/// first cut at the edge when not even it fits.
fn keys(keys: &[&str], room: usize, style: Style) -> Vec<Span<'static>> {
    let mut text = String::new();
    for key in keys {
        let next = if text.is_empty() {
            (*key).to_owned()
        } else {
            format!("{text}{SEPARATOR}{key}")
        };
        if !text.is_empty() && str_width(&next) > room {
            break;
        }
        text = next;
    }
    truncate(vec![Span::styled(text, style)], room)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_do_not_fit_are_left_out_from_the_last() {
        let text = |room| {
            keys(KEYS[0], room, Style::new())
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(
            text(61),
            "? keys · /help commands · @ files · Ctrl+F find · Ctrl+O more"
        );
        assert_eq!(text(60), "? keys · /help commands · @ files · Ctrl+F find");
        assert_eq!(text(46), "? keys · /help commands · @ files");
        assert_eq!(text(32), "? keys · /help commands");
        assert_eq!(text(22), "? keys");
        assert_eq!(text(5), "? ke…");
    }
}
