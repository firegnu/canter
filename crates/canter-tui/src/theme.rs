//! Colours by purpose. Screens ask for a purpose (accent, failure, warning…)
//! and never name a colour themselves.
//!
//! Text uses the terminal's own 16 colours, so it follows the terminal's
//! colour scheme. Only the diff backgrounds and the background of a sent
//! message are true colours, and only when the terminal can show them and its
//! background is known to be dark or light; so are the shimmers' shades;
//! and the welcome header's pony, when the terminal can show them; and
//! code, when `/theme` chose a theme other than `terminal` and the terminal
//! shows 256 colours or more.

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
    fn bg(self, dark: Shade, light: Shade) -> Option<Color> {
        let ([r, g, b], index) = match self.background {
            Background::Dark => dark,
            Background::Light => light,
            Background::Unknown => return None,
        };
        match self.depth {
            Depth::TrueColor => Some(Color::Rgb(r, g, b)),
            Depth::Ansi256 => Some(Color::Indexed(index)),
            Depth::Ansi16 | Depth::None => None,
        }
    }

    /// `rgb` as a true colour, or else the nearest of the 256.
    fn shade(self, rgb: [u8; 3]) -> Color {
        match self.depth {
            Depth::TrueColor => Color::Rgb(rgb[0], rgb[1], rgb[2]),
            _ => Color::Indexed(nearest_256(rgb)),
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
/// numbering: black and the dark colours (0–6) and dark grey (8) are dark;
/// light grey (7) and the bright colours (9–15) are light.
fn colorfgbg(value: &str) -> Option<Background> {
    match value.rsplit(';').next()?.parse::<u8>().ok()? {
        0..=6 | 8 => Some(Background::Dark),
        7 | 9..=15 => Some(Background::Light),
        _ => None,
    }
}

/// A background as a true colour, and the colour standing in for it in the
/// 256-colour table. The stand-ins are picked by hand, not the nearest
/// colour: the nearest to a dark green or red is often a grey, which loses
/// the hue. They are past the first 16, which change with the terminal's
/// settings.
type Shade = ([u8; 3], u8);

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

/// A `!` command, which runs in the shell without the model: in shell mode
/// the input box's border, its `!` prompt and the hint on its bottom border;
/// the `!` on the command's card. Magenta, the terminal's own, which its
/// colour scheme keeps readable on a dark or light background and with 256
/// colours (user's choice).
pub fn shell() -> Style {
    current().fg(Color::Magenta)
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

/// What an item of the status bar at the bottom shows, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarItem {
    /// Cyan.
    Model,
    /// Magenta.
    Thinking,
    /// Yellow.
    Cost,
    /// Blue.
    Directory,
    /// Green.
    Branch,
    /// The cache hit percentages and the tokens a second: cyan.
    Number,
}

/// An item of the status bar in its colour; the words around it are
/// [`secondary`]. Without colours dim, as the bar was before it had colours.
pub fn bar_item(item: BarItem) -> Style {
    bar(match item {
        BarItem::Model | BarItem::Number => Color::Cyan,
        BarItem::Thinking => Color::Magenta,
        BarItem::Cost => Color::Yellow,
        BarItem::Directory => Color::Blue,
        BarItem::Branch => Color::Green,
    })
}

/// The context percentages on the status bar, by how much room is left
/// before automatic compaction (`left`, 0 to 100): 50% or more green, 20% or
/// more yellow, less red. Without colours dim.
pub fn bar_room(left: u64) -> Style {
    bar(match left {
        50.. => Color::Green,
        20.. => Color::Yellow,
        _ => Color::Red,
    })
}

fn bar(color: Color) -> Style {
    match current().depth {
        Depth::None => secondary(),
        _ => Style::new().fg(color),
    }
}

/// Code coloured by syntax: colour `number` (0–15) of the terminal's palette,
/// as the syntax theme names it. Yellow and bright yellow are faint on a
/// light background, so there the default colour, not bold: code has too
/// many of them for bold.
pub fn palette(number: u8) -> Style {
    let theme = current();
    if theme.background == Background::Light && matches!(number, 3 | 11) {
        return Style::new();
    }
    let color = match number {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        _ => return Style::new(),
    };
    theme.fg(color)
}

/// Code coloured by a theme of `/theme` that names colour `number` of the
/// 256 past the first 16.
pub fn code_indexed(number: u8) -> Style {
    current().fg(Color::Indexed(number))
}

/// Code coloured by a theme of `/theme` in true colour `rgb`: as it is, or
/// the nearest of the 256 without true colour.
pub fn code_rgb(rgb: [u8; 3]) -> Style {
    let theme = current();
    theme.fg(theme.shade(rgb))
}

/// A diff's added line (its `+`, and its text while it has no background): green.
pub fn diff_added() -> Style {
    current().fg(Color::Green)
}

/// A diff's removed line: red.
pub fn diff_removed() -> Style {
    current().fg(Color::Red)
}

/// The background of an added line, when one can be shown. 256 colours:
/// 22 (`#005F00`) on dark, 194 (`#D7FFD7`) on light.
pub fn diff_added_bg() -> Option<Color> {
    current().bg(([0x21, 0x3a, 0x2b], 22), ([0xda, 0xfb, 0xe1], 194))
}

/// The background of a removed line, when one can be shown. 256 colours:
/// 52 (`#5F0000`) on dark, 224 (`#FFD7D7`) on light.
pub fn diff_removed_bg() -> Option<Color> {
    current().bg(([0x4a, 0x22, 0x1d], 52), ([0xff, 0xeb, 0xe9], 224))
}

/// The gutter (number and sign) of an added line: a shade deeper than the
/// line, apart from the changed words. 256 colours: 151 (`#AFD7AF`) on
/// light; on dark the line's 22, already the darkest green past black.
pub fn diff_added_gutter_bg() -> Option<Color> {
    current().bg(([0x1b, 0x30, 0x24], 22), ([0xc6, 0xef, 0xd0], 151))
}

/// The gutter of a removed line. 256 colours: 181 (`#D7AFAF`) on light; on
/// dark the line's 52, already the darkest red past black.
pub fn diff_removed_gutter_bg() -> Option<Color> {
    current().bg(([0x3d, 0x1c, 0x18], 52), ([0xff, 0xd9, 0xd5], 181))
}

/// The changed words of an added line: a step deeper than the line's
/// background, or reversed without one. 256 colours: 28 (`#008700`) on
/// dark, 157 (`#AFFFAF`) on light.
pub fn diff_added_word() -> Style {
    word(current().bg(([0x2e, 0x5c, 0x3f], 28), ([0xab, 0xf2, 0xbc], 157)))
}

/// The changed words of a removed line. 256 colours: 88 (`#870000`) on
/// dark, 217 (`#FFAFAF`) on light.
pub fn diff_removed_word() -> Style {
    word(current().bg(([0x73, 0x32, 0x2a], 88), ([0xff, 0xce, 0xcb], 217)))
}

/// The background of a sent message, a grey a step off the terminal's own,
/// when one can be shown. 256 colours: 236 (`#303030`) on dark, 254
/// (`#E4E4E4`) on light.
pub fn user_bg() -> Option<Color> {
    current().bg(([0x2d, 0x2e, 0x33], 236), ([0xe9, 0xe9, 0xe5], 254))
}

fn word(background: Option<Color>) -> Style {
    match background {
        Some(color) => Style::new().bg(color),
        None => Style::new().reversed(),
    }
}

/// A part of the pony on the welcome header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pony {
    Body,
    /// The mane, the tail and the hooves.
    Mane,
    Eye,
    /// The light in the eye.
    Shine,
    Nose,
    Blush,
}

/// What a part of the pony is drawn in; `None` leaves it out. True colour
/// (chestnut body, dark brown mane, cream nose), else one of the 256 colours
/// past the first 16, picked by hand: the nearest turns the mane grey and the
/// body red, and the blush into the body. Body 173 (`#D7875F`), mane 94
/// (`#875F00`), eye 234 (`#1C1C1C`), shine 231 (`#FFFFFF`), nose 223
/// (`#FFD7AF`), blush 211 (`#FF87AF`). With 16 colours the whole pony is
/// cyan, the accent, and without colours the text colour; then the eye is
/// left out, a hole in the head.
pub fn pony(part: Pony) -> Option<Style> {
    let ([r, g, b], index) = match part {
        Pony::Body => ([0xc0, 0x70, 0x3f], 173),
        Pony::Mane => ([0x5b, 0x32, 0x20], 94),
        Pony::Eye => ([0x1e, 0x1a, 0x18], 234),
        Pony::Shine => ([0xff, 0xff, 0xff], 231),
        Pony::Nose => ([0xf1, 0xd2, 0xa6], 223),
        Pony::Blush => ([0xe8, 0x9a, 0x8c], 211),
    };
    let eye = matches!(part, Pony::Eye | Pony::Shine);
    match current().depth {
        Depth::TrueColor => Some(Style::new().fg(Color::Rgb(r, g, b))),
        Depth::Ansi256 => Some(Style::new().fg(Color::Indexed(index))),
        _ if eye => None,
        Depth::Ansi16 => Some(accent()),
        Depth::None => Some(Style::new()),
    }
}

/// How the two shimmers (see [`crate::shimmer`]) are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shimmer {
    /// Fixed colours shading into each other, two sets for a dark or light
    /// terminal: with 256 colours or more and the background known.
    Gradient,
    /// A few steps of the terminal's own colours and dim and bold.
    Steps,
    /// `NO_COLOR`: nothing sweeps.
    Off,
}

