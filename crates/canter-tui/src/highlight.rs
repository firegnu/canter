//! Syntax colours for code blocks, the command in the approval box and the
//! code in diffs: syntect with two-face's syntax definitions and its `ansi`
//! theme.
//!
//! The `ansi` theme names colours by their number in the terminal's 16-colour
//! palette, and leaves plain text in the terminal's default colour, so code
//! follows the terminal's colour scheme like the rest of the screen.
//!
//! The text in progress is drawn again from the start every frame. Each code
//! block's colours are remembered with the parser's state after its last
//! line, so when the block grows only the new lines are worked out. A diff
//! waiting for approval is drawn every frame too; each side of it is
//! remembered the same way.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::LazyLock;
use std::thread::LocalKey;

use ratatui::style::Style;
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, HighlightState, Theme};
use syntect::parsing::{ParseState, SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

use crate::theme::{self, Depth};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static ANSI: LazyLock<Theme> = LazyLock::new(|| {
    two_face::theme::extra()
        .get(EmbeddedThemeName::Ansi)
        .clone()
});

/// Past any of these, code is drawn plain: colouring it would take too long.
const MAX_BYTES: usize = 512 * 1024;
const MAX_LINES: usize = 10_000;
const MAX_LINE_BYTES: usize = 4 * 1024;

/// How many code blocks keep their colours.
const REMEMBERED: usize = 16;

/// The spans of each of a code block's `lines`, coloured by the language its
/// fence names (`language`, empty when it names none), or by a `#!` first
/// line when it names none. `None` when it should be drawn plain: the
/// language is unknown, the block is too big, or there are no colours.
pub(crate) fn code_block(language: &str, lines: &[&str]) -> Option<Vec<Vec<Span<'static>>>> {
    highlight(find_syntax(language, lines.first()?)?, lines)
}

/// The spans of each of `lines` coloured as Bash, or `None` as for
/// [`code_block`].
pub(crate) fn bash(lines: &[&str]) -> Option<Vec<Vec<Span<'static>>>> {
    highlight(SYNTAXES.find_syntax_by_extension("bash")?, lines)
}

/// The spans of each of `lines` coloured by the language of the file at
/// `path`, found by its name (`Makefile`) or extension; `None` as for
/// [`code_block`]. Only the lines are read, never the file.
pub(crate) fn file(path: &str, lines: &[&str]) -> Option<Vec<Vec<Span<'static>>>> {
    let name = path.rsplit(['/', '\\']).next()?;
    let syntax = SYNTAXES
        .find_syntax_by_extension(name)
        .or_else(|| SYNTAXES.find_syntax_by_extension(name.rsplit_once('.')?.1))?;
    if syntax.name == "Plain Text" {
        return None;
    }
    remembered(&SIDES, syntax, lines, true)
}

/// By the first word of the fence's info string, as a syntax's file
/// extension or name, with a few common aliases. Without one, by a `#!`
/// first line. Never by guessing from the code.
fn find_syntax(language: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let word = language
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or_default();
    let syntax = if word.is_empty() {
        if !first_line.starts_with("#!") {
            return None;
        }
        SYNTAXES.find_syntax_by_first_line(first_line)?
    } else {
        let token = match word.to_ascii_lowercase().as_str() {
            "golang" => "go",
            "shell" => "sh",
            "python3" => "py",
            "csharp" => "cs",
            _ => word,
        };
        SYNTAXES.find_syntax_by_token(token)?
    };
    (syntax.name != "Plain Text").then_some(syntax)
}

fn highlight(syntax: &'static SyntaxReference, lines: &[&str]) -> Option<Vec<Vec<Span<'static>>>> {
    remembered(&DONE, syntax, lines, false)
}

/// `lines` coloured, from a block in `cache` that they continue (or, when
/// `whole`, that has just these lines), or from the start.
fn remembered(
    cache: &'static LocalKey<RefCell<VecDeque<Block>>>,
    syntax: &'static SyntaxReference,
    lines: &[&str],
    whole: bool,
) -> Option<Vec<Vec<Span<'static>>>> {
    if theme::current().depth == Depth::None || too_big(lines) {
        return None;
    }
    cache.with_borrow_mut(|done| {
        let reuse = done
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                block.continues(syntax, lines) && (!whole || block.lines.len() == lines.len())
            })
            .max_by_key(|(_, block)| block.lines.len())
            .map(|(i, _)| i);
        let mut block = match reuse {
            Some(i) => done.remove(i).expect("found above"),
            None => Block::new(syntax),
        };
        block.extend(&lines[block.lines.len()..])?;
        let spans = block.spans();
        done.push_front(block);
        done.truncate(REMEMBERED);
        Some(spans)
    })
}

