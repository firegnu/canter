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

    /// At most `max` rows: the title, the keys, the list, then the preview,
    /// with blank rows between them. When they do not fit, the blank rows
    /// go first, then the list shows only the rows around the one selected;
    /// nothing when not even one list row fits.
    pub fn rows(&self, width: u16, max: usize, values: &Values) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, width));
        let head = [
            cut(vec![Span::styled(TITLE, Style::new().bold())]),
            cut(vec![Span::styled(KEYS, theme::secondary())]),
        ];
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
        let tail = [
            cut(vec![Span::styled(" Preview", Style::new().bold())]),
            first,
            second,
        ];
        let (list, selected) = self.list(values);
        let fixed = head.len() + tail.len();
        let blank = fixed + list.len() + 2 <= max;
        let room = max.saturating_sub(fixed).min(list.len());
        if room == 0 {
            return Vec::new();
        }
        let first = (selected + 1).saturating_sub(room);
        let mut rows = Vec::from(head);
        if blank {
            rows.push(Line::default());
        }
        rows.extend(list.into_iter().skip(first).take(room).map(cut));
        if blank {
            rows.push(Line::default());
        }
        rows.extend(tail);
        rows
    }

    /// The list's rows, not cut to any width, and the place of the selected
    /// one among them.
    fn list(&self, values: &Values) -> (Vec<Vec<Span<'static>>>, usize) {
        // Marked and bold when selected, like a choice's rows.
        let row = |label: String, value: Option<&Vec<Span<'static>>>, chosen: bool| {
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
        };
        let mut rows = Vec::new();
        let mut selected = 0;
        if self.script.is_some() {
            let mark = if self.bar.use_script { "[x]" } else { "[ ]" };
            rows.push(row(
                SWITCH.replacen("[x]", mark, 1),
                None,
                self.selected == 0,
            ));
            rows.push(vec![
                Span::raw("   "),
                Span::styled("─".repeat(SWITCH.len()), theme::secondary()),
            ]);
        }
        for (index, entry) in self.bar.entries.iter().enumerate() {
            let chosen = self.selected == self.switch() + index;
            if chosen {
                selected = rows.len();
            }
            rows.push(match *entry {
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
            });
        }
        (rows, selected)
    }
}
