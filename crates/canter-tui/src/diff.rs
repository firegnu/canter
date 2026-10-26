//! A unified diff as the history and the approval box draw it: one column of
//! line numbers (old ones on removed lines, new ones elsewhere) with the
//! `+` / `-` sign, `⋯` and the function named on the `@@` line between hunks,
//! added and removed lines on a background when the terminal can show one
//! (coloured text otherwise), and the changed words of paired lines a step
//! deeper (reversed without a background). With a background the code is
//! coloured by the file's language and the gutter (number and sign) is a
//! shade deeper than the line.

use canter_protocol::PatchChangeKind;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::highlight;
use crate::text::{LogicalLine, Row, drawn_width, spans_width, truncate, wrap_rows};
use crate::theme;

/// Lines longer than this (in bytes) are not compared word by word: comparing
/// them on every frame could stall the screen. They keep their colour, no
/// words marked.
const WORD_DIFF_MAX: usize = 1024;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Added,
    Removed,
    Context,
    /// Text that is not a diff line: `\ No newline at end of file`, or the
    /// tools' `[truncated N bytes]`.
    Note,
    /// Between two hunks; its text is the function the next one is in, from
    /// its `@@` line, or empty.
    Gap,
}

struct DiffLine {
    kind: Kind,
    /// None where the place in the file is not known.
    number: Option<u32>,
    text: String,
}

pub(crate) struct Diff {
    lines: Vec<DiffLine>,
    /// The changed file, whose name says what language its code is in.
    path: String,
}

/// A piece of a line's text; `true` marks a changed word.
type Words = Vec<(bool, String)>;

impl Diff {
    /// Splits a unified diff into lines. Text without hunks is a whole file
    /// being added (or deleted). Lines after something that is not a diff
    /// line have no number until the next hunk. `path` is the changed file.
    pub(crate) fn parse(text: &str, kind: &PatchChangeKind, path: &str) -> Self {
        let path = path.to_owned();
        if !text.lines().any(|line| line.starts_with("@@")) {
            let kind = match kind {
                PatchChangeKind::Delete => Kind::Removed,
                _ => Kind::Added,
            };
            let lines = text
                .lines()
                .zip(1..)
                .map(|(line, number)| DiffLine {
                    kind,
                    number: Some(number),
                    text: line.to_owned(),
                })
                .collect();
            return Self { lines, path };
        }
        let mut lines = Vec::new();
        let (mut old, mut new) = (None, None);
        let mut in_hunk = false;
        for line in text.lines() {
            if line.starts_with("@@") {
                if in_hunk {
                    lines.push(DiffLine {
                        kind: Kind::Gap,
                        number: None,
                        text: hunk_function(line).to_owned(),
                    });
                }
                in_hunk = true;
                (old, new) = hunk_start(line);
                continue;
            }
            if !in_hunk {
                // File headers (diff --git, index, ---, +++) repeat the card's title.
                continue;
            }
            let (kind, number, text) = if let Some(rest) = line.strip_prefix('+') {
                (Kind::Added, next(&mut new), rest)
            } else if let Some(rest) = line.strip_prefix('-') {
                (Kind::Removed, next(&mut old), rest)
            } else if let Some(rest) = line.strip_prefix(' ').or(line.is_empty().then_some("")) {
                next(&mut old);
                (Kind::Context, next(&mut new), rest)
            } else {
                if !line.starts_with('\\') {
                    (old, new) = (None, None);
                }
                (Kind::Note, None, line)
            };
            lines.push(DiffLine {
                kind,
                number,
                text: text.to_owned(),
            });
        }
        Self { lines, path }
    }

    pub(crate) fn added(&self) -> usize {
        self.count(Kind::Added)
    }

