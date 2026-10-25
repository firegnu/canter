//! The conversation screen: keeps the session snapshot up to date from events
//! and draws the whole terminal: the conversation from the top, following the
//! latest output unless scrolled back, then the status line, the command list
//! while a `/` command is typed, the input box (or the approval box) and the
//! info line at the bottom.

use std::collections::{HashSet, VecDeque};

use canter_protocol::{
    ApprovalDecision, ApprovalRequest, ApprovalResolution, Event, EventPayload, Item, ItemStatus,
    PendingRequest, PermissionMode, RequestKind, SessionSnapshot, SessionStatus,
    SessionUpdateParams, Turn, TurnStatus, UserInput, apply_event_mut,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::approval::{self, ApprovalBox, shorten_home};
use crate::cells;
use crate::commands::{self, Chooser, Command};
use crate::input::InputBox;
use crate::text::{LogicalLine, sanitize, str_width, truncate, wrap};

const PLACEHOLDER: &str = "Type a message (Enter to send, Ctrl+J for a new line)";
const MAX_INPUT_ROWS: usize = 8;
/// Border, padding and the `› ` prompt on the left; padding and border on the right.
const INPUT_LEFT: u16 = 4;
const INPUT_CHROME: u16 = 6;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Conversation rows one step of the mouse wheel scrolls.
const WHEEL_ROWS: usize = 3;
/// Changes of what is on screen kept for keys still waiting to be handled.
const SHOWN_KEPT: usize = 8;
/// After an approval box's answers come on screen, answer keys are dropped
/// for this long, and every key that comes in meanwhile pushes it back. The
/// terminal may still show what was there before for a moment, so a key
/// right after the box appears may not be meant for it (user's decision).
pub const ANSWER_GUARD_MS: i64 = 300;

/// What the caller should do after a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this text to the session.
    Send(String),
    /// Answer an approval request.
    Resolve {
        request_id: String,
        decision: ApprovalDecision,
    },
    /// Stop this turn.
    Interrupt {
        turn_id: String,
    },
    Quit,
    /// Open a new session with this one's directory, model and permission
    /// mode, and switch to it.
    NewSession,
    /// Show the session list and switch to the session picked.
    PickSession,
    /// Fetch the available models and offer a choice.
    PickModel,
    /// Compact this session's context.
    Compact,
    /// Change this session's settings.
    Update(SessionUpdateParams),
}

/// A place in the conversation: a turn, whether its prompt is before it, and
/// the item. Places later in the conversation compare greater.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    turn: usize,
    prompt: bool,
    item: usize,
}

/// A line from the client or server (warning, error, who answered), shown
/// where the finished part of the conversation ended when it came.
struct Notice {
    at: Place,
    line: LogicalLine,
}

/// The finished part of the conversation, wrapped for `width`, with the
/// notices in their places. Finished parts do not change, so they are wrapped
/// once, and again only when the width changes.
#[derive(Default)]
struct History {
    width: u16,
    rows: Vec<Line<'static>>,
    /// Where the finished part ends.
    end: Place,
    /// How many notices are among the rows.
    notices: usize,
}

