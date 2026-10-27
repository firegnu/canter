//! Syntax colours for code blocks, the command in the approval box and the
//! code in diffs: syntect with two-face's syntax definitions, in the theme
//! `/theme` chose (see [`CodeTheme`]).
//!
//! The default, `terminal`, is two-face's `ansi` theme: it names colours by
//! their number in the terminal's 16-colour palette, and leaves plain text in
//! the terminal's default colour, so code follows the terminal's colour
//! scheme like the rest of the screen. The other themes give true colours,
//! drawn as they are or as the nearest of the 256; only their text colours
//! are used, never their backgrounds or bold. With 16 colours only
//! `terminal` applies.
//!
//! The text in progress is drawn again from the start every frame. Each code
//! block's colours are remembered with the parser's state after its last
//! line, so when the block grows only the new lines are worked out. A diff
//! waiting for approval is drawn every frame too; each side of it is
//! remembered the same way. What is remembered is for one theme: another
//! theme works the colours out again.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::{Deref, Range};
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::thread::LocalKey;

use ratatui::style::Style;
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, HighlightState, Theme, ThemeSet};
use syntect::parsing::{ParseState, SyntaxReference, SyntaxSet};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

use crate::theme::{self, Background, Depth};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
/// Each theme is read from two-face's data the first time it is drawn.
static THEMES: LazyLock<EmbeddedLazyThemeSet> = LazyLock::new(two_face::theme::extra);

/// The themes two-face comes with, by the names Codex `rust-v0.163.0` gives
/// them (`tui/src/render/highlight.rs`), in its order.
const BUILT_IN: [(&str, EmbeddedThemeName); 32] = [
    ("1337", EmbeddedThemeName::Leet),
    ("ansi", EmbeddedThemeName::Ansi),
    ("base16", EmbeddedThemeName::Base16),
    ("base16-256", EmbeddedThemeName::Base16_256),
    (
        "base16-eighties-dark",
        EmbeddedThemeName::Base16EightiesDark,
    ),
    ("base16-mocha-dark", EmbeddedThemeName::Base16MochaDark),
    ("base16-ocean-dark", EmbeddedThemeName::Base16OceanDark),
    ("base16-ocean-light", EmbeddedThemeName::Base16OceanLight),
    ("catppuccin-frappe", EmbeddedThemeName::CatppuccinFrappe),
    ("catppuccin-latte", EmbeddedThemeName::CatppuccinLatte),
    (
        "catppuccin-macchiato",
        EmbeddedThemeName::CatppuccinMacchiato,
    ),
    ("catppuccin-mocha", EmbeddedThemeName::CatppuccinMocha),
    ("coldark-cold", EmbeddedThemeName::ColdarkCold),
    ("coldark-dark", EmbeddedThemeName::ColdarkDark),
    ("dark-neon", EmbeddedThemeName::DarkNeon),
    ("dracula", EmbeddedThemeName::Dracula),
    ("github", EmbeddedThemeName::Github),
    ("gruvbox-dark", EmbeddedThemeName::GruvboxDark),
    ("gruvbox-light", EmbeddedThemeName::GruvboxLight),
    ("inspired-github", EmbeddedThemeName::InspiredGithub),
    ("monokai-extended", EmbeddedThemeName::MonokaiExtended),
    (
        "monokai-extended-bright",
        EmbeddedThemeName::MonokaiExtendedBright,
    ),
    (
        "monokai-extended-light",
        EmbeddedThemeName::MonokaiExtendedLight,
    ),
    (
        "monokai-extended-origin",
        EmbeddedThemeName::MonokaiExtendedOrigin,
    ),
    ("nord", EmbeddedThemeName::Nord),
    ("one-half-dark", EmbeddedThemeName::OneHalfDark),
    ("one-half-light", EmbeddedThemeName::OneHalfLight),
    ("solarized-dark", EmbeddedThemeName::SolarizedDark),
    ("solarized-light", EmbeddedThemeName::SolarizedLight),
    ("sublime-snazzy", EmbeddedThemeName::SublimeSnazzy),
    ("two-dark", EmbeddedThemeName::TwoDark),
    ("zenburn", EmbeddedThemeName::Zenburn),
];