    pub(crate) fn removed(&self) -> usize {
        self.count(Kind::Removed)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn count(&self, kind: Kind) -> usize {
        self.lines.iter().filter(|line| line.kind == kind).count()
    }

    /// Rows `width` columns wide, each starting with `indent`. With a
    /// `limit`, at most that many lines, then `… N more lines · Ctrl+O`.
    pub(crate) fn rows(&self, indent: &str, width: u16, limit: Option<usize>) -> Vec<Row> {
        let all = self.lines.len();
        let mut shown = limit.map_or(all, |limit| limit.min(all));
        if shown < all && shown > 0 && self.lines[shown - 1].kind == Kind::Gap {
            shown -= 1;
        }
        let hidden = self.lines[shown..]
            .iter()
            .filter(|line| line.kind != Kind::Gap)
            .count();
        let digits = self
            .lines
            .iter()
            .filter_map(|line| line.number)
            .max()
            .map_or(0, |max| max.to_string().len());

        let mut words: Vec<Option<Words>> = vec![None; shown];
        for (removed, added) in self.pairs() {
            if removed >= shown {
                break;
            }
            let (old, new) = (&self.lines[removed].text, &self.lines[added].text);
            if old.len() > WORD_DIFF_MAX || new.len() > WORD_DIFF_MAX {
                continue;
            }
            let (old, new) = changed_words(old, new);
            words[removed] = Some(old);
            if added < shown {
                words[added] = Some(new);
            }
        }

        let colours = self.colours(shown);
        let mut rows = Vec::new();
        for ((line, words), colours) in self.lines[..shown].iter().zip(words).zip(colours) {
            rows.extend(line_rows(line, words, colours, indent, digits, width));
        }
        if hidden > 0 {
            rows.push(Row::marks(Line::from(vec![
                Span::raw(indent.to_owned()),
                Span::styled(
                    format!("{:>digits$} {hidden} more lines · Ctrl+O", "…"),
                    theme::secondary(),
                ),
            ])));
        }
        rows
    }

    /// The code of the first `shown` lines coloured by the file's language,
    /// only when lines have backgrounds. Each side of the change is coloured
    /// on its own, so the parser reads the file as it was (context and
    /// removed lines) and as it is (context and added lines). All `None` when
    /// either side cannot be coloured. Colours are remembered as code
    /// blocks' are ([`highlight`]), so drawing the same diff again does not
    /// colour it again.
    fn colours(&self, shown: usize) -> Vec<Option<Vec<Span<'static>>>> {
        let mut colours = vec![None; shown];
        if theme::diff_added_bg().is_none() {
            return colours;
        }
        for side in [Kind::Removed, Kind::Added] {
            let at: Vec<usize> = (0..shown)
                .filter(|&i| self.lines[i].kind == Kind::Context || self.lines[i].kind == side)
                .collect();
            if at.is_empty() {
                continue;
            }
            let texts: Vec<&str> = at.iter().map(|&i| self.lines[i].text.as_str()).collect();
            let Some(spans) = highlight::file(&self.path, &texts) else {
                return vec![None; shown];
            };
            for (i, spans) in at.into_iter().zip(spans) {
                colours[i] = Some(spans);
            }
        }
        colours
    }

    /// Removed and added lines to compare word by word: in a run of removed
    /// lines followed by as many added lines, each with its counterpart.
    fn pairs(&self) -> Vec<(usize, usize)> {
        let run = |from: usize, kind: Kind| {
            self.lines[from..]
                .iter()
                .take_while(|line| line.kind == kind)
                .count()
        };
        let mut pairs = Vec::new();
        let mut at = 0;
        while at < self.lines.len() {
            let removed = run(at, Kind::Removed);
            if removed == 0 {
                at += 1;
                continue;
            }
            let added = run(at + removed, Kind::Added);
            if added == removed {
                pairs.extend((0..removed).map(|i| (at + i, at + removed + i)));
            }
            at += removed + added;
        }
        pairs
    }
}

/// The first old and new line numbers of `@@ -a,b +c,d @@`.
fn hunk_start(header: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = header.split_whitespace().skip(1);
    let mut start = |sign: char| {
        parts
            .next()?
            .strip_prefix(sign)?
            .split(',')
            .next()?
            .parse()
            .ok()
    };
    let old = start('-');
    (old, start('+'))
}

/// What follows `@@ -a,b +c,d @@`: the function the hunk is in, or empty.
fn hunk_function(header: &str) -> &str {
    header
        .strip_prefix("@@")
        .and_then(|rest| rest.split_once("@@"))
        .map_or("", |(_, name)| name.trim())
}

/// The number for this line, moving on to the next.
fn next(number: &mut Option<u32>) -> Option<u32> {
    let current = *number;
    *number = current.map(|n| n + 1);
    current
}

/// The two lines in pieces with their changed words marked. Nothing is
/// marked when less than half of them is the same (similar's `inline`).
fn changed_words(old: &str, new: &str) -> (Words, Words) {
    let diff = TextDiff::from_lines(old, new);
    let (mut old_words, mut new_words) = (Vec::new(), Vec::new());
    for op in diff.ops() {
        for change in diff.iter_inline_changes(op) {
            let words = match change.tag() {
                ChangeTag::Delete => &mut old_words,
                ChangeTag::Insert => &mut new_words,
                ChangeTag::Equal => continue,
            };
            for (changed, text) in change.iter_strings_lossy() {
                words.push((changed, text.into_owned()));
            }
        }
    }
    if old_words.is_empty() || new_words.is_empty() {
        return (unmarked(old), unmarked(new));
    }
    (old_words, new_words)
}

fn unmarked(text: &str) -> Words {
    vec![(false, text.to_owned())]
}

