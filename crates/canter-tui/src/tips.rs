//! The tip under the status line while the model works: a new one from
//! `assets/tips.txt` each turn, in the file's order from a place the clock
//! picks, so one comes again only after all the others. `"tips": false` in
//! `tui.json` turns them off.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use ratatui::text::{Line, Span};

use crate::text::str_width;
use crate::theme;

/// The tips, without the comment and blank lines.
static TIPS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("../assets/tips.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
});

#[derive(Clone)]
pub(crate) struct Tips {
    /// The tip the next turn gets.
    next: usize,
    /// The turn the latest tip is for, and the tip.
    turn: Option<(String, &'static str)>,
}

impl Tips {
    /// On unless `tui.json` under `env`'s home says `"tips": false`.
    pub(crate) fn load(env: &BTreeMap<String, String>) -> Option<Self> {
        let off = crate::tui_json::path(env)
            .and_then(|path| crate::tui_json::read(&path).ok().flatten())
            .is_some_and(|settings| settings.get("tips") == Some(&false.into()));
        (!off).then(|| Self::starting_at(crate::now_ms().max(0) as usize))
    }

    fn starting_at(next: usize) -> Self {
        Self {
            next: next % TIPS.len(),
            turn: None,
        }
    }

    /// Picks the tip for `turn`, which has just started; once a turn.
    pub(crate) fn start(&mut self, turn: &str) {
        if self.turn.as_ref().is_some_and(|(id, _)| id == turn) {
            return;
        }
        self.turn = Some((turn.to_owned(), TIPS[self.next]));
        self.next = (self.next + 1) % TIPS.len();
    }

    /// The row for `turn`: only one that started here, and only when the
    /// whole tip fits in `width` columns.
    pub(crate) fn line(&self, turn: &str, width: u16) -> Option<Line<'static>> {
        let (id, tip) = self.turn.as_ref()?;
        let text = format!(" Tip: {tip}");
        (id == turn && str_width(&text) <= usize::from(width))
            .then(|| Line::from(Span::styled(text, theme::secondary())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_and_key_named_is_in_the_help_box() {
        let keys: Vec<&str> = crate::help::SHORTCUTS
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .flat_map(|(key, _)| key.split(" / "))
            .map(|key| key.split(' ').next().unwrap_or_default())
            .collect();
        assert!((20..=30).contains(&TIPS.len()), "{} tips", TIPS.len());
        for tip in TIPS.iter() {
            for word in tip.split_whitespace() {
                let word = word.trim_end_matches([',', '.', ';', ':', ')']);
                if let Some(name) = word.strip_prefix('/') {
                    if name.starts_with('<') {
                        assert!(keys.contains(&word), "{word} in: {tip}");
                    } else {
                        assert!(crate::commands::find(name).is_some(), "{word} in: {tip}");
                    }
                }
                if ["Ctrl+", "Alt+", "Shift+"]
                    .iter()
                    .any(|modifier| word.starts_with(modifier))
                {
                    assert!(keys.contains(&word), "{word} in: {tip}");
                }
            }
        }
    }

    #[test]
    fn a_tip_comes_again_only_after_all_the_others() {
        let mut tips = Tips::starting_at(7);
        let mut seen = Vec::new();
        for turn in 0..TIPS.len() {
            tips.start(&format!("t{turn}"));
            let tip = tips.turn.as_ref().unwrap().1;
            assert!(!seen.contains(&tip), "again: {tip}");
            seen.push(tip);
        }
        tips.start("again");
        assert_eq!(tips.turn.as_ref().unwrap().1, seen[0]);
    }
}
