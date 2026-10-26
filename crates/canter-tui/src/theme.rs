//! Colours by purpose. Screens ask for a purpose (accent, failure, warning…)
//! and never name a colour themselves.
//!
//! Text uses the terminal's own 16 colours, so it follows the terminal's
//! colour scheme. Only the diff backgrounds are true colours, and only when
//! the terminal can show them and its background is known to be dark or light.

use std::cell::Cell;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};

/// How many colours the terminal shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Depth {
    /// `NO_COLOR` is set: bold, italic, underline and reverse only.
    None,
    #[default]
    Ansi16,
    Ansi256,
    TrueColor,
}

/// The terminal's background.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Background {
    Dark,
    Light,
    #[default]
    Unknown,
}

/// What the screen is drawn for. The default (16 colours, background
/// unknown) is what tests get unless they set another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Theme {
    pub depth: Depth,
    pub background: Background,
}

/// The longest wait for the terminal to say what its background is.
const ASK_TIMEOUT: Duration = Duration::from_millis(200);

impl Theme {
    /// From the environment and the terminal itself. Call it once the screen
    /// is taken over and before anything else reads the terminal's input:
    /// the answer comes in on the same input.
    pub fn detect() -> Self {
        Self::from_env(|name| std::env::var(name).ok(), ask_terminal)
    }

    /// From environment variables (`var`) and, unless `CANTER_THEME` says,
    /// the terminal's answer about its background (`ask`).
    ///
    /// Depth: `NO_COLOR` not empty means no colours; `COLORTERM` `truecolor`
    /// or `24bit` true colour; `TERM` containing `256color` 256 colours;
    /// otherwise 16. Background: `CANTER_THEME` `dark` or `light`, else the
    /// terminal's answer, else the last number in `COLORFGBG`, else unknown.
    pub fn from_env(
        var: impl Fn(&str) -> Option<String>,
        ask: impl FnOnce() -> Option<Background>,
    ) -> Self {
        let depth = if var("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            Depth::None
        } else if var("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit") {
            Depth::TrueColor
        } else if var("TERM").is_some_and(|value| value.contains("256color")) {
            Depth::Ansi256
        } else {
            Depth::Ansi16
        };
        let background = match var("CANTER_THEME").as_deref() {
            Some("dark") => Background::Dark,
            Some("light") => Background::Light,
            _ => ask()
                .or_else(|| var("COLORFGBG").and_then(|value| colorfgbg(&value)))
                .unwrap_or(Background::Unknown),
        };
        Self { depth, background }
    }

    /// `color` as the text colour, or nothing without colours.
    fn fg(self, color: Color) -> Style {
        match self.depth {
            Depth::None => Style::new(),
            _ => Style::new().fg(color),
        }
    }

    /// One of two backgrounds for a dark or light terminal; none with 16
    /// colours or less, or when the background is unknown.
    fn bg(self, dark: [u8; 3], light: [u8; 3]) -> Option<Color> {
        let [r, g, b] = match self.background {
            Background::Dark => dark,
            Background::Light => light,
            Background::Unknown => return None,
        };
        match self.depth {
            Depth::TrueColor => Some(Color::Rgb(r, g, b)),
            Depth::Ansi256 => Some(Color::Indexed(nearest_256([r, g, b]))),
            Depth::Ansi16 | Depth::None => None,
        }
    }
}

/// The terminal's answer to the background colour query (OSC 11).
fn ask_terminal() -> Option<Background> {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = ASK_TIMEOUT;
    let color = terminal_colorsaurus::background_color(options).ok()?;
    Some(if color.perceived_lightness() > 0.5 {
        Background::Light
    } else {
        Background::Dark
    })
}

/// `COLORFGBG` is `fg;bg` (sometimes `fg;default;bg`) in the 16-colour
/// numbering: black to light grey (0–6) and dark grey (8) are dark.
fn colorfgbg(value: &str) -> Option<Background> {
    match value.rsplit(';').next()?.parse::<u8>().ok()? {
        0..=6 | 8 => Some(Background::Dark),
        7 | 9..=15 => Some(Background::Light),
        _ => None,
    }
}

/// The closest colour in the 256-colour table, past the first 16 (those
/// change with the terminal's settings).
fn nearest_256(rgb: [u8; 3]) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let color = |index: u8| -> [u8; 3] {
        if index >= 232 {
            let gray = 8 + 10 * (index - 232);
            [gray; 3]
        } else {
            let cube = usize::from(index - 16);
            [LEVELS[cube / 36], LEVELS[cube / 6 % 6], LEVELS[cube % 6]]
        }
    };
    let distance = |other: [u8; 3]| -> u32 {
        rgb.iter()
            .zip(other)
            .map(|(&a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum()
    };
    (16..=255)
        .min_by_key(|&index| distance(color(index)))
        .unwrap_or(16)
}

thread_local! {
    static CURRENT: Cell<Theme> = Cell::new(Theme::default());
}

/// Sets the theme for screens drawn on this thread: [`crate::run`] sets
/// what [`Theme::detect`] found, tests set what they draw for.
pub fn set(theme: Theme) {
    CURRENT.set(theme);
}

pub fn current() -> Theme {
    CURRENT.get()
}

/// `›`, the selected row, inline code, code blocks, the spinner: cyan.
pub fn accent() -> Style {
    current().fg(Color::Cyan)
}

/// A card that worked, `✓`, `+N`: green.
pub fn success() -> Style {
    current().fg(Color::Green)
}

/// A failed card's `✗`, `exit 1`, errors, `−N`, `Allow all`: red.
pub fn failure() -> Style {
    current().fg(Color::Red)
}

/// Warnings and waiting for approval: yellow, which is faint on a light
/// background, so there the default colour in bold.
pub fn warning() -> Style {
    let theme = current();
    match theme.background {
        Background::Light => Style::new().bold(),
        _ => theme.fg(Color::Yellow),
    }
}

/// The approval box's border: yellow on any background.
pub fn warning_border() -> Style {
    current().fg(Color::Yellow)
}

/// Details beside the main text, command output, hints: dim.
pub fn secondary() -> Style {
    Style::new().dim()
}

/// Thinking: dim and italic.
pub fn thinking() -> Style {
    Style::new().dim().italic()
}

/// The `/btw` answer, fainter than secondary text: dark grey and dim.
pub fn aside() -> Style {
    current().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}

/// A diff's added line (its `+`, and its text while it has no background): green.
pub fn diff_added() -> Style {
    current().fg(Color::Green)
}

/// A diff's removed line: red.
pub fn diff_removed() -> Style {
    current().fg(Color::Red)
}

/// The background of an added line, when one can be shown.
pub fn diff_added_bg() -> Option<Color> {
    current().bg([0x21, 0x3a, 0x2b], [0xda, 0xfb, 0xe1])
}

/// The background of a removed line, when one can be shown.
pub fn diff_removed_bg() -> Option<Color> {
    current().bg([0x4a, 0x22, 0x1d], [0xff, 0xeb, 0xe9])
}

/// The changed words of an added line: a step deeper than the line's
/// background, or reversed without one.
pub fn diff_added_word() -> Style {
    word(current().bg([0x2e, 0x5c, 0x3f], [0xab, 0xf2, 0xbc]))
}

/// The changed words of a removed line.
pub fn diff_removed_word() -> Style {
    word(current().bg([0x73, 0x32, 0x2a], [0xff, 0xce, 0xcb]))
}

fn word(background: Option<Color>) -> Style {
    match background {
        Some(color) => Style::new().bg(color),
        None => Style::new().reversed(),
    }
}