/// One line: `number sign text`, wrapped under the text. A line with a
/// background has it from the number to the right edge, the gutter (number
/// and sign) a shade deeper, and its number coloured like its sign. Its
/// code is in `colours` when coloured by its language.
fn line_rows(
    line: &DiffLine,
    words: Option<Words>,
    colours: Option<Vec<Span<'static>>>,
    indent: &str,
    digits: usize,
    width: u16,
) -> Vec<Row> {
    let (marker, marker_style, text_style, word_style, background, gutter_bg) = match line.kind {
        Kind::Gap => return vec![gap_row(&line.text, indent, digits, width)],
        Kind::Added => (
            "+",
            theme::diff_added(),
            theme::diff_added(),
            theme::diff_added_word(),
            theme::diff_added_bg(),
            theme::diff_added_gutter_bg(),
        ),
        Kind::Removed => (
            "-",
            theme::diff_removed(),
            theme::diff_removed(),
            theme::diff_removed_word(),
            theme::diff_removed_bg(),
            theme::diff_removed_gutter_bg(),
        ),
        Kind::Context => (
            " ",
            theme::secondary(),
            theme::secondary(),
            Style::new(),
            None,
            None,
        ),
        Kind::Note => (
            "",
            Style::new(),
            theme::secondary(),
            Style::new(),
            None,
            None,
        ),
    };
    // On a background the text is the default colour where it has no other.
    let text_style = if background.is_some() {
        Style::new()
    } else {
        text_style
    };
    let paint = |style: Style| background.map_or(style, |color| style.bg(color));
    let (gutter, number_style) = match gutter_bg {
        Some(color) => (Style::new().bg(color), marker_style),
        None => (Style::new(), theme::secondary()),
    };
    let number = match line.number {
        Some(number) => format!("{number:>digits$}"),
        None => " ".repeat(digits),
    };
    let space = if digits == 0 { "" } else { " " };
    let mut first = vec![
        Span::raw(indent.to_owned()),
        Span::styled(format!("{number}{space}"), gutter.patch(number_style)),
    ];
    let body = if line.kind == Kind::Note {
        vec![Span::styled(line.text.clone(), text_style)]
    } else {
        first.push(Span::styled(
            format!("{marker} "),
            gutter.patch(marker_style),
        ));
        let words = words.unwrap_or_else(|| unmarked(&line.text));
        let pieces = match colours {
            Some(colours) => mark_words(colours, &words, line.kind),
            None => words
                .into_iter()
                .map(|(changed, text)| (text_style, changed, text))
                .collect(),
        };
        pieces
            .into_iter()
            .map(|(style, changed, text)| {
                let style = paint(style);
                let style = if changed {
                    style.patch(word_style)
                } else {
                    style
                };
                Span::styled(text, style)
            })
            .collect()
    };
    let hang_width = spans_width(&first) - indent.len();
    let hang = vec![
        Span::raw(indent.to_owned()),
        Span::styled(" ".repeat(hang_width), gutter),
    ];
    let mut rows = wrap_rows(
        &LogicalLine {
            indent: first,
            hang,
            body,
            marker: 0,
            links: Vec::new(),
        },
        width,
    );
    // Notes are not the file's text.
    if line.kind == Kind::Note {
        for row in &mut rows {
            row.text = None;
        }
    }
    if background.is_some() {
        for row in &mut rows {
            let filled = spans_width(&row.line.spans);
            // The background after the text is not text: it starts where the
            // text ends as drawn.
            if let Some(text) = &mut row.text {
                text.end = drawn_width(&row.line.spans);
            }
            let gap = usize::from(width).saturating_sub(filled);
            row.line
                .spans
                .push(Span::styled(" ".repeat(gap), paint(Style::new())));
        }
    }
    rows
}

/// Between two hunks: `⋯` under the signs, then the function the next hunk
/// is in, grey and italic, cut to the row. Not the file's text.
fn gap_row(function: &str, indent: &str, digits: usize, width: u16) -> Row {
    let lead = if digits == 0 {
        String::new()
    } else {
        " ".repeat(digits + 1)
    };
    let mut spans = vec![
        Span::raw(format!("{indent}{lead}")),
        Span::styled("⋯", theme::secondary()),
    ];
    if !function.is_empty() {
        spans.push(Span::styled(
            format!(" {function}"),
            theme::secondary().italic(),
        ));
    }
    Row::marks(Line::from(truncate(spans, usize::from(width))))
}

/// A line's code in its syntax colours, cut again where its changed words
/// start and end: each piece's style, whether it is a changed word, and its
/// text. Green on an added line and red on a removed one would not show on
/// its background, so those pieces are in the default colour.
fn mark_words(
    colours: Vec<Span<'static>>,
    words: &Words,
    kind: Kind,
) -> Vec<(Style, bool, String)> {
    let mut changed = Vec::new();
    let mut at = 0;
    for (marked, text) in words {
        if *marked {
            changed.push(at..at + text.len());
        }
        at += text.len();
    }
    let hidden: &[Color] = match kind {
        Kind::Added => &[Color::Green, Color::LightGreen],
        Kind::Removed => &[Color::Red, Color::LightRed],
        _ => &[],
    };
    let mut pieces = Vec::new();
    let mut at = 0;
    for span in colours {
        let mut style = span.style;
        if style.fg.is_some_and(|color| hidden.contains(&color)) {
            style.fg = None;
        }
        let text = span.content;
        let end = at + text.len();
        let mut cuts: Vec<usize> = changed
            .iter()
            .flat_map(|word| [word.start, word.end])
            .filter(|&cut| cut > at && cut < end && text.is_char_boundary(cut - at))
            .collect();
        cuts.push(end);
        cuts.sort_unstable();
        cuts.dedup();
        let mut from = at;
        for cut in cuts {
            let marked = changed.iter().any(|word| word.contains(&from));
            pieces.push((style, marked, text[from - at..cut - at].to_owned()));
            from = cut;
        }
        at = end;
    }
    pieces
}