fn too_big(lines: &[&str]) -> bool {
    lines.len() > MAX_LINES
        || lines.iter().any(|line| line.len() > MAX_LINE_BYTES)
        || lines.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_BYTES
}

thread_local! {
    /// The blocks coloured lately, the latest first.
    static DONE: RefCell<VecDeque<Block>> = const { RefCell::new(VecDeque::new()) };
    /// The sides of diffs coloured lately, the latest first. Apart from the
    /// code blocks, and only ever taken whole: one side of a diff often
    /// starts with the other, and taking it to grow would drop it.
    static SIDES: RefCell<VecDeque<Block>> = const { RefCell::new(VecDeque::new()) };
}

/// A code block coloured so far.
struct Block {
    syntax: &'static str,
    lines: Vec<String>,
    /// Each line's runs of one colour: a palette number (`None` for the
    /// default colour) and the run's bytes in the line.
    runs: Vec<Vec<(Option<u8>, Range<usize>)>>,
    /// Where the parser is after the last line; `None` only while a line is
    /// being worked out.
    state: Option<(HighlightState, ParseState)>,
}

impl Block {
    fn new(syntax: &'static SyntaxReference) -> Self {
        Self {
            syntax: &syntax.name,
            lines: Vec::new(),
            runs: Vec::new(),
            state: Some(HighlightLines::new(syntax, &ANSI).state()),
        }
    }

    /// Whether `lines` are this block's lines and maybe more.
    fn continues(&self, syntax: &SyntaxReference, lines: &[&str]) -> bool {
        self.syntax == syntax.name
            && self.lines.len() <= lines.len()
            && self.lines.iter().zip(lines).all(|(old, new)| old == new)
    }

    /// Colours `more` lines after the ones so far; `None` if syntect fails.
    fn extend(&mut self, more: &[&str]) -> Option<()> {
        let (highlight_state, parse_state) = self.state.take()?;
        let mut lines = HighlightLines::from_state(&ANSI, highlight_state, parse_state);
        for line in more {
            // The syntaxes are the ones that expect each line's newline.
            let text = format!("{line}\n");
            let mut runs: Vec<(Option<u8>, Range<usize>)> = Vec::new();
            let mut at = 0;
            for (style, piece) in lines.highlight_line(&text, &SYNTAXES).ok()? {
                let range = at..(at + piece.len()).min(line.len());
                at += piece.len();
                if range.is_empty() {
                    continue;
                }
                let color = palette(style.foreground);
                match runs.last_mut() {
                    Some((last, run)) if *last == color => run.end = range.end,
                    _ => runs.push((color, range)),
                }
            }
            self.lines.push((*line).to_owned());
            self.runs.push(runs);
        }
        self.state = Some(lines.state());
        Some(())
    }

    fn spans(&self) -> Vec<Vec<Span<'static>>> {
        self.lines
            .iter()
            .zip(&self.runs)
            .map(|(line, runs)| {
                runs.iter()
                    .map(|(color, range)| {
                        Span::styled(line[range.clone()].to_owned(), code_style(*color))
                    })
                    .collect()
            })
            .collect()
    }
}

/// The `ansi` theme writes a palette colour as alpha 0 with the number in
/// red, and the default colour as alpha 1. Anything else (it has none) is
/// taken as the default colour, never as a true colour.
fn palette(color: Color) -> Option<u8> {
    (color.a == 0 && color.r < 16).then_some(color.r)
}

fn code_style(color: Option<u8>) -> Style {
    match color {
        Some(number) => theme::palette(number),
        None => Style::new(),
    }
}
