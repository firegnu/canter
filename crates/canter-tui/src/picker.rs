//! The session list for `canter --resume` without an id: this directory's
//! sessions first, most recently updated on top; Tab shows every directory.

use canter_protocol::{
    SearchSource, SessionSearchParams, SessionSearchResult, SessionStatus, SessionSummary,
};
use chrono::{Local, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::approval::shorten_home;
use crate::short_path::fit_path;
use crate::text::{LogicalLine, sanitize, str_width, truncate, wrap};
use crate::theme;

/// Rows of sessions shown at once.
const MAX_ROWS: usize = 12;
const STATUS_COLUMNS: usize = 10;
const DIRECTORY_COLUMNS: usize = 24;

/// How the session list was left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Open(String),
    /// Confirmed deletion, handled without leaving the picker.
    Trash(String),
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
    confirm: Option<String>,
    error: Option<String>,
    search: Option<SessionSearch>,
}

#[derive(Default)]
struct SessionSearch {
    query: crate::input::InputBox,
    all: bool,
    selected: usize,
    results: Vec<SessionSearchResult>,
    pending: bool,
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
            confirm: None,
            error: None,
            search: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Pick> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if matches!(key.code, KeyCode::Char('c' | 'd'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return Some(Pick::Cancel);
        }
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y')
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    return self.confirm.take().map(Pick::Trash);
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => self.confirm = None,
                _ => {}
            }
            return None;
        }
        if let Some(search) = &mut self.search {
            let before = search.query.text().to_owned();
            match key.code {
                KeyCode::Esc => {
                    self.search = None;
                    return None;
                }
                KeyCode::Up => search.selected = search.selected.saturating_sub(1),
                KeyCode::Down => {
                    search.selected =
                        (search.selected + 1).min(search.results.len().saturating_sub(1))
                }
                KeyCode::Enter if !search.pending => {
                    return search
                        .results
                        .get(search.selected)
                        .map(|row| Pick::Open(row.session.id.clone()));
                }
                KeyCode::Tab => {
                    search.all = !search.all;
                    search.pending = true;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    search.query.insert(c.encode_utf8(&mut [0; 4]))
                }
                KeyCode::Backspace => search.query.backspace(),
                KeyCode::Delete => search.query.delete(),
                KeyCode::Left => search.query.left(),
                KeyCode::Right => search.query.right(),
                KeyCode::Home => search.query.home(),
                KeyCode::End => search.query.end(),
                _ => {}
            }
            if search.query.text() != before {
                search.pending = true;
            }
            if search.pending {
                search.results.clear();
                search.selected = 0;
            }
            return None;
        }
        let shown = self.shown().len();
        match key.code {
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.error = None;
                self.search = Some(SessionSearch {
                    pending: true,
                    ..Default::default()
                });
            }
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
            KeyCode::Char('d') if key.modifiers.is_empty() => {
                self.confirm = self
                    .shown()
                    .get(self.selected)
                    .map(|session| session.id.clone());
                self.error = None;
            }
            KeyCode::Esc => return Some(Pick::Cancel),
            _ => {}
        }
        None
    }

    pub fn trashed(&mut self, id: &str) {
        self.sessions.retain(|session| session.id != id);
        self.selected = self.selected.min(self.shown().len().saturating_sub(1));
        self.error = None;
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(search) = &mut self.search {
            let before = search.query.text().to_owned();
            search
                .query
                .insert(&text.split_whitespace().collect::<Vec<_>>().join(" "));
            if search.query.text() != before {
                search.pending = true;
                search.selected = 0;
                search.results.clear();
            }
        }
    }

    pub(crate) fn search_params(&self) -> Option<SessionSearchParams> {
        self.search.as_ref().map(|search| SessionSearchParams {
            query: search.query.text().to_owned(),
            cwd: (!search.all).then(|| self.cwd.clone()),
        })
    }

    pub(crate) fn search_finished(&mut self, result: Result<Vec<SessionSearchResult>, String>) {
        match result {
            Ok(results) => {
                if let Some(search) = &mut self.search {
                    search.results = results;
                    search.selected = 0;
                    search.pending = false;
                }
            }
            Err(error) => {
                self.search = None;
                self.error = Some(error);
            }
        }
    }

    pub fn deletion_failed(&mut self, error: String) {
        self.error = Some(error);
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
        if let Some(search) = &self.search {
            return self.search_view(search, width, height);
        }
        let shown = self.shown();
        let inner = usize::from(width.saturating_sub(2));
        let notice = if self.confirm.is_some() {
            Span::styled("Move selected session to trash? Y/N", theme::accent())
        } else if let Some(error) = &self.error {
            Span::styled(sanitize(error).replace('\n', " "), theme::failure())
        } else {
            Span::raw("")
        };
        let notice = wrap(
            &LogicalLine::new(vec![Span::raw("   ")], vec![notice]),
            width.saturating_sub(2),
        );
        let room = usize::from(height.saturating_sub(3 + notice.len() as u16)).clamp(1, MAX_ROWS);
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
        rows.extend(notice);
        let tab = if self.all {
            "Tab this directory only"
        } else {
            "Tab all directories"
        };
        rows.push(Line::from(Span::styled(
            format!("   {tab} · Enter open · d delete · Esc cancel"),
            theme::secondary(),
        )));
        PickerView {
            rows,
            title: "Which session to resume?".into(),
            cursor: None,
        }
    }

    fn search_view(&self, search: &SessionSearch, width: u16, height: u16) -> PickerView {
        let inner = usize::from(width.saturating_sub(2));
        let scope = if search.all {
            "all directories"
        } else {
            "this directory"
        };
        let mut rows = vec![Line::from(truncate(
            vec![Span::styled(
                format!(" Sessions  {scope} · search: {}", search.query.text()),
                theme::accent(),
            )],
            inner,
        ))];
        let room = usize::from(height.saturating_sub(6) / 2).min(MAX_ROWS);
        let first = (search.selected + 1).saturating_sub(room);
        if search.pending || search.results.is_empty() {
            rows.push(Line::from(Span::styled(
                if search.pending {
                    "   Searching…"
                } else {
                    "   No matching sessions"
                },
                theme::secondary(),
            )));
        }
        for (index, result) in search.results.iter().enumerate().skip(first).take(room) {
            let selected = index == search.selected;
            let session = &result.session;
            let directory = fit_path(
                &shorten_home(&sanitize(&session.cwd), self.home.as_deref()),
                DIRECTORY_COLUMNS - 2,
            );
            let marker = if selected { " › " } else { "   " };
            let name = session.name.as_deref().unwrap_or("(unnamed session)");
            rows.push(Line::from(truncate(
                vec![Span::styled(
                    format!(
                        "{marker}{}  {directory}  {}",
                        format_time(session.updated_at),
                        sanitize(name).replace('\n', " ")
                    ),
                    if selected {
                        theme::accent().bold()
                    } else {
                        Style::new()
                    },
                )],
                inner,
            )));
            let snippet = result
                .snippet
                .as_ref()
                .map(|snippet| {
                    format!(
                        "       {}{}",
                        match snippet.source {
                            SearchSource::UserMessage => "you: ",
                            SearchSource::AgentMessage => "canter: ",
                        },
                        sanitize(&snippet.text).replace('\n', " ")
                    )
                })
                .unwrap_or_default();
            rows.push(Line::from(truncate(
                vec![Span::styled(snippet, theme::secondary())],
                inner,
            )));
        }
        rows.push(Line::default());
        let layout = search
            .query
            .layout(usize::from(width.saturating_sub(6)).max(1));
        let range = &layout.rows[layout.cursor.0];
        let cursor = Some((4 + layout.cursor.1 as u16, 1 + rows.len() as u16));
        rows.push(Line::from(format!(
            " / {}",
            &search.query.text()[range.clone()]
        )));
        let scope = if search.all {
            "this directory"
        } else {
            "all directories"
        };
        rows.push(Line::from(Span::styled(
            format!("   ↑↓ select · Enter open · Tab {scope} · Esc back"),
            theme::secondary(),
        )));
        PickerView {
            rows,
            title: String::new(),
            cursor,
        }
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
            None => spans.push(Span::styled("(unnamed session)", theme::secondary())),
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
    title: String,
    cursor: Option<(u16, u16)>,
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
                Span::styled(self.title, Style::new().bold()),
                Span::raw(" "),
            ]));
        frame.render_widget(
            Paragraph::new(self.rows).block(block),
            Rect { height, ..area },
        );
        if let Some((x, y)) = self.cursor {
            let position = ratatui::layout::Position::new(area.x + x, area.y + y);
            if area.contains(position) {
                frame.set_cursor_position(position);
            }
        }
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

    #[test]
    fn slash_searches_escape_restores_the_list_and_d_still_deletes() {
        let mut picker = Picker::new(
            vec![SessionSummary {
                agent_status: None,
                parent: None,
                id: "s1".into(),
                cwd: "/work".into(),
                name: Some("Original session".into()),
                status: SessionStatus::Idle,
                updated_at: 1,
                last_event_seq: 1,
            }],
            "/work".into(),
            None,
        );
        assert_eq!(picker.handle_key(KeyEvent::from(KeyCode::Char('/'))), None);
        let text: String = picker
            .view(120, 20)
            .rows
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            text.contains("search:") && text.contains("Searching"),
            "{text}"
        );
        picker.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(picker.handle_key(KeyEvent::from(KeyCode::Esc)), None);
        assert_eq!(
            picker.handle_key(KeyEvent::from(KeyCode::Enter)),
            Some(Pick::Open("s1".into()))
        );
        picker.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('y'))),
            Some(Pick::Trash("s1".into()))
        );
    }

    #[test]
    fn deletion_requires_confirmation_and_ctrl_d_still_cancels() {
        let mut picker = Picker::new(
            vec![SessionSummary {
                agent_status: None,
                parent: None,
                id: "s1".into(),
                cwd: "/project".into(),
                name: Some("my session".into()),
                status: SessionStatus::Idle,
                updated_at: 0,
                last_event_seq: 1,
            }],
            "/project".into(),
            None,
        );
        assert_eq!(picker.handle_key(KeyEvent::from(KeyCode::Char('d'))), None);
        let text: String = picker
            .view(100, 20)
            .rows
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            text.contains("Move") && text.contains("trash") && text.contains("Y/N"),
            "{text}"
        );
        assert_eq!(picker.handle_key(KeyEvent::from(KeyCode::Char('n'))), None);
        assert_eq!(
            picker.handle_key(KeyEvent::from(KeyCode::Enter)),
            Some(Pick::Open("s1".into()))
        );
        picker.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('y'))),
            Some(Pick::Trash("s1".into()))
        );
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Pick::Cancel)
        );
    }

    #[test]
    fn deletion_error_keeps_the_recovery_instruction_on_narrow_screens() {
        let mut picker = Picker::new(Vec::new(), "/work".into(), None);
        picker.deletion_failed(
            "This server does not support deleting sessions; run canter serve --replace and retry"
                .into(),
        );
        let text: String = picker
            .view(55, 20)
            .rows
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("--replace and retry"), "{text}");
    }

    /// With every directory shown, a long one loses whole directories from
    /// its middle and keeps its first part and its last: the name column
    /// stays where it was.
    #[test]
    fn a_long_directory_loses_directories_from_its_middle() {
        let session = |id: &str, cwd: &str| SessionSummary {
            agent_status: None,
            parent: None,
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
