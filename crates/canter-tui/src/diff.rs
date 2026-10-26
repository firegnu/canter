//! A unified diff as the history and the approval box draw it: one column of
//! line numbers (old ones on removed lines, new ones elsewhere), `⋮` between
//! hunks, added and removed lines on a background when the terminal can show
//! one (coloured text otherwise), and the changed words of paired lines a step
//! deeper (reversed without a background).

use canter_protocol::PatchChangeKind;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::text::{LogicalLine, spans_width, wrap};
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
    /// Between two hunks.
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
}

/// A piece of a line's text; `true` marks a changed word.
type Words = Vec<(bool, String)>;

impl Diff {
    /// Splits a unified diff into lines. Text without hunks is a whole file
    /// being added (or deleted). Lines after something that is not a diff
    /// line have no number until the next hunk.
    pub(crate) fn parse(text: &str, kind: &PatchChangeKind) -> Self {
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
            return Self { lines };
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
                        text: String::new(),
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
        Self { lines }
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
    pub(crate) fn rows(
        &self,
        indent: &str,
        width: u16,
        limit: Option<usize>,
    ) -> Vec<Line<'static>> {
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

        let mut rows = Vec::new();
        for (line, words) in self.lines[..shown].iter().zip(words) {
            rows.extend(line_rows(line, words, indent, digits, width));
        }
        if hidden > 0 {
            rows.push(Line::from(vec![
                Span::raw(indent.to_owned()),
                Span::styled(
                    format!("{:>digits$} {hidden} more lines · Ctrl+O", "…"),
                    theme::secondary(),
                ),
            ]));
        }
        rows
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

/// One line: `number marker text`, wrapped under the text. A line with a
/// background has it from the number to the right edge.
fn line_rows(
    line: &DiffLine,
    words: Option<Words>,
    indent: &str,
    digits: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let (marker, marker_style, text_style, word_style, background) = match line.kind {
        Kind::Added => (
            "+",
            theme::diff_added(),
            theme::diff_added(),
            theme::diff_added_word(),
            theme::diff_added_bg(),
        ),
        Kind::Removed => (
            "-",
            theme::diff_removed(),
            theme::diff_removed(),
            theme::diff_removed_word(),
            theme::diff_removed_bg(),
        ),
        Kind::Context => (
            " ",
            theme::secondary(),
            theme::secondary(),
            Style::new(),
            None,
        ),
        Kind::Note | Kind::Gap => ("", Style::new(), theme::secondary(), Style::new(), None),
    };
    // On a background the text is the default colour; only the marker is coloured.
    let text_style = if background.is_some() {
        Style::new()
    } else {
        text_style
    };
    let paint = |style: Style| background.map_or(style, |color| style.bg(color));
    let number = match line.number {
        Some(number) => format!("{number:>digits$}"),
        None => " ".repeat(digits),
    };
    let gutter = if digits == 0 { "" } else { " " };
    let mut first = vec![
        Span::raw(indent.to_owned()),
        Span::styled(format!("{number}{gutter}"), paint(theme::secondary())),
    ];
    let body = match line.kind {
        Kind::Gap => vec![Span::styled("⋮", theme::secondary())],
        Kind::Note => vec![Span::styled(line.text.clone(), text_style)],
        _ => {
            first.push(Span::styled(format!("{marker} "), paint(marker_style)));
            words
                .unwrap_or_else(|| unmarked(&line.text))
                .into_iter()
                .map(|(changed, text)| {
                    let style = paint(text_style);
                    let style = if changed {
                        style.patch(word_style)
                    } else {
                        style
                    };
                    Span::styled(text, style)
                })
                .collect()
        }
    };
    let hang_width = spans_width(&first) - indent.len();
    let hang = vec![
        Span::raw(indent.to_owned()),
        Span::styled(" ".repeat(hang_width), paint(Style::new())),
    ];
    let mut rows = wrap(
        &LogicalLine {
            indent: first,
            hang,
            body,
        },
        width,
    );
    if background.is_some() {
        for row in &mut rows {
            let gap = usize::from(width).saturating_sub(spans_width(&row.spans));
            row.spans
                .push(Span::styled(" ".repeat(gap), paint(Style::new())));
        }
    }
    rows
}
