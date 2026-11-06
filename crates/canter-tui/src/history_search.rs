//! Ctrl+R's local history list. The original input box stays untouched until Enter.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::input::InputBox;
use crate::input_history::InputHistory;

#[derive(Debug, Default)]
pub(crate) struct HistorySearch {
    pub query: InputBox,
    all: bool,
    selected: usize,
}

impl HistorySearch {
    /// True closes the search; Enter recalls without sending.
    pub fn key(
        &mut self,
        key: KeyEvent,
        history: &mut InputHistory,
        cwd: &str,
        input: &mut InputBox,
    ) -> bool {
        let matches = history.search_matches(cwd, self.all, self.query.text());
        let before = self.query.text().to_owned();
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Enter => {
                if let Some(&index) = matches.get(self.selected) {
                    history.show(index, input);
                    return true;
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Tab => {
                self.all = !self.all;
                self.selected = 0;
            }
            KeyCode::Down | KeyCode::Char('r')
                if key.code == KeyCode::Down || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.selected = (self.selected + 1).min(matches.len().saturating_sub(1));
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.insert(c.encode_utf8(&mut [0; 4]))
            }
            KeyCode::Backspace => self.query.backspace(),
            KeyCode::Delete => self.query.delete(),
            KeyCode::Left => self.query.left(),
            KeyCode::Right => self.query.right(),
            KeyCode::Home => self.query.home(),
            KeyCode::End => self.query.end(),
            _ => {}
        }
        if self.query.text() != before {
            self.selected = 0;
        }
        false
    }

    pub fn paste(&mut self, text: &str) {
        self.query
            .insert(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        self.selected = 0;
    }

    pub fn rows(
        &self,
        history: &InputHistory,
        cwd: &str,
        width: u16,
        max: usize,
    ) -> Vec<Line<'static>> {
        let matches = history.search_matches(cwd, self.all, self.query.text());
        let count = matches.len();
        let scope = if self.all {
            "all directories"
        } else {
            "this directory"
        };
        let mut rows = vec![Line::from(Span::styled(
            format!(
                " Search history  {scope} · {count} {}",
                if count == 1 { "match" } else { "matches" }
            ),
            crate::theme::accent(),
        ))];
        let room = max.saturating_sub(1).min(8);
        if matches.is_empty() {
            rows.push(Line::from(Span::styled(
                "   No matching history",
                crate::theme::secondary(),
            )));
        }
        let first = (self.selected + 1).saturating_sub(room);
        for (position, &index) in matches.iter().enumerate().skip(first).take(room) {
            rows.push(history.search_row(index, position == self.selected, width));
        }
        rows.truncate(max);
        rows
    }

    pub fn hint(&self) -> Line<'static> {
        let scope = if self.all {
            "this directory"
        } else {
            "all directories"
        };
        Line::from(Span::styled(
            format!(" ↑↓ select · Enter put in box · Tab {scope} · Esc cancel"),
            crate::theme::secondary(),
        ))
    }
}
