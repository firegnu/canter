//! The conversation screen: keeps the session snapshot up to date from events,
//! hands finished rows to the scrollback in order, and draws what is still in
//! progress plus the status line, input box and info line.

use std::collections::HashSet;

use canter_protocol::{
    Event, EventPayload, Item, ItemStatus, PermissionMode, SessionSnapshot, SessionStatus, Turn,
    TurnStatus, UserInput, apply_event_mut,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::cells;
use crate::input::InputBox;
use crate::text::{LogicalLine, sanitize, str_width, truncate, wrap};

const PLACEHOLDER: &str = "输入一句话（Enter 发送，Ctrl+J 换行）";
const MAX_INPUT_ROWS: usize = 8;
/// Border, padding and the `› ` prompt on the left; padding and border on the right.
const INPUT_LEFT: u16 = 4;
const INPUT_CHROME: u16 = 6;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What the caller should do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send this text to the session.
    Send(String),
    Quit,
}

/// How far the scrollback has got: the next turn, whether its prompt is out,
/// the next item, and how many rows of a streaming message are out.
#[derive(Debug, Default, Clone, Copy)]
struct Written {
    turn: usize,
    prompt: bool,
    item: usize,
    lines: usize,
}

pub struct App {
    snapshot: SessionSnapshot,
    home: Option<String>,
    input: InputBox,
    written: Written,
    /// Rows to add to the scrollback after the items (warnings, errors).
    notices: Vec<LogicalLine>,
    /// Items an `item.completed` event has closed, as (turn id, item id).
    completed: HashSet<(String, String)>,
    /// Turns whose error has already been shown.
    errors_shown: HashSet<String>,
    /// Unix ms when the current turn started.
    working_since: Option<i64>,
    disconnected: Option<String>,
    width: u16,
}

impl App {
    /// `home` shortens paths in the info line to `~`.
    pub fn new(snapshot: SessionSnapshot, home: Option<String>, now_ms: i64) -> Self {
        let working = matches!(
            snapshot.status,
            SessionStatus::Working | SessionStatus::WaitingApproval
        );
        Self {
            snapshot,
            home,
            input: InputBox::default(),
            written: Written::default(),
            notices: Vec::new(),
            completed: HashSet::new(),
            errors_shown: HashSet::new(),
            working_since: working.then_some(now_ms),
            disconnected: None,
            width: 80,
        }
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn apply_event(&mut self, event: &Event, now_ms: i64) {
        if let Err(error) = apply_event_mut(&mut self.snapshot, event) {
            self.notices.push(cells::notice(
                "✗ ",
                &format!("跳过一条对不上的事件（序号 {}）：{error}", event.seq),
                Style::new().red(),
            ));
            return;
        }
        match &event.payload {
            EventPayload::TurnStarted { .. } => self.working_since = Some(event.timestamp),
            EventPayload::SessionStatus { status } => match status {
                SessionStatus::Working | SessionStatus::WaitingApproval => {
                    self.working_since.get_or_insert(now_ms);
                }
                _ => self.working_since = None,
            },
            EventPayload::ItemCompleted { turn_id, item } => {
                if let Some(id) = item.id() {
                    self.completed.insert((turn_id.clone(), id.to_owned()));
                }
            }
            EventPayload::Warning { message } => {
                self.notices
                    .push(cells::notice("! ", message, Style::new().yellow()));
            }
            EventPayload::Error {
                error,
                will_retry,
                turn_id,
            } => {
                let text = if *will_retry {
                    format!("{}（会重试）", error.message)
                } else {
                    error.message.clone()
                };
                let style = if *will_retry {
                    Style::new().yellow()
                } else {
                    Style::new().red()
                };
                self.notices.push(cells::notice("✗ ", &text, style));
                if let Some(turn_id) = turn_id
                    && !will_retry
                {
                    self.errors_shown.insert(turn_id.clone());
                }
            }
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.input.is_empty() {
                    return Some(Action::Quit);
                }
                self.input.clear();
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return Some(Action::Quit),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert("\n")
            }
            KeyCode::Enter => {
                if !self.input.text().trim().is_empty() {
                    return Some(Action::Send(self.input.take()));
                }
            }
            KeyCode::Char(c) if !ctrl => self.input.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Up => self.input.vertical(-1, self.input_width()),
            KeyCode::Down => self.input.vertical(1, self.input_width()),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            _ => {}
        }
        None
    }

    /// Bracketed paste: the whole text goes in at once, newlines included.
    pub fn handle_paste(&mut self, text: &str) {
        self.input.insert(text);
    }

    pub fn send_failed(&mut self, message: &str) {
        self.notices.push(cells::notice(
            "✗ ",
            &format!("没送出去：{message}"),
            Style::new().red(),
        ));
    }

    /// The event stream ended; shown in the status line.
    pub fn disconnected(&mut self, reason: &str) {
        self.disconnected = Some(reason.to_owned());
    }

