//! The two shimmers: a light sweeping over the working verb on the status
//! line, and a light sweeping once over the highlight when a copy of the
//! selection worked. Where the light is comes from the time, not from how
//! many frames were drawn, so it moves at the same speed however often the
//! screen is drawn. How much light looks like what is the theme's
//! ([`theme::working_shimmer`], [`theme::copy_shimmer`]).
//!
//! The verb's sweep follows Codex `rust-v0.163.0`
//! (`codex-rs/tui/src/shimmer.rs`): two seconds a sweep, ten columns of
//! nothing either side of the word, a light five columns to either side of
//! its middle. Codex shades between the terminal's own colours, which it
//! asks the terminal for; canter does not ask and shades between fixed ones.
//! No code is taken from it.

use std::ops::Range;

use ratatui::text::Span;

use crate::theme::{self, Shimmer};

/// One sweep of the verb.
const SWEEP_MS: i64 = 2000;
/// Columns the verb's light runs on either side of the word.
const PAD: f32 = 10.0;
/// How far the light reaches either side of its middle, in columns.
const HALF: f32 = 5.0;
/// How long the light takes over a copied highlight.
pub(crate) const COPY_MS: i64 = 500;
/// Columns the copy's light starts left of the highlight and ends right of it.
const COPY_PAD: f32 = 6.0;
/// How far a row's light is behind the row above, in columns.
const LAG: f32 = 1.5;
/// Half the width of the copy's light with 16 colours: four columns.
const BAND: f32 = 2.0;

/// `verb` (such as `Thinking`, or `Running ` with its space) with the light
/// where it is at `now_ms`; as it is without colours.
pub(crate) fn working(verb: &'static str, now_ms: i64) -> Vec<Span<'static>> {
    if theme::shimmer() == Shimmer::Off {
        return vec![Span::raw(verb)];
    }
    let word = verb.trim_end();
    let letters = word.chars().count() as f32;
    let period = letters + 2.0 * PAD;
    let at = now_ms.rem_euclid(SWEEP_MS) as f32 / SWEEP_MS as f32 * period - PAD;
    let mut spans: Vec<Span<'static>> = word
        .chars()
        .enumerate()
        .map(|(i, letter)| {
            let style = theme::working_shimmer(level(i as f32, at, HALF));
            Span::styled(letter.to_string(), style)
        })
        .collect();
    if word.len() < verb.len() {
        spans.push(Span::raw(&verb[word.len()..]));
    }
    spans
}

/// The light over a copied highlight on screen.
pub(crate) struct CopyLight {
    /// The highlight's first row on screen.
    first: usize,
    /// Where the middle of the light is on that row.
    at: f32,
}

impl CopyLight {
    /// The light `elapsed` ms after the copy worked, over a highlight in
    /// `columns` on `rows` (those on screen): it starts left of the columns
    /// and ends right of them, each row behind the one above, in [`COPY_MS`]
    /// however many rows. `None` once it is over, and without colours.
    pub fn new(elapsed: i64, columns: Range<usize>, rows: Range<usize>) -> Option<Self> {
        if !(0..COPY_MS).contains(&elapsed) || theme::shimmer() == Shimmer::Off {
            return None;
        }
        let behind = LAG * rows.len().saturating_sub(1) as f32;
        let run = columns.len() as f32 + 2.0 * COPY_PAD + behind;
        let at = columns.start as f32 - COPY_PAD + run * elapsed as f32 / COPY_MS as f32;
        Some(Self {
            first: rows.start,
            at,
        })
    }

    /// How much light (0 to 1) is on `column` of row `row`. With 16
    /// colours there is light or none: the four columns nearest its middle.
    pub fn level(&self, row: usize, column: usize) -> f32 {
        let at = self.at - LAG * row.saturating_sub(self.first) as f32;
        let column = column as f32;
        match theme::shimmer() {
            Shimmer::Gradient => level(column, at, HALF),
            _ if (column + 0.5 - at).abs() < BAND => 1.0,
            _ => 0.0,
        }
    }
}

/// How much light (0 to 1) is on column `x` when the middle of a light
/// reaching `half` columns either side is at `at`: full in the middle,
/// fading as a cosine to nothing at its edge.
fn level(x: f32, at: f32, half: f32) -> f32 {
    let distance = (x - at).abs();
    if distance > half {
        return 0.0;
    }
    0.5 * (1.0 + (std::f32::consts::PI * distance / half).cos())
}