/// Scrolling back through the conversation.
#[derive(Default)]
struct Scroll {
    /// The conversation row at the top of the view while scrolled back;
    /// `None` while following the latest output.
    top: Option<usize>,
    /// The conversation changed since scrolling back: the hint shows.
    unseen: bool,
    /// From the last layout: rows in view and the furthest top row.
    page: usize,
    max_top: usize,
    /// The last frame's width, history length and rows after the history,
    /// to tell whether the conversation changed since.
    seen: (u16, usize, Vec<Line<'static>>),
}

/// The command list's selection, and whether Esc closed it, for one input
/// text; once something else is typed, the list starts afresh.
#[derive(Default)]
struct CommandList {
    text: String,
    selected: usize,
    closed: bool,
}

/// The approval box's highlighted answer and body scroll, for one request.
#[derive(Debug, Default)]
struct ApprovalCursor {
    request: String,
    selected: usize,
    scroll: usize,
}

/// What a frame showed. A key acts on the frame that was on screen when it
/// came in, not on anything drawn since: a key meant for the input box or
/// for one request never answers the next request.
#[derive(Debug, Default, Clone, PartialEq)]
struct Shown {
    /// The request in the approval box; `None` when the input box showed.
    approval: Option<String>,
    /// Whether that box had room for its answers.
    answers: bool,
    /// The turn in progress.
    turn: Option<String>,
    /// The choice whose choices showed, numbered as [`App::choose`] opened it.
    chooser: Option<u64>,
    /// How many completions the input box showed.
    completions: u64,
}

impl Shown {
    /// The request whose answers showed.
    fn answering(&self) -> Option<&str> {
        self.approval.as_deref().filter(|_| self.answers)
    }
}

pub struct App {
    snapshot: SessionSnapshot,
    home: Option<String>,
    /// This terminal's `clientName`; what others send is marked with theirs.
    client_name: String,
    input: InputBox,
    commands: CommandList,
    /// A choice offered above the input box, such as `/mode` alone.
    chooser: Option<Chooser>,
    /// How many choices have been offered, numbering them.
    choosers: u64,
    /// How many times a command name was completed in the input box.
    completions: u64,
    /// In the order they came, which is also the order of their places.
    notices: Vec<Notice>,
    history: History,
    scroll: Scroll,
    /// Items an `item.completed` event has closed, as (turn id, item id).
    completed: HashSet<(String, String)>,
    /// Turns whose error has already been shown.
    errors_shown: HashSet<String>,
    /// Unix ms when the current turn started.
    working_since: Option<i64>,
    /// Why the event stream is down while reconnecting.
    reconnecting: Option<String>,
    /// The turn an interrupt was sent for.
    interrupting: Option<String>,
    /// Approval requests this terminal answered; their box stays closed
    /// while the answer is on its way.
    answered: HashSet<String>,
    approval: ApprovalCursor,
    /// From the last layout: body rows per PgUp/PgDn and the furthest scroll.
    approval_page: usize,
    approval_max_scroll: usize,
    /// What the frame being drawn shows; on screen after [`App::frame_on_screen`].
    drawing: Shown,
    /// Recent frames on screen: each entry holds from that frame number on.
    shown: VecDeque<(u64, Shown)>,
    /// Unix ms until which answer keys are dropped; see [`ANSWER_GUARD_MS`].
    guard_until: i64,
    /// Unix ms until which Enter in the input box does not send: the same
    /// guard, started when the input box comes back after an approval box.
    send_guard_until: i64,
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
            client_name: crate::CLIENT_NAME.to_owned(),
            input: InputBox::default(),
            commands: CommandList::default(),
            chooser: None,
            choosers: 0,
            completions: 0,
            notices: Vec::new(),
            history: History::default(),
            scroll: Scroll::default(),
            completed: HashSet::new(),
            errors_shown: HashSet::new(),
            working_since: working.then_some(now_ms),
            reconnecting: None,
            interrupting: None,
            answered: HashSet::new(),
            approval: ApprovalCursor::default(),
            approval_page: 1,
            approval_max_scroll: 0,
            drawing: Shown::default(),
            shown: VecDeque::new(),
            guard_until: i64::MIN,
            send_guard_until: i64::MIN,
            width: 80,
        }
    }

    /// Sets this terminal's `clientName` (the default is [`crate::CLIENT_NAME`]).
    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = name.into();
        self
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn apply_event(&mut self, event: &Event, now_ms: i64) {
        let open_request = match &event.payload {
            EventPayload::RequestResolved { request_id, .. } => {
                self.snapshot.pending_requests.iter().any(|request| {
                    request.id == *request_id && matches!(request.kind, RequestKind::Approval(_))
                })
            }
            _ => false,
        };
        if let Err(error) = apply_event_mut(&mut self.snapshot, event) {
            self.notice(cells::notice(
                "✗ ",
                &format!(
                    "Skipped an event that does not fit (seq {}): {error}",
                    event.seq
                ),
                Style::new().red(),
            ));
            return;
        }
        match &event.payload {
            EventPayload::TurnStarted { .. } => self.working_since = Some(event.timestamp),
            EventPayload::TurnCompleted { turn } => {
                if self.interrupting.as_ref() == Some(&turn.id) {
                    self.interrupting = None;
                }
            }
            EventPayload::SessionStatus { status, .. } => match status {
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
            EventPayload::RequestResolved {
                request_id,
                resolution,
                client_name,
            } => {
                self.answered.remove(request_id);
                if open_request
                    && let Some(notice) = self.resolution_notice(resolution, client_name.as_deref())
                {
                    self.notice(notice);
                }
            }
            EventPayload::Warning { message } => {
                self.notice(cells::notice("! ", message, Style::new().yellow()));
            }
            EventPayload::Error {
                error,
                will_retry,
                turn_id,
            } => {
                let text = if *will_retry {
                    format!("{} (will retry)", error.message)
                } else {
                    error.message.clone()
                };
                let style = if *will_retry {
                    Style::new().yellow()
                } else {
                    Style::new().red()
                };
                self.notice(cells::notice("✗ ", &text, style));
                if let Some(turn_id) = turn_id
                    && !will_retry
                {
                    self.errors_shown.insert(turn_id.clone());
                }
            }
            _ => {}
        }
    }

    /// The frame numbered `frame` is on screen from `at` (Unix ms).
    pub(crate) fn frame_on_screen(&mut self, frame: u64, at: i64) {
        let before = self.shown.back().map(|(_, shown)| shown);
        if before != Some(&self.drawing) {
            let answering = self.drawing.answering();
            if answering.is_some() && before.and_then(Shown::answering) != answering {
                self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
            }
            if before.is_some_and(|shown| shown.approval.is_some())
                && self.drawing.approval.is_none()
            {
                self.send_guard_until = self.send_guard_until.max(at + ANSWER_GUARD_MS);
            }
            self.shown.push_back((frame, self.drawing.clone()));
            // Keys wait at most a frame or two before they are handled.
            if self.shown.len() > SHOWN_KEPT {
                self.shown.pop_front();
            }
        }
    }

    /// A key pressed at `now_ms` on the latest frame on screen.
    pub fn handle_key(&mut self, key: KeyEvent, now_ms: i64) -> Option<Action> {
        let shown = self.shown.back().map(|(_, shown)| shown.clone());
        self.key_on(key, shown.unwrap_or_default(), now_ms)
    }

    /// A key that came in at `at` (Unix ms) while frame `frame` was on
    /// screen. Before any frame, keys go to the input box.
    pub(crate) fn handle_key_on(&mut self, key: KeyEvent, frame: u64, at: i64) -> Option<Action> {
        let shown = self
            .shown
            .iter()
            .rev()
            .find(|(since, _)| *since <= frame)
            .map(|(_, shown)| shown.clone());
        self.key_on(key, shown.unwrap_or_default(), at)
    }

    fn key_on(&mut self, key: KeyEvent, shown: Shown, at: i64) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let guarded = at < self.guard_until;
        if guarded {
            self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
        }
        let send_guarded = at < self.send_guard_until;
        if send_guarded {
            self.send_guard_until = self.send_guard_until.max(at + ANSWER_GUARD_MS);
        }
        if shown.approval.is_some() {
            return self.approval_key(key, shown, guarded);
        }
        if self.chooser.is_some() && !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let seen = shown.chooser == Some(self.choosers);
            return self.chooser_key(key, seen, send_guarded);
        }
        if let Some((matches, selected)) = self.command_list() {
            let last = matches.len() - 1;
            let list = match key.code {
                // Closes only the list, not the turn in progress.
                KeyCode::Esc => Some((selected, true)),
                KeyCode::Up => Some((selected.saturating_sub(1), false)),
                KeyCode::Down => Some(((selected + 1).min(last), false)),
                KeyCode::Tab => {
                    self.complete(matches[selected]);
                    return None;
                }
                _ => None,
            };
            if let Some((selected, closed)) = list {
                self.commands = CommandList {
                    text: self.input.text().to_owned(),
                    selected,
                    closed,
                };
                return None;
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return self.interrupt_seen(shown.turn),
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
                // Right after an approval box closed it was meant for the box;
                // before a completion showed, for what was there before it.
                if !send_guarded && shown.completions == self.completions {
                    return self.enter();
                }
            }
            KeyCode::PageUp => self.scroll_by(-(self.scroll.page as isize)),
            KeyCode::PageDown => self.scroll_by(self.scroll.page as isize),
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

    /// Enter in the input box: runs the command whose whole name is typed,
    /// or sends the message. With `/` alone or the start of a name, it only
    /// completes the command selected, like Tab: the first one, `/compact`,
    /// cannot be undone (user's decision).
    fn enter(&mut self) -> Option<Action> {
        if let Some((matches, selected)) = self.command_list()
            && commands::typing(self.input.text())
                .and_then(commands::find)
                .is_none()
        {
            self.complete(matches[selected]);
            return None;
        }
        let typed = commands::parse(self.input.text()).map(|(name, args)| {
            let command = commands::find(name).ok_or_else(|| name.to_owned());
            (command, args.to_owned())
        });
        match typed {
            Some((Ok(command), args)) => self.run_command(command, &args),
            Some((Err(name), _)) => {
                self.scroll.top = None;
                self.warning(&format!("Unknown command: /{name}"));
                None
            }
            None if self.input.text().trim().is_empty() => None,
            None => {
                // Sending follows the latest output again.
                self.scroll.top = None;
                Some(Action::Send(self.input.take()))
            }
        }
    }

    /// Puts `/name ` in the input box; the list closes, and Enter runs it.
    fn complete(&mut self, command: &Command) {
        self.completions += 1;
        self.input.clear();
        self.input.insert(&format!("/{} ", command.name));
    }

    /// Runs `command`; the input box is emptied unless it fails. Text after
    /// a command that takes none, such as pasted lines, is refused, not
    /// dropped.
    fn run_command(&mut self, command: &Command, args: &str) -> Option<Action> {
        // Like sending, so that what the command shows is in view.
        self.scroll.top = None;
        let run = if command.takes_args || args.is_empty() {
            (command.run)(self, args)
        } else {
            Err(format!("/{} takes no arguments", command.name))
        };
        match run {
            Ok(action) => {
                self.input.clear();
                action
            }
            Err(message) => {
                self.warning(&message);
                None
            }
        }
    }

    /// The commands listed above the input box for what is typed, and the
    /// one selected. None while the input cannot be a command, nothing
    /// matches, or Esc closed the list.
    fn command_list(&self) -> Option<(Vec<&'static Command>, usize)> {
        let text = self.input.text();
        let typed = commands::typing(text)?;
        let same = self.commands.text == text;
        if same && self.commands.closed {
            return None;
        }
        let matches = commands::matching(typed);
        let last = matches.len().checked_sub(1)?;
        let selected = if same { self.commands.selected } else { 0 };
        Some((matches, selected.min(last)))
    }

    /// Keys while a choice is offered: Up and Down select, Enter chooses
    /// (not right after an approval box closed), Esc, Ctrl+C or Ctrl+D
    /// cancels, like in the session list. Other keys do nothing. Only keys
    /// pressed while its choices were on screen (`seen`) select or choose;
    /// any key can cancel.
    fn chooser_key(&mut self, key: KeyEvent, seen: bool, send_guarded: bool) -> Option<Action> {
        let chooser = self.chooser.as_mut()?;
        let last = chooser.choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Up if seen => chooser.selected = chooser.selected.saturating_sub(1),
            KeyCode::Down if seen => chooser.selected = (chooser.selected + 1).min(last),
            KeyCode::Enter if seen && !send_guarded => {
                let chooser = self.chooser.take()?;
                return chooser
                    .choices
                    .into_iter()
                    .nth(chooser.selected)
                    .map(|choice| choice.action);
            }
            KeyCode::Esc => self.chooser = None,
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.chooser = None;
            }
            _ => {}
        }
        None
    }

    /// Offers a choice above the input box.
    pub(crate) fn choose(&mut self, chooser: Chooser) {
        self.choosers += 1;
        self.chooser = Some(chooser);
    }

    /// Lines for the conversation from the client, such as `/help`'s.
    pub(crate) fn show(&mut self, lines: Vec<LogicalLine>) {
        for line in lines {
            self.notice(line);
        }
    }

    /// The mouse wheel scrolls the conversation, also while the approval box
    /// is open. Other mouse events do nothing.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_by(-(WHEEL_ROWS as isize)),
            MouseEventKind::ScrollDown => self.scroll_by(WHEEL_ROWS as isize),
            _ => {}
        }
    }

    /// Moves the view `rows` up (negative) or down, as far as the last frame
    /// allows. [`App::view`] follows the latest output again once the view
    /// reaches the bottom.
    fn scroll_by(&mut self, rows: isize) {
        let top = self.scroll.top.unwrap_or(self.scroll.max_top);
        self.scroll.top = Some(top.saturating_add_signed(rows).min(self.scroll.max_top));
    }

    /// Bracketed paste: the whole text goes in at once, newlines included.
    pub fn handle_paste(&mut self, text: &str) {
        self.input.insert(text);
    }

    /// A line for the conversation, e.g. the server's version differs.
    pub fn warning(&mut self, message: &str) {
        self.notice(cells::notice("! ", message, Style::new().yellow()));
    }

    /// Puts `line` after everything finished so far, before what is still
    /// in progress.
    fn notice(&mut self, line: LogicalLine) {
        self.extend_history(self.width);
        self.notices.push(Notice {
            at: self.history.end,
            line,
        });
    }

    pub fn send_failed(&mut self, message: &str) {
        self.notice(cells::notice(
            "✗ ",
            &format!("Not sent: {message}"),
            Style::new().red(),
        ));
    }

    /// The answer did not reach the server; the box opens again.
    pub fn answer_failed(&mut self, request_id: &str, message: &str) {
        self.answered.remove(request_id);
        self.notice(cells::notice(
            "✗ ",
            &format!("Answer not sent: {message}"),
            Style::new().red(),
        ));
    }

    /// Another client answered first, so this terminal's answer was ignored.
    pub fn answer_late(&mut self) {
        self.notice(cells::notice(
            "· ",
            "Your answer came too late and was ignored",
            Style::new().dim(),
        ));
    }

    pub fn interrupt_failed(&mut self, message: &str) {
        self.interrupting = None;
        self.notice(cells::notice(
            "✗ ",
            &format!("Interrupt failed: {message}"),
            Style::new().red(),
        ));
    }

    /// The event stream is down; the status line says so until [`App::reconnected`].
    pub fn reconnecting(&mut self, reason: &str) {
        self.reconnecting = Some(reason.to_owned());
    }

    pub fn reconnected(&mut self) {
        self.reconnecting = None;
    }

    /// Adds what finished since the last call to the history, notices in
    /// their places. A part is finished once it and everything before it is
    /// done. A new width wraps the whole history again.
    fn extend_history(&mut self, width: u16) {
        if self.history.width != width {
            self.history = History {
                width,
                ..History::default()
            };
        }
        loop {
            while let Some(notice) = self.notices.get(self.history.notices)
                && notice.at <= self.history.end
            {
                self.history.rows.extend(wrap(&notice.line, width));
                self.history.notices += 1;
            }
            let Some((rows, next)) = self.finished_part(self.history.end, width) else {
                break;
            };
            self.history.rows.extend(rows);
            self.history.end = next;
        }
    }

    /// The rows of the part at `at` (a prompt, an item or a turn's end) and
    /// the place after it, if that part is finished.
    fn finished_part(&self, at: Place, width: u16) -> Option<(Vec<Line<'static>>, Place)> {
        let turn = self.snapshot.turns.get(at.turn)?;
        if !at.prompt {
            let from = source(&self.client_name, &turn.client_name);
            let rows = cells::prompt(&turn.input, from, width);
            return Some((rows, Place { prompt: true, ..at }));
        }
        if let Some(item) = turn.items.get(at.item) {
            if !self.item_done(turn, at.item) {
                return None;
            }
            let rows = if Some(at.item) == echo_index(turn) {
                // Already shown as the turn's prompt.
                Vec::new()
            } else {
                cells::item(item, true, &self.snapshot.settings.cwd, width)
            };
            let next = Place {
                item: at.item + 1,
                ..at
            };
            return Some((rows, next));
        }
        if turn.status == TurnStatus::InProgress {
            return None;
        }
        let error_shown = self.errors_shown.contains(&turn.id);
        let rows = cells::turn_footer(turn, error_shown)
            .map(|footer| wrap(&footer, width))
            .unwrap_or_default();
        let next = Place {
            turn: at.turn + 1,
            ..Place::default()
        };
        Some((rows, next))
    }

    /// Lays out the whole screen of `width` × `height`.
    pub(crate) fn view(&mut self, width: u16, height: u16, now_ms: i64) -> View {
        self.width = width;
        // Leave the status line, the info line, the hint and one row of the conversation.
        let box_height = height.saturating_sub(4);
        let approval = self.open_approval().map(|(request, approval)| {
            let (selected, scroll) = if self.approval.request == request.id {
                (self.approval.selected, self.approval.scroll)
            } else {
                (0, 0)
            };
            let home = self.home.as_deref();
            // Answers already on screen and past the guard; a box that
            // first shows them in this frame starts a guard.
            let taken = now_ms >= self.guard_until
                && self.shown.back().and_then(|(_, shown)| shown.answering())
                    == Some(request.id.as_str());
            let laid = approval::layout(
                &self.snapshot,
                request,
                approval,
                home,
                selected,
                scroll,
                taken,
                width,
                box_height,
            );
            (request.id.clone(), laid)
        });
        self.drawing = Shown {
            approval: approval.as_ref().map(|(request, _)| request.clone()),
            answers: approval.as_ref().is_some_and(|(_, laid)| laid.fits),
            turn: self
                .snapshot
                .turns
                .iter()
                .rev()
                .find(|turn| turn.status == TurnStatus::InProgress)
                .map(|turn| turn.id.clone()),
            chooser: None,
            completions: self.completions,
        };
        let bottom = match approval {
            Some((request, laid)) => {
                self.approval_page = laid.page;
                self.approval_max_scroll = laid.max_scroll;
                if self.approval.request == request {
                    self.approval.scroll = self.approval.scroll.min(laid.max_scroll);
                }
                Bottom::Approval(laid)
            }
            None => self.input_view(width, now_ms),
        };
        // Commands are only for the input box.
        let list = match bottom {
            Bottom::Input { .. } => self.list_view(
                width,
                usize::from(height.saturating_sub(2 + bottom.height())),
            ),
            Bottom::Approval(_) => Vec::new(),
        };
        // The choice offered is on screen when any of its choices fit.
        self.drawing.chooser = self
            .chooser
            .as_ref()
            .filter(|_| !list.is_empty())
            .map(|_| self.choosers);

        self.extend_history(width);
        let live = self.live_rows(width);
        let history = &self.history.rows;
        let (seen_width, seen_history, seen_live) = &self.scroll.seen;
        // Finished rows stay as they are, so only the rows after the history
        // seen last time can have changed.
        if self.scroll.top.is_some()
            && *seen_width == width
            && !history[*seen_history..].iter().chain(&live).eq(seen_live)
        {
            self.scroll.unseen = true;
        }
        self.scroll.seen = (width, history.len(), live.clone());

        // Nothing is above the conversation, so it starts without the first
        // prompt's blank row.
        let lead = usize::from(
            history
                .iter()
                .chain(&live)
                .next()
                .is_some_and(|row| row.width() == 0),
        );
        let total = history.len() + live.len() - lead;
        let room = usize::from(height.saturating_sub(2 + bottom.height())) - list.len();
        if self.scroll.top.is_some_and(|top| top + room >= total) {
            self.scroll.top = None;
        }
        if self.scroll.top.is_none() {
            self.scroll.unseen = false;
        }
        let hint = self.scroll.unseen.then(|| {
            // PgDn scrolls the approval box while it is open.
            let how = match bottom {
                Bottom::Input { .. } => " · scroll down or press PgDn",
                Bottom::Approval(_) => " · scroll down",
            };
            Line::from(vec![
                Span::styled(" ↓ New output below", Style::new().cyan()),
                Span::styled(how, Style::new().dim()),
            ])
        });
        let area = room.saturating_sub(usize::from(hint.is_some()));
        self.scroll.page = area.max(1);
        self.scroll.max_top = total.saturating_sub(area);
        let top = self.scroll.top.unwrap_or(self.scroll.max_top);
        let conversation = history
            .iter()
            .chain(&live)
            .skip(lead + top)
            .take(area)
            .cloned()
            .collect();
        View {
            conversation,
            hint,
            status: self.status_line(now_ms),
            list,
            bottom,
            info: self.info_line(width),
        }
    }

    /// The list above the input box: the choice offered, or the commands
    /// matching what is typed. At most `max` rows.
    fn list_view(&self, width: u16, max: usize) -> Vec<Line<'static>> {
        if let Some(chooser) = &self.chooser {
            return chooser.rows(width, max);
        }
        let Some((matches, selected)) = self.command_list() else {
            return Vec::new();
        };
        let names: Vec<String> = matches
            .iter()
            .map(|command| format!("/{}", command.name))
            .collect();
        let entries: Vec<(&str, &str)> = names
            .iter()
            .zip(&matches)
            .map(|(name, command)| (name.as_str(), command.about))
            .collect();
        commands::list_rows(&entries, selected, width, max)
    }

    fn input_view(&self, width: u16, now_ms: i64) -> Bottom {
        // Like the approval box's answers, the rows show dim while Enter does
        // not send; a frame that replaces an approval box starts the guard.
        let guarded = now_ms < self.send_guard_until
            || self
                .shown
                .back()
                .is_some_and(|(_, shown)| shown.approval.is_some());
        let input_width = usize::from(width.saturating_sub(INPUT_CHROME));
        let layout = self.input.layout(input_width);
        let first = layout
            .cursor
            .0
            .saturating_sub(MAX_INPUT_ROWS - 1)
            .min(layout.rows.len().saturating_sub(MAX_INPUT_ROWS));
        let mut rows = Vec::new();
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
            let line = Line::from(truncate(spans, usize::from(width.saturating_sub(2))));
            rows.push(if guarded {
                line.patch_style(Style::new().dim())
            } else {
                line
            });
        }
        let cursor = (
            INPUT_LEFT + layout.cursor.1 as u16,
            1 + (layout.cursor.0 - first) as u16,
        );
        Bottom::Input { rows, cursor }
    }

    fn input_width(&self) -> usize {
        usize::from(self.width.saturating_sub(INPUT_CHROME))
    }

    /// The approval request the box shows: the first one this terminal has
    /// not answered yet.
    fn open_approval(&self) -> Option<(&PendingRequest, &ApprovalRequest)> {
        self.snapshot
            .pending_requests
            .iter()
            .find_map(|request| match &request.kind {
                RequestKind::Approval(approval) if !self.answered.contains(&request.id) => {
                    Some((request, approval))
                }
                _ => None,
            })
    }

    /// Keys pressed on an approval box. They answer only the request that box
    /// showed, and only when its answers were on screen; once that request is
    /// gone, they do nothing. While `guarded`, answer keys are dropped.
    fn approval_key(&mut self, key: KeyEvent, shown: Shown, guarded: bool) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'd') if ctrl => return Some(Action::Quit),
            KeyCode::Esc => return self.interrupt_seen(shown.turn),
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('1'..='9') if guarded => {
                return None;
            }
            _ => {}
        }
        let request_id = shown.approval?;
        if !shown.answers {
            return None;
        }
        let options =
            self.snapshot
                .pending_requests
                .iter()
                .find_map(|request| match &request.kind {
                    RequestKind::Approval(approval)
                        if request.id == request_id && !self.answered.contains(&request.id) =>
                    {
                        Some(approval::options(approval))
                    }
                    _ => None,
                })?;
        if self.approval.request != request_id {
            self.approval = ApprovalCursor {
                request: request_id.clone(),
                ..ApprovalCursor::default()
            };
        }
        let cursor = &mut self.approval;
        let decision = match key.code {
            KeyCode::Up => {
                cursor.selected = cursor.selected.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                cursor.selected = (cursor.selected + 1).min(options.len().saturating_sub(1));
                None
            }
            KeyCode::PageUp => {
                cursor.scroll = cursor.scroll.saturating_sub(self.approval_page);
                None
            }
            KeyCode::PageDown => {
                cursor.scroll = (cursor.scroll + self.approval_page).min(self.approval_max_scroll);
                None
            }
            KeyCode::Enter => options.get(cursor.selected).copied(),
            KeyCode::Char(digit @ '1'..='9') if !ctrl => {
                options.get(digit as usize - '1' as usize).copied()
            }
            _ => None,
        }?;
        self.answered.insert(request_id.clone());
        Some(Action::Resolve {
            request_id,
            decision,
        })
    }

    /// Esc stops the turn the frame showed in progress, if it still is; when
    /// the frame showed nothing in progress it does nothing.
    fn interrupt_seen(&mut self, turn: Option<String>) -> Option<Action> {
        let turn_id = turn?;
        let working = self
            .snapshot
            .turns
            .iter()
            .any(|turn| turn.id == turn_id && turn.status == TurnStatus::InProgress);
        if !working {
            return None;
        }
        self.interrupt(turn_id)
    }

    fn interrupt(&mut self, turn_id: String) -> Option<Action> {
        if self.interrupting.as_ref() == Some(&turn_id) {
            return None;
        }
        self.interrupting = Some(turn_id.clone());
        Some(Action::Interrupt { turn_id })
    }

    /// Who answered an approval, for the conversation.
    fn resolution_notice(
        &self,
        resolution: &ApprovalResolution,
        client: Option<&str>,
    ) -> Option<LogicalLine> {
        let dim = Style::new().dim();
        match resolution {
            ApprovalResolution::Answered { decision } => {
                let label = approval::decision_label(*decision);
                let text = match client {
                    Some(name) if name == self.client_name => format!("You chose: {label}"),
                    Some(name) => format!("{name} answered: {label}"),
                    None => format!("Answered: {label}"),
                };
                let (mark, style) = match decision {
                    ApprovalDecision::Accept | ApprovalDecision::AcceptForSession => {
                        ("✓ ", Style::new().green())
                    }
                    _ => ("✗ ", Style::new().red()),
                };
                Some(cells::notice(mark, &text, style))
            }
            ApprovalResolution::Cancelled => {
                Some(cells::notice("■ ", "Approval request cancelled", dim))
            }
            ApprovalResolution::TimedOut => {
                Some(cells::notice("■ ", "Approval request timed out", dim))
            }
            ApprovalResolution::Unknown => None,
        }
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

    /// The conversation after the history: what is still in progress, then
    /// the queued messages.
    fn live_rows(&self, width: u16) -> Vec<Line<'static>> {
        let end = self.history.end;
        let cwd = &self.snapshot.settings.cwd;
        let waiting: Vec<&str> = self
            .snapshot
            .pending_requests
            .iter()
            .filter_map(|request| match &request.kind {
                RequestKind::Approval(approval) => Some(approval.item_id.as_str()),
                RequestKind::Unknown => None,
            })
            .collect();
        let mut rows = Vec::new();
        for (turn_index, turn) in self.snapshot.turns.iter().enumerate().skip(end.turn) {
            let current = turn_index == end.turn;
            if !current || !end.prompt {
                let from = source(&self.client_name, &turn.client_name);
                rows.extend(cells::prompt(&turn.input, from, width));
            }
            let echo = echo_index(turn);
            let first = if current { end.item } else { 0 };
            for (index, item) in turn.items.iter().enumerate().skip(first) {
                if Some(index) == echo {
                    continue;
                }
                let done = self.item_done(turn, index);
                if !done && item.id().is_some_and(|id| waiting.contains(&id)) {
                    rows.extend(cells::waiting(item, cwd, width));
                } else {
                    rows.extend(cells::item(item, done, cwd, width));
                }
            }
        }
        for queued in &self.snapshot.queued_inputs {
            let mut text = format!("  · Queued: {}", first_line(&queued.input));
            if let Some(from) = source(&self.client_name, &queued.client_name) {
                text += &format!("  (from {})", sanitize(from));
            }
            rows.push(Line::from(truncate(
                vec![Span::styled(text, Style::new().dim())],
                usize::from(width),
            )));
        }
        rows
    }

    fn status_line(&self, now_ms: i64) -> Line<'static> {
        if let Some(reason) = &self.reconnecting {
            return Line::from(vec![
                Span::styled(" ⟳ Reconnecting…", Style::new().yellow()),
                Span::styled(format!(" · {}", sanitize(reason)), Style::new().dim()),
            ]);
        }
        let interrupting = self.interrupting.as_ref().is_some_and(|id| {
            self.snapshot
                .turns
                .iter()
                .any(|turn| turn.id == *id && turn.status == TurnStatus::InProgress)
        });
        if interrupting {
            return Line::from(Span::styled(" ■ Interrupting…", Style::new().yellow()));
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
                    Span::raw("Working"),
                    Span::styled(format!(" · {elapsed}"), Style::new().dim()),
                ])
            }
            SessionStatus::WaitingApproval => Line::from(vec![
                Span::styled(" ⏸ Waiting for approval", Style::new().yellow()),
                Span::styled(format!(" · {elapsed}"), Style::new().dim()),
            ]),
            SessionStatus::Starting => Line::from(Span::styled(" Starting…", Style::new().dim())),
            SessionStatus::Error => {
                let message = self.snapshot.error.as_ref().map_or(String::new(), |error| {
                    format!(": {}", sanitize(&error.message))
                });
                Line::from(Span::styled(
                    format!(" ✗ Error{message}"),
                    Style::new().red(),
                ))
            }
            // Only an explicit `canter --resume` opens it again.
            SessionStatus::Closed => Line::from(Span::styled(
                " ■ Session closed · to resume, quit and run canter --resume",
                Style::new().yellow(),
            )),
            SessionStatus::Idle | SessionStatus::Unknown => Line::default(),
        }
    }

    fn info_line(&self, width: u16) -> Line<'static> {
        let settings = &self.snapshot.settings;
        let mode = match settings.approval_policy {
            PermissionMode::AskForExecution => "Ask for execution",
            PermissionMode::AllowAll => "Allow all",
            // Unknown modes are treated as the strictest one.
            PermissionMode::AlwaysAsk | PermissionMode::Unknown => "Always ask",
        };
        let cwd = shorten_home(&sanitize(&settings.cwd), self.home.as_deref());
        let mut text = format!(
            " {} · think {} · {mode} · {cwd}",
            sanitize(&settings.model),
            settings.reasoning_effort.as_str()
        );
        if let Some(usage) = &self.snapshot.token_usage {
            let percent = |input: u64, cached: u64| {
                (input != 0)
                    .then(|| (u128::from(cached) * 100 + u128::from(input) / 2) / u128::from(input))
            };
            let total = percent(usage.total.input_tokens, usage.total.cached_input_tokens);
            let last = percent(usage.last.input_tokens, usage.last.cached_input_tokens);
            if total.is_some() || last.is_some() {
                text.push_str(" · cache");
                if let Some(total) = total {
                    text.push_str(&format!(" {total}%"));
                }
                if let Some(last) = last {
                    text.push_str(&format!(" (last {last}%)"));
                }
            }
        }
        Line::from(truncate(
            vec![Span::styled(text, Style::new().dim())],
            usize::from(width),
        ))
    }
}