    /// Rows that are finished since the last call, in order. A row is
    /// finished once nothing before it is still in progress.
    pub(crate) fn take_history(&mut self, width: u16) -> Vec<Line<'static>> {
        self.width = width;
        let mut rows = Vec::new();
        let cwd = self.snapshot.settings.cwd.clone();
        while let Some(turn) = self.snapshot.turns.get(self.written.turn) {
            if !self.written.prompt {
                rows.extend(cells::prompt(&turn.input, width));
                self.written.prompt = true;
            }
            let echo = echo_index(turn);
            let mut waiting = false;
            while let Some(item) = turn.items.get(self.written.item) {
                let index = self.written.item;
                let done = self.item_done(turn, index);
                if Some(index) == echo {
                    // Already shown as the turn's prompt.
                } else if let Item::AgentMessage { text, .. } = item {
                    let source = if done {
                        text.as_str()
                    } else {
                        complete_lines(text)
                    };
                    let lines = cells::agent_message(source);
                    let from = self.written.lines.min(lines.len());
                    rows.extend(cells::wrap_all(&lines[from..], width));
                    self.written.lines = self.written.lines.max(lines.len());
                } else if done {
                    rows.extend(cells::item(item, true, &cwd, width));
                }
                if !done {
                    waiting = true;
                    break;
                }
                self.written.item += 1;
                self.written.lines = 0;
            }
            if waiting || turn.status == TurnStatus::InProgress {
                break;
            }
            let error_shown = self.errors_shown.contains(&turn.id);
            if let Some(footer) = cells::turn_footer(turn, error_shown) {
                rows.extend(wrap(&footer, width));
            }
            self.written = Written {
                turn: self.written.turn + 1,
                ..Written::default()
            };
        }
        rows.extend(cells::wrap_all(&std::mem::take(&mut self.notices), width));
        rows
    }

    /// Lays out the viewport for a screen of `width` × `height`.
    pub(crate) fn view(&self, width: u16, height: u16, now_ms: i64) -> View {
        let input_width = usize::from(width.saturating_sub(INPUT_CHROME));
        let layout = self.input.layout(input_width);
        let first = layout
            .cursor
            .0
            .saturating_sub(MAX_INPUT_ROWS - 1)
            .min(layout.rows.len().saturating_sub(MAX_INPUT_ROWS));
        let mut input = Vec::new();
        for (row, range) in layout
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(MAX_INPUT_ROWS)
        {
            let prompt = if row == 0 { "› " } else { "  " };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(prompt, Style::new().cyan().bold()),
            ];
            if self.input.is_empty() {
                spans.push(Span::styled(PLACEHOLDER, Style::new().dim()));
            } else {
                spans.push(Span::raw(self.input.text()[range.clone()].to_owned()));
            }
            input.push(Line::from(truncate(
                spans,
                usize::from(width.saturating_sub(2)),
            )));
        }
        let cursor = (
            INPUT_LEFT + layout.cursor.1 as u16,
            1 + (layout.cursor.0 - first) as u16,
        );

        let bottom = 1 + 1 + input.len() as u16 + 2 + 1;
        let mut live = self.live_rows(width);
        let room = usize::from(height.saturating_sub(bottom));
        if live.len() > room {
            live.drain(..live.len() - room);
        }
        View {
            live,
            status: self.status_line(now_ms),
            input,
            cursor,
            info: self.info_line(width),
        }
    }

    fn input_width(&self) -> usize {
        usize::from(self.width.saturating_sub(INPUT_CHROME))
    }

    /// Statusless items (messages, thinking) are done once `item.completed`
    /// arrived, their turn ended, or a later item started.
    fn item_done(&self, turn: &Turn, index: usize) -> bool {
        let item = &turn.items[index];
        let status = match item {
            Item::CommandExecution { status, .. }
            | Item::FileChange { status, .. }
            | Item::FileRead { status, .. } => Some(*status),
            _ => None,
        };
        if let Some(status) = status {
            return status != ItemStatus::InProgress;
        }
        let closed = item
            .id()
            .is_some_and(|id| self.completed.contains(&(turn.id.clone(), id.to_owned())));
        closed || turn.status != TurnStatus::InProgress || index + 1 < turn.items.len()
    }

    /// Everything not yet in the scrollback.
    fn live_rows(&self, width: u16) -> Vec<Line<'static>> {
        let cwd = &self.snapshot.settings.cwd;
        let mut rows = Vec::new();
        for (turn_index, turn) in self
            .snapshot
            .turns
            .iter()
            .enumerate()
            .skip(self.written.turn)
        {
            let current = turn_index == self.written.turn;
            if !current || !self.written.prompt {
                rows.extend(cells::prompt(&turn.input, width));
            }
            let echo = echo_index(turn);
            let first = if current { self.written.item } else { 0 };
            for (index, item) in turn.items.iter().enumerate().skip(first) {
                if Some(index) == echo {
                    continue;
                }
                let done = self.item_done(turn, index);
                if let Item::AgentMessage { text, .. } = item {
                    let lines = cells::agent_message(text);
                    let from = if current && index == self.written.item {
                        self.written.lines.min(lines.len())
                    } else {
                        0
                    };
                    rows.extend(cells::wrap_all(&lines[from..], width));
                } else {
                    rows.extend(cells::item(item, done, cwd, width));
                }
            }
        }
        for queued in &self.snapshot.queued_inputs {
            let text = first_line(&queued.input);
            rows.push(Line::from(truncate(
                vec![Span::styled(
                    format!("  · 排队：{text}"),
                    Style::new().dim(),
                )],
                usize::from(width),
            )));
        }
        rows
    }

    fn status_line(&self, now_ms: i64) -> Line<'static> {
        if let Some(reason) = &self.disconnected {
            return Line::from(Span::styled(
                format!(" ✗ 和服务端断开了：{reason}"),
                Style::new().red(),
            ));
        }
        let elapsed = self
            .working_since
            .map(|since| format_elapsed(now_ms.saturating_sub(since)))
            .unwrap_or_default();
        match self.snapshot.status {
            SessionStatus::Working => {
                let frame = SPINNER[(now_ms.max(0) / 80) as usize % SPINNER.len()];
                Line::from(vec![
                    Span::styled(format!(" {frame} "), Style::new().cyan()),
                    Span::raw("干活中"),
                    Span::styled(format!(" · {elapsed}"), Style::new().dim()),
                ])
            }
            SessionStatus::WaitingApproval => Line::from(vec![
                Span::styled(" ⏸ 等你批准", Style::new().yellow()),
                Span::styled(format!(" · {elapsed}"), Style::new().dim()),
            ]),
            SessionStatus::Starting => Line::from(Span::styled(" 启动中…", Style::new().dim())),
            SessionStatus::Error => {
                let message = self.snapshot.error.as_ref().map_or(String::new(), |error| {
                    format!("：{}", sanitize(&error.message))
                });
                Line::from(Span::styled(
                    format!(" ✗ 出错了{message}"),
                    Style::new().red(),
                ))
            }
            SessionStatus::Closed => Line::from(Span::styled(" 会话已结束", Style::new().dim())),
            SessionStatus::Idle | SessionStatus::Unknown => Line::default(),
        }
    }

    fn info_line(&self, width: u16) -> Line<'static> {
        let settings = &self.snapshot.settings;
        let mode = match settings.approval_policy {
            PermissionMode::AskForExecution => "只问执行",
            PermissionMode::AllowAll => "全放行",
            // Unknown modes are treated as the strictest one.
            PermissionMode::AlwaysAsk | PermissionMode::Unknown => "每次问",
        };
        let mut cwd = sanitize(&settings.cwd);
        if let Some(home) = &self.home
            && let Some(rest) = cwd.strip_prefix(home.trim_end_matches('/'))
            && (rest.is_empty() || rest.starts_with('/'))
        {
            cwd = format!("~{rest}");
        }
        let text = format!(" {} · {mode} · {cwd}", sanitize(&settings.model));
        Line::from(truncate(
            vec![Span::styled(text, Style::new().dim())],
            usize::from(width),
        ))
    }
}