/// The theme code is drawn in, as `/theme` chose it.
#[derive(Clone, Debug, Default)]
pub(crate) enum CodeTheme {
    /// The terminal's own 16 colours: two-face's `ansi`.
    #[default]
    Terminal,
    /// `catppuccin-latte` on a light terminal, else `catppuccin-mocha`.
    Auto,
    BuiltIn(EmbeddedThemeName),
    /// A `.tmTheme` file, numbered by when it was read.
    File(u64, Arc<Theme>),
}

thread_local! {
    static CODE: RefCell<CodeTheme> = const { RefCell::new(CodeTheme::Terminal) };
    static FILES_READ: Cell<u64> = const { Cell::new(0) };
    /// How many times the theme was set: what is drawn from code before
    /// a change is drawn again.
    static SETS: Cell<u64> = const { Cell::new(0) };
}

/// Draws code on this thread in `theme` from now on.
pub(crate) fn set_code_theme(theme: CodeTheme) {
    CODE.set(theme);
    SETS.set(SETS.get() + 1);
}

pub(crate) fn code_theme() -> CodeTheme {
    CODE.with_borrow(Clone::clone)
}

/// Changes each time the theme is set.
pub(crate) fn code_revision() -> u64 {
    SETS.get()
}

/// What `auto` stands for on this terminal.
pub(crate) fn auto_name() -> &'static str {
    match theme::current().background {
        Background::Light => "catppuccin-latte",
        _ => "catppuccin-mocha",
    }
}

/// Whether themes other than `terminal` can be drawn: 256 colours or more.
pub(crate) fn themes_apply() -> bool {
    matches!(theme::current().depth, Depth::Ansi256 | Depth::TrueColor)
}

/// The theme named `name`: `terminal`, `auto`, a built-in one, or else the
/// file `<name>.tmTheme` in `dir`. `None` when there is none, or the file
/// cannot be read.
pub(crate) fn find(name: &str, dir: Option<&Path>) -> Option<CodeTheme> {
    match name {
        "terminal" => return Some(CodeTheme::Terminal),
        "auto" => return Some(CodeTheme::Auto),
        _ => {}
    }
    if let Some((_, built_in)) = BUILT_IN.iter().find(|(known, _)| *known == name) {
        return Some(CodeTheme::BuiltIn(*built_in));
    }
    // Only a file in the directory, never a path elsewhere.
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
        return None;
    }
    read_file(&dir?.join(format!("{name}.tmTheme")))
}

fn read_file(path: &Path) -> Option<CodeTheme> {
    let theme = ThemeSet::get_theme(path).ok()?;
    let number = FILES_READ.get() + 1;
    FILES_READ.set(number);
    Some(CodeTheme::File(number, Arc::new(theme)))
}

/// A row of `/theme`'s list.
pub(crate) struct Choice {
    pub name: String,
    /// From a file in the themes directory.
    pub custom: bool,
    /// `None` for a file that cannot be read.
    pub theme: Option<CodeTheme>,
}

/// `terminal`, `auto`, the built-in themes, then the `.tmTheme` files in
/// `dir` by name; a file named like one above is left out.
pub(crate) fn choices(dir: Option<&Path>) -> Vec<Choice> {
    let fixed = |name: &str, theme| Choice {
        name: name.to_owned(),
        custom: false,
        theme: Some(theme),
    };
    let mut list = vec![
        fixed("terminal", CodeTheme::Terminal),
        fixed("auto", CodeTheme::Auto),
    ];
    list.extend(
        BUILT_IN
            .iter()
            .map(|(name, built_in)| fixed(name, CodeTheme::BuiltIn(*built_in))),
    );
    let mut files: Vec<(String, std::path::PathBuf)> = dir
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "tmTheme" {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_owned();
            (!name.is_empty() && !name.starts_with('.')).then_some((name, path))
        })
        .filter(|(name, _)| !list.iter().any(|choice| &choice.name == name))
        .collect();
    files.sort_by_cached_key(|(name, _)| (name.to_lowercase(), name.clone()));
    list.extend(files.into_iter().map(|(name, path)| Choice {
        name,
        custom: true,
        theme: read_file(&path),
    }));
    list
}