pub(crate) fn shimmer() -> Shimmer {
    let theme = current();
    match (theme.depth, theme.background) {
        (Depth::None, _) => Shimmer::Off,
        (Depth::Ansi16, _) | (_, Background::Unknown) => Shimmer::Steps,
        _ => Shimmer::Gradient,
    }
}

/// A letter of the working verb on the status line, `light` (0 to 1) from
/// the light sweeping over it. Gradient: grey shading to white on a dark
/// terminal, to black on a light one, bold in the light's middle. Steps:
/// dim away from the light, normal near it, bold in its middle.
pub(crate) fn working_shimmer(light: f32) -> Style {
    let theme = current();
    let colours = match theme.background {
        Background::Dark => Some(([0x7f, 0x7b, 0x75], [0xff, 0xff, 0xff])),
        Background::Light => Some(([0x8a, 0x85, 0x7e], [0x00, 0x00, 0x00])),
        Background::Unknown => None,
    };
    match (shimmer(), colours) {
        (Shimmer::Gradient, Some((grey, bright))) => {
            let style = Style::new().fg(theme.shade(mix(grey, bright, light * 0.95)));
            if light > 0.5 { style.bold() } else { style }
        }
        (Shimmer::Off, _) => Style::new(),
        _ if light < 0.2 => Style::new().dim(),
        _ if light < 0.6 => Style::new(),
        _ => Style::new().bold(),
    }
}