/// The viewport's content, laid out for one frame.
pub(crate) struct View {
    live: Vec<Line<'static>>,
    status: Line<'static>,
    input: Vec<Line<'static>>,
    /// Cursor inside the input box, relative to the box's top-left corner.
    cursor: (u16, u16),
    info: Line<'static>,
}

impl View {
    pub fn height(&self) -> u16 {
        (self.live.len() + 1 + 1 + self.input.len() + 2 + 1) as u16
    }

    pub fn render(self, frame: &mut Frame) {
        let area = frame.area();
        let mut y = area.y;
        let mut row = |height: u16| {
            let rect = Rect::new(area.x, y, area.width, height).intersection(area);
            y += height;
            rect
        };
        let live = row(self.live.len() as u16);
        frame.render_widget(Paragraph::new(self.live), live);
        row(1);
        frame.render_widget(Paragraph::new(self.status), row(1));
        let input = row(self.input.len() as u16 + 2);
        frame.render_widget(
            Paragraph::new(self.input).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().dim()),
            ),
            input,
        );
        frame.render_widget(Paragraph::new(self.info), row(1));
        let cursor = Position::new(input.x + self.cursor.0, input.y + self.cursor.1);
        if input.contains(cursor) {
            frame.set_cursor_position(cursor);
        }
    }
}

/// The item that repeats the turn's input as a user message, if any.
fn echo_index(turn: &Turn) -> Option<usize> {
    if turn.input.is_empty() {
        return None;
    }
    turn.items.iter().position(
        |item| matches!(item, Item::UserMessage { content, .. } if *content == turn.input),
    )
}

/// The text up to and including its last newline.
fn complete_lines(text: &str) -> &str {
    text.rfind('\n').map_or("", |end| &text[..=end])
}

fn first_line(input: &[UserInput]) -> String {
    let text = input
        .iter()
        .find_map(|part| match part {
            UserInput::Text { text } => Some(sanitize(text)),
            UserInput::Unknown => None,
        })
        .unwrap_or_default();
    let first = text.lines().next().unwrap_or_default();
    if str_width(first) < str_width(&text) {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

fn format_elapsed(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}
