//! `/statusline`: a list above the input box, like a choice such as `/mode`
//! alone, to pick what the rows at the bottom show. Each item with what it
//! says now; a line 2 entry that moves like the items; a preview of both
//! rows below. With a status command in `config.json`, a switch on top
//! chooses between it and the items.
//!
//! Follows Codex `rust-v0.163.0` (`codex-rs/tui/src/bottom_pane/
//! status_line_setup.rs` and `multi_select_picker.rs`); no code is taken
//! from it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::bar::{Bar, Entry, Values};
use crate::text::{sanitize, truncate};
use crate::theme;

const TITLE: &str = " Configure status line";
const KEYS: &str = " Space toggle · ←/→ move · Enter save · Esc cancel";
const SWITCH: &str = "[x] Use statusLine script";
/// Item names are padded to the longest, `permission`, and three spaces.
const NAME_WIDTH: usize = "permission".len() + 3;

pub(crate) struct Setup {
    /// What saving keeps.
    pub bar: Bar,
    /// The status command in `config.json`, if any.
    script: Option<String>,
    /// The row selected: the switch, when there is one, then the entries.
    selected: usize,
}

impl Setup {
    pub fn new(bar: Bar, script: Option<String>) -> Self {
        Self {
            bar,
            script,
            selected: 0,
        }
    }

    /// Rows above the entries: the switch, when there is a command.
    fn switch(&self) -> usize {
        usize::from(self.script.is_some())
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.switch() + self.bar.entries.len() - 1);
    }

    /// Space: the switch or the selected item on or off.
    pub fn toggle(&mut self) {
        match self.selected.checked_sub(self.switch()) {
            None => self.bar.use_script = !self.bar.use_script,
            Some(index) => {
                if let Entry::Item(_, shown) = &mut self.bar.entries[index] {
                    *shown = !*shown;
                }
            }
        }
    }

    /// Left and Right: the selected entry one place earlier or later; the
    /// switch stays on top.
    pub fn shift(&mut self, later: bool) {
        let Some(index) = self.selected.checked_sub(self.switch()) else {
            return;
        };
        let to = if later {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(to) => to,
                None => return,
            }
        };
        if to < self.bar.entries.len() {
            self.bar.entries.swap(index, to);
            self.selected = self.switch() + to;
        }
    }

    /// At most `max` rows: the title and the keys, the switch, the entries,
    /// then the preview. When the entries do not all fit, they show only
    /// the rows around the one selected, with how many more there are above
    /// and below; the rest stays. With fewer than three rows left for the
    /// entries, the preview folds away; nothing when not even one fits.
    pub fn rows(&self, width: u16, max: usize, values: &Values) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, width));
        let mut head = vec![
            cut(vec![Span::styled(TITLE, Style::new().bold())]),
            cut(vec![Span::styled(KEYS, theme::secondary())]),
            Line::default(),
        ];
        if self.script.is_some() {
            let mark = if self.bar.use_script { "[x]" } else { "[ ]" };
            head.push(cut(row(
                SWITCH.replacen("[x]", mark, 1),
                None,
                self.selected == 0,
            )));
            head.push(cut(vec![
                Span::raw("   "),
                Span::styled("─".repeat(SWITCH.len()), theme::secondary()),
            ]));
        }
        let preview = match &self.script {
            Some(command) if self.bar.use_script => [
                vec![Span::styled(
                    format!(
                        "  From the statusLine script: {}",
                        sanitize(command).replace('\n', " ")
                    ),
                    theme::secondary(),
                )],
                Vec::new(),
            ],
            _ => self.bar.draw(values).0.map(|mut spans| {
                if !spans.is_empty() {
                    spans.insert(0, Span::raw(" "));
                }
                spans
            }),
        };
        let [first, second] = preview.map(cut);
        let mut tail = vec![
            Line::default(),
            cut(vec![Span::styled(" Preview", Style::new().bold())]),
            first,
            second,
        ];
        let entries = self.entries(values);
        let mut room = max.saturating_sub(head.len() + tail.len());
        if room < entries.len() && room < 3 {
            tail.clear();
            room = max.saturating_sub(head.len());
        }
        if room == 0 {
            return Vec::new();
        }
        // With the switch selected, the entries show from the top.
        let at = self.selected.saturating_sub(self.switch());
        head.extend(window(entries, at, room).into_iter().map(cut));
        head.extend(tail);
        head
    }

    /// A row for each entry, not cut to any width.
    fn entries(&self, values: &Values) -> Vec<Vec<Span<'static>>> {
        let at = self.selected.checked_sub(self.switch());
        self.bar
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let chosen = at == Some(index);
                match *entry {
                    Entry::Item(item, shown) => {
                        let mark = if shown { 'x' } else { ' ' };
                        let value = values[item as usize].as_ref();
                        let name = match value {
                            Some(_) => format!("{:NAME_WIDTH$}", item.name()),
                            None => item.name().to_owned(),
                        };
                        row(format!("[{mark}] {name}"), value, chosen)
                    }
                    Entry::Line2 => row("─── line 2 ───".to_owned(), None, chosen),
                }
            })
            .collect()
    }
}

/// A list row, marked and bold when selected like a choice's rows.
fn row(label: String, value: Option<&Vec<Span<'static>>>, chosen: bool) -> Vec<Span<'static>> {
    let mut spans = if chosen {
        vec![
            Span::styled(" › ", theme::accent().bold()),
            Span::styled(label, Style::new().bold()),
        ]
    } else {
        vec![Span::raw("   "), Span::raw(label)]
    };
    spans.extend(value.into_iter().flatten().cloned());
    spans
}

/// `room` of `rows` with row `at` among them: like a choice's list, as low
/// as it goes, with a row each for how many more there are above and below
/// when there are three rows or more.
pub(crate) fn window(
    rows: Vec<Vec<Span<'static>>>,
    at: usize,
    room: usize,
) -> Vec<Vec<Span<'static>>> {
    let total = rows.len();
    if total <= room {
        return rows;
    }
    if room < 3 {
        let first = (at + 1).saturating_sub(room);
        return rows.into_iter().skip(first).take(room).collect();
    }
    let (first, shown) = if at < room - 1 {
        (0, room - 1)
    } else if at >= total - (room - 1) {
        (total - (room - 1), room - 1)
    } else {
        // Room for both counts; `at` the last row between them.
        (at + 3 - room, room - 2)
    };
    let more = |arrow: char, count: usize| {
        vec![Span::styled(
            format!("   {arrow} {count} more"),
            theme::secondary(),
        )]
    };
    let below = total - first - shown;
    let mut window = Vec::new();
    if first > 0 {
        window.push(more('↑', first));
    }
    window.extend(rows.into_iter().skip(first).take(shown));
    if below > 0 {
        window.push(more('↓', below));
    }
    window
}