/// A highlighted character, drawn in `style`, under the light (`light` 0
/// to 1) sweeping the highlight once a copy worked. Gradient: the
/// highlight's background shading to cyan, under text the colour of the
/// terminal's background. Steps: a cyan background where the light is.
pub(crate) fn copy_shimmer(style: Style, light: f32) -> Style {
    let theme = current();
    let colours = match theme.background {
        Background::Dark => Some(([0xe6, 0xe4, 0xdf], [0x56, 0xc2, 0xcf], [0x1c, 0x1d, 0x20])),
        Background::Light => Some(([0x26, 0x24, 0x1f], [0x0f, 0x8a, 0x99], [0xfb, 0xfb, 0xf9])),
        Background::Unknown => None,
    };
    if light <= 0.0 {
        return style;
    }
    match (shimmer(), colours) {
        (Shimmer::Gradient, Some((highlight, cyan, text))) => style
            .remove_modifier(Modifier::REVERSED)
            .fg(theme.shade(text))
            .bg(theme.shade(mix(highlight, cyan, light))),
        (Shimmer::Off, _) => style,
        // Reversed: cyan behind the terminal's background colour.
        _ => style
            .fg(Color::Cyan)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED),
    }
}

/// `from` shaded `t` (0 to 1) of the way to `to`.
fn mix(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| {
        let (a, b) = (f32::from(from[i]), f32::from(to[i]));
        (a + (b - a) * t).round() as u8
    })
}

/// The colour of the 256 nearest to `rgb`, past the first 16 (which change
/// with the terminal's settings): the 6×6×6 cube and the greys.
fn nearest_256(rgb: [u8; 3]) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let colour = |index: u8| -> [u8; 3] {
        if index < 232 {
            let i = usize::from(index - 16);
            [LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6]]
        } else {
            [8 + 10 * (index - 232); 3]
        }
    };
    (16..=255)
        .min_by_key(|&index| {
            colour(index)
                .iter()
                .zip(rgb)
                .map(|(&a, b)| (i32::from(a) - i32::from(b)).pow(2))
                .sum::<i32>()
        })
        .expect("not empty")
}