/// The theme drawn now: the one chosen, or `ansi` with fewer than 256
/// colours.
enum Drawn {
    BuiltIn(EmbeddedThemeName),
    File(u64, Arc<Theme>),
}

impl Drawn {
    fn now() -> Self {
        if !themes_apply() {
            return Self::BuiltIn(EmbeddedThemeName::Ansi);
        }
        match code_theme() {
            CodeTheme::Terminal => Self::BuiltIn(EmbeddedThemeName::Ansi),
            CodeTheme::Auto => Self::BuiltIn(match theme::current().background {
                Background::Light => EmbeddedThemeName::CatppuccinLatte,
                _ => EmbeddedThemeName::CatppuccinMocha,
            }),
            CodeTheme::BuiltIn(name) => Self::BuiltIn(name),
            CodeTheme::File(number, theme) => Self::File(number, theme),
        }
    }

    /// What tells it apart in the caches.
    fn key(&self) -> Key {
        match self {
            Self::BuiltIn(name) => Key::BuiltIn(*name),
            Self::File(number, _) => Key::File(*number),
        }
    }
}

impl Deref for Drawn {
    type Target = Theme;

    fn deref(&self) -> &Theme {
        match self {
            Self::BuiltIn(name) => THEMES.get(*name),
            Self::File(_, theme) => theme,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    BuiltIn(EmbeddedThemeName),
    File(u64),
}

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
    let drawn = Drawn::now();
    let key = drawn.key();
    cache.with_borrow_mut(|done| {
        let reuse = done
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                block.continues(key, syntax, lines) && (!whole || block.lines.len() == lines.len())
            })
            .max_by_key(|(_, block)| block.lines.len())
            .map(|(i, _)| i);
        let mut block = match reuse {
            Some(i) => done.remove(i).expect("found above"),
            None => Block::new(key, &drawn, syntax),
        };
        block.extend(&drawn, &lines[block.lines.len()..])?;
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
    theme: Key,
    syntax: &'static str,
    lines: Vec<String>,
    /// Each line's runs of one colour, as the theme gives it, and the run's
    /// bytes in the line.
    runs: Vec<Vec<(Color, Range<usize>)>>,
    /// Where the parser is after the last line; `None` only while a line is
    /// being worked out.
    state: Option<(HighlightState, ParseState)>,
}

impl Block {
    fn new(key: Key, theme: &Theme, syntax: &'static SyntaxReference) -> Self {
        Self {
            theme: key,
            syntax: &syntax.name,
            lines: Vec::new(),
            runs: Vec::new(),
            state: Some(HighlightLines::new(syntax, theme).state()),
        }
    }

    /// Whether `lines` are this block's lines and maybe more, in the theme
    /// `key`.
    fn continues(&self, key: Key, syntax: &SyntaxReference, lines: &[&str]) -> bool {
        self.theme == key
            && self.syntax == syntax.name
            && self.lines.len() <= lines.len()
            && self.lines.iter().zip(lines).all(|(old, new)| old == new)
    }

    /// Colours `more` lines after the ones so far; `None` if syntect fails.
    fn extend(&mut self, theme: &Theme, more: &[&str]) -> Option<()> {
        let (highlight_state, parse_state) = self.state.take()?;
        let mut lines = HighlightLines::from_state(theme, highlight_state, parse_state);
        for line in more {
            // The syntaxes are the ones that expect each line's newline.
            let text = format!("{line}\n");
            let mut runs: Vec<(Color, Range<usize>)> = Vec::new();
            let mut at = 0;
            for (style, piece) in lines.highlight_line(&text, &SYNTAXES).ok()? {
                let range = at..(at + piece.len()).min(line.len());
                at += piece.len();
                if range.is_empty() {
                    continue;
                }
                let color = style.foreground;
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

/// A colour as a theme gives it. The `ansi` and `base16` themes write a
/// palette colour as alpha 0 with the number in red, and the default colour
/// as alpha 1; the others give true colours.
fn code_style(color: Color) -> Style {
    match color.a {
        0 if color.r < 16 => theme::palette(color.r),
        0 => theme::code_indexed(color.r),
        1 => Style::new(),
        _ => theme::code_rgb([color.r, color.g, color.b]),
    }
}
