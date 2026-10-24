//! The session list for `canter --resume` without an id: this directory's
//! sessions first, most recently updated on top; Tab shows every directory.

use canter_protocol::{SessionStatus, SessionSummary};
use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::approval::shorten_home;
use crate::text::{sanitize, str_width, truncate};

/// Rows of sessions shown at once.
const MAX_ROWS: usize = 12;
const STATUS_COLUMNS: usize = 8;
const DIRECTORY_COLUMNS: usize = 24;

/// What the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Open(String),
    Cancel,
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
                "   还没有会话"
            } else {
                "   这个目录还没有会话"
            };
            rows.push(Line::from(Span::styled(text, Style::new().dim())));
        }
        for (index, session) in shown.iter().enumerate().skip(first).take(room) {
            rows.push(self.row(session, index == self.selected, inner));
        }
        rows.push(Line::default());
        let tab = if self.all {
            "Tab 只看当前目录"
        } else {
            "Tab 看全部目录的会话"
        };
        rows.push(Line::from(Span::styled(
            format!("   {tab} · Enter 打开 · Esc 取消"),
            Style::new().dim(),
        )));
        PickerView { rows }
    }

    fn row(&self, session: &SessionSummary, selected: bool, width: usize) -> Line<'static> {
        let mut spans = vec![if selected {
            Span::styled(" › ", Style::new().cyan().bold())
        } else {
            Span::raw("   ")
        }];
        spans.push(Span::raw(format!("{}  ", format_time(session.updated_at))));
        spans.push(Span::raw(pad(status_label(session.status), STATUS_COLUMNS)));
        if self.all {
            let directory = shorten_home(&sanitize(&session.cwd), self.home.as_deref());
            let directory = truncate(vec![Span::raw(directory)], DIRECTORY_COLUMNS - 2);
            let used: usize = directory.iter().map(|span| str_width(&span.content)).sum();
            spans.extend(
                directory
                    .into_iter()
                    .map(|span| span.style(Style::new().dim())),
            );
            spans.push(Span::raw(" ".repeat(DIRECTORY_COLUMNS - used)));
        }
        match &session.name {
            Some(name) => spans.push(Span::raw(sanitize(name).replace('\n', " "))),
            None => spans.push(Span::styled("（还没说话）", Style::new().dim())),
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
    pub fn height(&self) -> u16 {
        self.rows.len() as u16 + 2
    }

    pub fn render(self, frame: &mut Frame) {
        let border = Style::new().dim();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title_top(Line::from(vec![
                Span::styled("─ ", border),
                Span::styled("接着哪个会话？", Style::new().bold()),
                Span::raw(" "),
            ]));
        frame.render_widget(Paragraph::new(self.rows).block(block), frame.area());
    }
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "空闲",
        SessionStatus::Working => "干活中",
        SessionStatus::WaitingApproval => "等批准",
        SessionStatus::Starting => "启动中",
        SessionStatus::Error => "出错",
        SessionStatus::Closed => "已结束",
        SessionStatus::Unknown => "？",
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
