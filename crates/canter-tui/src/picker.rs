//! The session list for `canter --resume` without an id: this directory's
//! sessions first, most recently updated on top; Tab shows every directory.

use canter_protocol::{SessionStatus, SessionSummary};
use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::approval::shorten_home;
use crate::short_path::fit_path;
use crate::text::{sanitize, str_width, truncate};
use crate::theme;

/// Rows of sessions shown at once.
const MAX_ROWS: usize = 12;
const STATUS_COLUMNS: usize = 10;
const DIRECTORY_COLUMNS: usize = 24;

/// How the session list was left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Open(String),
    Cancel,
    /// Input ended or the terminal was asked to shut down.
    Exit,
}

pub struct Picker {
    sessions: Vec<SessionSummary>,
    cwd: String,
    home: Option<String>,
    /// Every directory, not just `cwd`.
    all: bool,
    selected: usize,
}

impl Picker {
    /// `home` shortens paths to `~`.
    pub fn new(mut sessions: Vec<SessionSummary>, cwd: String, home: Option<String>) -> Self {
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        Self {
            sessions,
            cwd,
            home,
            all: false,
            selected: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Pick> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let shown = self.shown().len();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(shown.saturating_sub(1)),
            KeyCode::Tab => {
                self.all = !self.all;
                self.selected = 0;
            }
            KeyCode::Enter => {
                return self
                    .shown()
                    .get(self.selected)
                    .map(|session| Pick::Open(session.id.clone()));
            }
            KeyCode::Esc => return Some(Pick::Cancel),
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(Pick::Cancel);
            }
            _ => {}
        }
        None
    }

    /// The session at the top of the list as it opens: `canter -c`.
    pub fn first(&self) -> Option<String> {
        self.shown().first().map(|session| session.id.clone())
    }

    fn shown(&self) -> Vec<&SessionSummary> {
        self.sessions
            .iter()
            .filter(|session| self.all || session.cwd == self.cwd)
            .collect()
    }

    /// Lays the list out for a screen of `width` × `height`.
    pub(crate) fn view(&self, width: u16, height: u16) -> PickerView {
        let shown = self.shown();
        let inner = usize::from(width.saturating_sub(2));
        let room = usize::from(height.saturating_sub(4)).clamp(1, MAX_ROWS);
        let first = (self.selected + 1).saturating_sub(room);
        let mut rows = Vec::new();
        if shown.is_empty() {
            let text = if self.all {
                "   No sessions yet"
            } else {
                "   No sessions in this directory yet"
            };
            rows.push(Line::from(Span::styled(text, theme::secondary())));
        }
        for (index, session) in shown.iter().enumerate().skip(first).take(room) {
            rows.push(self.row(session, index == self.selected, inner));
        }
        rows.push(Line::default());
        let tab = if self.all {
            "Tab this directory only"
        } else {
            "Tab all directories"
        };
        rows.push(Line::from(Span::styled(
            format!("   {tab} · Enter open · Esc cancel"),
            theme::secondary(),
        )));
        PickerView { rows }
    }

    fn row(&self, session: &SessionSummary, selected: bool, width: usize) -> Line<'static> {
        let mut spans = vec![if selected {
            Span::styled(" › ", theme::accent().bold())
        } else {
            Span::raw("   ")
        }];
        spans.push(Span::raw(format!("{}  ", format_time(session.updated_at))));
        spans.push(Span::raw(pad(status_label(session.status), STATUS_COLUMNS)));
        if self.all {
            let directory = shorten_home(&sanitize(&session.cwd), self.home.as_deref());
            let directory = fit_path(&directory, DIRECTORY_COLUMNS - 2);
            let used = str_width(&directory);
            spans.push(Span::styled(directory, theme::secondary()));
            spans.push(Span::raw(" ".repeat(DIRECTORY_COLUMNS - used)));
        }
        match &session.name {
            Some(name) => spans.push(Span::raw(sanitize(name).replace('\n', " "))),
            None => spans.push(Span::styled("(no messages yet)", theme::secondary())),
        }
        let style = if selected {
            Style::new().bold()
        } else {
            Style::new()
        };
        Line::from(truncate(spans, width)).style(style)
    }
}

/// The list's content, laid out for one frame.
pub(crate) struct PickerView {
    rows: Vec<Line<'static>>,
}

impl PickerView {
    /// At the top of the screen, as tall as the list.
    pub fn render(self, frame: &mut Frame) {
        let area = frame.area();
        let height = (self.rows.len() as u16 + 2).min(area.height);
        let border = theme::secondary();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title_top(Line::from(vec![
                Span::styled("─ ", border),
                Span::styled("Which session to resume?", Style::new().bold()),
                Span::raw(" "),
            ]));
        frame.render_widget(
            Paragraph::new(self.rows).block(block),
            Rect { height, ..area },
        );
    }
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Working => "Working",
        SessionStatus::WaitingApproval => "Waiting",
        SessionStatus::Starting => "Starting",
        SessionStatus::Error => "Error",
        SessionStatus::Closed => "Closed",
        SessionStatus::Unknown => "?",
    }
}

/// `10-24 14:02` in local time.
fn format_time(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map_or_else(
        || "-".repeat(11),
        |time| time.format("%m-%d %H:%M").to_string(),
    )
}

fn pad(text: &str, columns: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(columns.saturating_sub(str_width(text)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With every directory shown, a long one loses whole directories from
    /// its middle and keeps its first part and its last: the name column
    /// stays where it was.
    #[test]
    fn a_long_directory_loses_directories_from_its_middle() {
        let session = |id: &str, cwd: &str| SessionSummary {
            id: id.into(),
            cwd: cwd.into(),
            name: Some(format!("session {id}")),
            status: SessionStatus::Idle,
            updated_at: 0,
            last_event_seq: 0,
        };
        let mut picker = Picker::new(
            vec![
                session("a", "/home/dev/projects/personal/worktrees/p2-55"),
                session("b", "/home/dev/canter"),
            ],
            "/elsewhere".into(),
            Some("/home/dev".into()),
        );
        picker.handle_key(KeyEvent::from(KeyCode::Tab));
        let text = |line: &Line| -> String {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let rows: Vec<String> = picker.view(100, 20).rows.iter().map(text).collect();
        let start = 3 + 13 + STATUS_COLUMNS;
        let directory = |row: &str| row.chars().skip(start).collect::<String>();
        assert_eq!(
            directory(&rows[0]),
            format!("{:<24}session a", "~/…/worktrees/p2-55"),
            "{rows:#?}"
        );
        assert_eq!(directory(&rows[1]), format!("{:<24}session b", "~/canter"));
    }
}