/// The screen's content, laid out for one frame.
pub(crate) struct View {
    /// The conversation rows in view, from the top of the screen.
    conversation: Vec<Line<'static>>,
    /// Says there is new output below while scrolled back.
    hint: Option<Line<'static>>,
    status: Line<'static>,
    /// The command list or a choice, right above the input box.
    list: Vec<Line<'static>>,
    bottom: Bottom,
    info: Line<'static>,
}

/// The input box, or the approval box in its place.
enum Bottom {
    Input {
        rows: Vec<Line<'static>>,
        /// Cursor inside the box, relative to the box's top-left corner.
        cursor: (u16, u16),
    },
    Approval(ApprovalBox),
}

impl Bottom {
    /// Rows inside the border, plus the border.
    fn height(&self) -> u16 {
        let rows = match self {
            Bottom::Input { rows, .. } => rows.len(),
            Bottom::Approval(laid) => laid.rows.len(),
        };
        rows as u16 + 2
    }
}

impl View {
    /// The conversation fills the rows the status line, the box and the info
    /// line (and the hint and the list) leave at the bottom.
    pub fn render(self, frame: &mut Frame) {
        let area = frame.area();
        let list = self.list.len() as u16;
        let below = 1 + list + self.bottom.height() + 1 + u16::from(self.hint.is_some());
        let mut y = area.y;
        let mut row = |height: u16| {
            let rect = Rect::new(area.x, y, area.width, height).intersection(area);
            y += height;
            rect
        };
        let conversation = row(area.height.saturating_sub(below));
        frame.render_widget(Paragraph::new(self.conversation), conversation);
        if let Some(hint) = self.hint {
            frame.render_widget(Paragraph::new(hint), row(1));
        }
        frame.render_widget(Paragraph::new(self.status), row(1));
        frame.render_widget(Paragraph::new(self.list), row(list));
        let boxed = row(self.bottom.height());
        let info = row(1);
        match self.bottom {
            Bottom::Input { rows, cursor } => {
                frame.render_widget(
                    Paragraph::new(rows).block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .border_style(Style::new().dim()),
                    ),
                    boxed,
                );
                let cursor = Position::new(boxed.x + cursor.0, boxed.y + cursor.1);
                if boxed.contains(cursor) {
                    frame.set_cursor_position(cursor);
                }
            }
            Bottom::Approval(laid) => {
                let mut block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().yellow())
                    .title_top(laid.title)
                    .title_bottom(laid.footer);
                if let Some(position) = laid.position {
                    block = block.title_top(position);
                }
                frame.render_widget(Paragraph::new(laid.rows).block(block), boxed);
            }
        }
        frame.render_widget(Paragraph::new(self.info), info);
    }
}

/// The client to name next to what someone sent, when it was not this terminal.
fn source<'a>(own: &str, client: &'a str) -> Option<&'a str> {
    (!client.is_empty() && client != own).then_some(client)
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
