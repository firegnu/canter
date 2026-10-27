//! Opens a session (a new one, a given one, or one picked from the list) and
//! runs the conversation screen until the user quits. [`run`] drives the real
//! terminal; [`run_on`] does the same on any backend.
//!
//! When the event stream drops, the screen keeps going and says 「重连中」:
//! it finds the server again through the [`Connector`] and subscribes from the
//! last event it had. A new server process gets a `resume` with this terminal's
//! environment, unless the snapshot says the session was explicitly closed.
//! The same server still has the session open, unless another client closed
//! it, and then it stays closed.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::ops::ControlFlow;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use canter_protocol::{
    ErrorCode, Event, InputMode, PermissionMode, RequestResolveParams, SessionCloseReason,
    SessionCreateParams, SessionInputParams, SessionSnapshot, SessionStatus, SessionUpdateParams,
    UserInput,
};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::{cursor, execute, queue};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use tokio::sync::{mpsc, watch};

use crate::client::{Client, ClientError, EventStream};
use crate::picker::{Pick, Picker};
use crate::tab_title::{TabState, TabTitle};
use crate::theme::{self, Theme};
use crate::{Action, App, Ui, now_ms};

/// The `clientName` prefix of the terminal client.
pub const CLIENT_NAME: &str = "canter-tui";

/// Pause between attempts to reach the server again.
const RETRY: Duration = Duration::from_millis(500);
/// Longest session name taken from a first message, in characters.
const TITLE_CHARS: usize = 80;

#[cfg(test)]
#[path = "run_retract_tests.rs"]
mod retract_tests;

/// A connection to the server.
pub struct Connected {
    pub client: Client,
    /// Shown to the user, e.g. that the server runs another version.
    pub warning: Option<String>,
    /// Which server process this is (its registered instance). `None` when
    /// the connector cannot tell, and then every reconnect checks the snapshot
    /// to decide whether to resume.
    pub server: Option<String>,
}

/// Finds the server again after the connection dropped.
pub trait Connector: Send + Sync + 'static {
    /// Looks up where the server is now and connects to it.
    fn connect(&self) -> impl Future<Output = Result<Connected, String>> + Send;
}

/// A server on a fixed socket, such as the fake server.
pub struct SocketConnector(pub PathBuf);

impl Connector for SocketConnector {
    async fn connect(&self) -> Result<Connected, String> {
        let client = Client::new(&self.0).map_err(|error| error.to_string())?;
        Ok(Connected {
            client,
            warning: None,
            server: None,
        })
    }
}

/// Which session to show.
pub enum Open {
    /// A new session in `cwd`; `first` is sent as its first message. An empty
    /// model means the server's default.
    New {
        cwd: String,
        model: String,
        approval_policy: PermissionMode,
        first: Option<String>,
    },
    /// An existing session. Without an id the user picks one, starting with
    /// the sessions in `cwd`. `update` is applied after resuming.
    Resume {
        id: Option<String>,
        cwd: String,
        update: SessionUpdateParams,
    },
}

/// Everything [`run`] needs.
pub struct Launch<C> {
    pub connector: C,
    /// The first connection.
    pub connected: Connected,
    pub open: Open,
    /// This terminal's environment, given to a new session and with every resume.
    pub env: BTreeMap<String, String>,
    /// Sent with everything this terminal does; other terminals show it.
    pub client_name: String,
    /// The user's home directory, for showing paths as `~/…`.
    pub home: Option<String>,
    /// The version line the welcome header shows after `canter`, such as
    /// `0.1.0 · 37489fe`.
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("no such session: {0}")]
    NoSession(String),
    #[error("terminal: {0}")]
    Io(#[from] io::Error),
}

/// Terminal events for [`run_on`], from an [`InputSender`]. Each one carries
/// the number of the frame on screen and the time (Unix ms) when it came in,
/// so that a key acts on what the user saw, not on a frame drawn after it
/// was pressed.
pub struct Input {
    events: mpsc::UnboundedReceiver<(u64, i64, TermEvent)>,
    frames: Arc<AtomicU64>,
}

/// Sends terminal events to [`run_on`]; see [`input`].
#[derive(Clone)]
pub struct InputSender {
    events: mpsc::UnboundedSender<(u64, i64, TermEvent)>,
    frames: Arc<AtomicU64>,
}

/// A channel for terminal events.
pub fn input() -> (InputSender, Input) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let frames = Arc::new(AtomicU64::new(0));
    let input = Input {
        events: receiver,
        frames: frames.clone(),
    };
    let sender = InputSender {
        events: sender,
        frames,
    };
    (sender, input)
}

impl InputSender {
    /// Sends `event`, stamped with the frame on screen and the time now.
    /// False once the screen is gone.
    pub fn send(&self, event: TermEvent) -> bool {
        let frame = self.frames.load(Ordering::SeqCst);
        self.events.send((frame, now_ms(), event)).is_ok()
    }

    /// How many frames have been put on screen so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::SeqCst)
    }
}

impl Input {
    /// The frame just drawn is on screen now; returns its number. Events sent
    /// from here on carry it.
    fn frame_on_screen(&self) -> u64 {
        self.frames.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Lets a frame, history included, appear at once in terminals that support it.
pub trait FrameSync: Backend {
    fn begin_frame(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn end_frame(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Writes `sequence` to the terminal as it is, between frames: OSC 52
    /// that puts a copy on the clipboard in tmux or over SSH.
    fn write_sequence(&mut self, _sequence: &str) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<W: Write> FrameSync for CrosstermBackend<W> {
    fn begin_frame(&mut self) -> io::Result<()> {
        queue!(self, BeginSynchronizedUpdate)
    }
    fn end_frame(&mut self) -> io::Result<()> {
        execute!(self, EndSynchronizedUpdate)
    }
    fn write_sequence(&mut self, sequence: &str) -> io::Result<()> {
        Write::write_all(self, sequence.as_bytes())?;
        Write::flush(self)
    }
}

impl FrameSync for TestBackend {}

/// Runs in the real terminal, taking over the whole screen, until the user
/// quits; the screen is as it was before once this returns. Returns the
/// session's id, or `None` when the user closed the session list without
/// picking one.
pub async fn run<C: Connector>(launch: Launch<C>) -> Result<Option<String>, RunError> {
    let _session = TerminalSession::start()?;
    // The terminal answers on its input, so ask before the reader takes it.
    theme::set(Theme::detect());
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (sender, input) = input();
    let reader = TerminalReader::spawn(sender)?;
    let result = run_on(terminal, input, launch).await;
    reader.stop().await;
    result
}

/// [`run`] on any backend, with terminal events from `input`.
pub async fn run_on<B, C>(
    mut terminal: Terminal<B>,
    mut input: Input,
    launch: Launch<C>,
) -> Result<Option<String>, RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
    C: Connector,
{
    let mut shutdown = Shutdown::listen()?;
    let Launch {
        connector,
        connected,
        open,
        env,
        client_name,
        home,
        version,
    } = launch;
    let status_line = crate::statusline::load(&env);
    let client = connected.client;
    let named_first = matches!(&open, Open::New { first: Some(_), .. });
    let mut tab = TabTitle::new(&env);
    // Every way out of here, a title written over the session list included,
    // goes through the clearing below.
    let opened: Result<Option<(SessionSnapshot, EventStream)>, RunError> = async {
        let opened = match open {
            Open::New {
                cwd,
                model,
                approval_policy,
                first,
            } => {
                let params = SessionCreateParams {
                    cwd,
                    env: env.clone(),
                    model,
                    approval_policy,
                    input: first.clone().map(|text| vec![UserInput::Text { text }]),
                    reasoning_effort: None,
                    name: first.as_deref().map(session_title),
                    client_name: client_name.clone(),
                };
                client
                    .create_session(&params)
                    .await
                    .map(|created| created.session)
                    .map_err(RunError::from)
            }
            Open::Resume { id, cwd, update } => {
                let id = match id {
                    Some(id) => Some(id),
                    None => {
                        let dir = crate::tab_title::dir_name(&cwd, home.as_deref());
                        let picker = Picker::new(client.list_sessions().await?, cwd, home.clone());
                        let tab = (&mut tab, dir.as_str());
                        match pick(&mut terminal, &mut input, picker, &mut shutdown, tab).await? {
                            Pick::Open(id) => Some(id),
                            Pick::Cancel | Pick::Exit => return Ok(None),
                        }
                    }
                };
                match id {
                    Some(id) => resume(&client, &id, &env, &update).await,
                    None => return Ok(None),
                }
            }
        };
        let snapshot = opened?;
        let stream = client
            .subscribe(&snapshot.id, snapshot.last_event_seq)
            .await?;
        Ok(Some((snapshot, stream)))
    }
    .await;
    let (snapshot, stream) = match opened {
        Ok(Some(opened)) => opened,
        other => {
            leave_tab(terminal.backend_mut(), &mut tab);
            return other.map(|_| None);
        }
    };

    let session = snapshot.id.clone();
    let mut app = App::new(snapshot, home.clone(), now_ms())
        .with_client_name(client_name.clone())
        .with_welcome(version.clone())
        .with_status_line(status_line.clone(), env.clone());
    match client.list_models().await {
        Ok(models) => app.set_models(&models),
        Err(error) => app.warning(&format!("Could not read image capabilities: {error}")),
    }
    if let Some(warning) = &connected.warning {
        app.warning(warning);
    }
    let mut ui = Ui::with_terminal(terminal, app);
    let connector = Arc::new(connector);
    let (sender, mut messages) = mpsc::unbounded_channel();
    let naming = AutoName::new(ui.app().snapshot(), named_first, sender.clone());
    let pump = tokio::spawn(pump(
        connector.clone(),
        stream,
        connected.server.clone(),
        session.clone(),
        env.clone(),
        sender.clone(),
    ));
    let mut chat = Chat {
        connector,
        client,
        server: connected.server,
        session,
        env,
        client_name,
        home,
        version,
        status_line,
        sender,
        pump,
        btw: None,
        naming,
    };
    let mut every = ui.app().redraw_every(now_ms());
    let mut tick = tokio::time::interval(every);

    let result = 'run: loop {
        if shutdown.requested() {
            break Ok(());
        }
        if let Err(error) = draw(&mut ui, &input, &mut tab) {
            break Err(error);
        }
        // Faster for a moment while a copy's light sweeps, then as before.
        let wanted = ui.app().redraw_every(now_ms());
        if wanted != every {
            every = wanted;
            tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        }
        let mut next = None;
        let mut quit = tokio::select! {
            biased;
            _ = shutdown.wait() => break 'run Ok(()),
            event = input.events.recv() => match event {
                Some(event) => {
                    next = Some(event);
                    false
                }
                None => true,
            },
            Some(message) = messages.recv() => chat.message(&mut ui, message),
            _ = tick.tick() => false,
        };
        while let Ok(message) = messages.try_recv() {
            quit |= chat.message(&mut ui, message);
        }
        while let Some((frame, at, event)) = next.take().or_else(|| input.events.try_recv().ok()) {
            match terminal_event(ui.app_mut(), frame, at, event) {
                None => {}
                Some(Action::Quit) => {
                    quit = true;
                    break;
                }
                Some(Action::NewSession) => {
                    if let Some(receiver) = chat.new_session(&mut ui, None).await {
                        messages = receiver;
                    }
                }
                Some(Action::PickSession) => {
                    let picked = chat
                        .pick_session(&mut ui, &mut input, &mut shutdown, &mut tab)
                        .await;
                    // Not the redraws missed while the list was up, all at once.
                    tick.reset();
                    match picked {
                        Ok(ControlFlow::Break(())) => break 'run Ok(()),
                        Ok(ControlFlow::Continue(Some(receiver))) => messages = receiver,
                        Ok(ControlFlow::Continue(None)) => {}
                        Err(error) => break 'run Err(error),
                    }
                }
                Some(Action::PickModel) => match chat.client.list_models().await {
                    Ok(models) => {
                        ui.app_mut().set_models(&models);
                        crate::commands::choose_model(ui.app_mut(), models);
                    }
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not list models: {error}")),
                },
                Some(Action::Usage) => match chat.client.usage(Some(&chat.session)).await {
                    Ok(report) => ui.app_mut().show_usage(&report),
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not read usage: {error}")),
                },
                Some(Action::Mcp) => match chat.client.mcp().await {
                    Ok(report) => ui.app_mut().show_mcp(&report),
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not read MCP status: {error}")),
                },
                Some(Action::Context) => match chat.client.context(&chat.session).await {
                    Ok(report) => ui.app_mut().show_context(&report),
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not read context: {error}")),
                },
                Some(Action::Files) => ui.app_mut().show_files().await,
                Some(Action::RewindList) => match chat.client.rewind_list(&chat.session).await {
                    Ok(report) => crate::rewind::choose_turn(ui.app_mut(), report),
                    Err(error) => ui.app_mut().warning(&format!("Could not rewind: {error}")),
                },
                Some(Action::RewindMode(turn)) => crate::rewind::choose_mode(ui.app_mut(), turn),
                Some(Action::RewindPreview(params)) => {
                    match chat.client.rewind_preview(&chat.session, &params).await {
                        Ok(report) => crate::rewind::confirm(ui.app_mut(), params, report),
                        Err(error) => ui
                            .app_mut()
                            .warning(&format!("Could not preview rewind: {error}")),
                    }
                }
                Some(Action::Rewind(params)) => {
                    match chat.client.rewind(&chat.session, &params).await {
                        Ok(response) if params.rewind_conversation => {
                            ui.app_mut().rewind_draft(&response.input)
                        }
                        Ok(_) => {}
                        Err(ClientError::Server {
                            code: ErrorCode::Conflict,
                            ..
                        }) => {
                            ui.app_mut()
                                .warning("Changes since preview; review and confirm again.");
                            match chat.client.rewind_preview(&chat.session, &params).await {
                                Ok(report) => crate::rewind::confirm(ui.app_mut(), params, report),
                                Err(error) => ui
                                    .app_mut()
                                    .warning(&format!("Could not preview rewind: {error}")),
                            }
                        }
                        Err(error) => ui.app_mut().warning(&format!("Could not rewind: {error}")),
                    }
                }
                Some(Action::ListSkills) => match chat.client.list_skills(&chat.session).await {
                    Ok(skills) => ui.app_mut().show_skills(skills),
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not list skills: {error}")),
                },
                Some(action) => chat.act(ui.app_mut(), action),
            }
        }
        if quit {
            break Ok(());
        }
        if let Some(note) = ui.app_mut().take_handoff()
            && let Some(receiver) = chat.new_session(&mut ui, Some(note)).await
        {
            messages = receiver;
        }
    };
    chat.pump.abort();
    chat.cancel_btw();
    ui.app_mut().stop_status_line().await;
    leave_tab(ui.backend_mut(), &mut tab);
    result.map(|()| Some(chat.session))
}

/// A session's name from its first message: the first non-empty line, cut
/// to [`TITLE_CHARS`] characters. The session list shows it.
fn session_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut chars = line.chars();
    let title: String = chars.by_ref().take(TITLE_CHARS).collect();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

async fn resume(
    client: &Client,
    id: &str,
    env: &BTreeMap<String, String>,
    update: &SessionUpdateParams,
) -> Result<SessionSnapshot, RunError> {
    let snapshot = match client.resume(id, env).await {
        Ok(resumed) => resumed.session,
        Err(ClientError::Server {
            code: ErrorCode::SessionNotFound,
            ..
        }) => return Err(RunError::NoSession(id.to_owned())),
        Err(error) => return Err(error.into()),
    };
    // The change arrives as a session.updated event after the snapshot.
    if *update != SessionUpdateParams::default() {
        client.update(id, update).await?;
    }
    Ok(snapshot)
}

/// Shows the session list, distinguishing dismissal from terminal shutdown.
/// Meanwhile the tab shows `tab`'s directory, idle.
async fn pick<B>(
    terminal: &mut Terminal<B>,
    input: &mut Input,
    mut picker: Picker,
    shutdown: &mut Shutdown,
    (tab, dir): (&mut TabTitle, &str),
) -> Result<Pick, RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        if shutdown.requested() {
            return Ok(Pick::Exit);
        }
        terminal
            .backend_mut()
            .begin_frame()
            .map_err(terminal_error)?;
        let drawn = terminal
            .draw(|frame| {
                let area = frame.area();
                picker.view(area.width, area.height).render(frame);
            })
            .map(drop);
        terminal.backend_mut().end_frame().map_err(terminal_error)?;
        drawn.map_err(terminal_error)?;
        let title = tab.update(TabState::Idle, dir, now_ms());
        if !title.is_empty() {
            terminal
                .backend_mut()
                .write_sequence(&title)
                .map_err(terminal_error)?;
        }
        input.frame_on_screen();
        // The list changes only with its own keys, so every key is for this frame.
        let event = tokio::select! {
            biased;
            _ = shutdown.wait() => return Ok(Pick::Exit),
            event = input.events.recv() => event,
        };
        match event {
            None => return Ok(Pick::Exit),
            Some((_, _, TermEvent::Key(key))) => {
                if let Some(picked) = picker.handle_key(key) {
                    return Ok(picked);
                }
            }
            Some(_) => {}
        }
    }
}

fn draw<B>(ui: &mut Ui<B>, input: &Input, tab: &mut TabTitle) -> Result<(), RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    ui.backend_mut().begin_frame().map_err(terminal_error)?;
    let drawn = ui.render(now_ms());
    ui.backend_mut().end_frame().map_err(terminal_error)?;
    drawn.map_err(terminal_error)?;
    if let Some(sequence) = ui.app_mut().take_osc52() {
        ui.backend_mut()
            .write_sequence(&sequence)
            .map_err(terminal_error)?;
    }
    let app = ui.app();
    let title = tab.update(app.tab_state(), &app.tab_dir(), now_ms());
    if !title.is_empty() {
        ui.backend_mut()
            .write_sequence(&title)
            .map_err(terminal_error)?;
    }
    // Only now is the frame on screen; keys that came in while it was drawn
    // were pressed on the one before.
    let frame = input.frame_on_screen();
    ui.app_mut().frame_on_screen(frame, now_ms());
    Ok(())
}

fn terminal_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> RunError {
    RunError::Io(io::Error::other(error))
}

/// Empties the tab's title and mark on quitting. The terminal may be gone
/// (SIGHUP), so a failed write is let go.
fn leave_tab<B: FrameSync>(backend: &mut B, tab: &mut TabTitle) {
    let sequence = tab.leave();
    if !sequence.is_empty() {
        let _ = backend.write_sequence(&sequence);
    }
}

/// What the background tasks tell the screen.
enum Message {
    Btw {
        id: usize,
        event: canter_protocol::BtwEvent,
    },
    Sent {
        event_seq: u64,
        turn_id: Option<String>,
        title: String,
        input: Vec<UserInput>,
    },
    ImageSendFailed(String),
    Models(Vec<canter_protocol::Model>),
    Server(Box<Event>),
    SendFailed {
        error: String,
        draft: Option<crate::input::InputBox>,
    },
    NameFailed(String),
    Warning(String),
    /// `/diff` collected the changes.
    Diff(crate::uncommitted::Changes),
    AnswerFailed {
        request_id: String,
        error: String,
    },
    AnswerLate,
    InterruptFailed {
        turn_id: String,
        connection_error: bool,
        message: String,
    },
    Interrupted {
        turn_id: String,
        retracted: bool,
    },
    Reconnecting(String),
    Reconnected(Connected),
}

/// `event` came in at `at` (Unix ms) while frame `frame` was on screen.
/// Returns what the user asked for.
fn terminal_event(app: &mut App, frame: u64, at: i64, event: TermEvent) -> Option<Action> {
    match event {
        TermEvent::Key(key) => app.handle_key_on(key, frame, at),
        TermEvent::Paste(text) => {
            app.handle_paste(&text);
            None
        }
        TermEvent::Mouse(mouse) => {
            app.handle_mouse_on(mouse, frame, at);
            None
        }
        _ => None,
    }
}

/// Carries out what the user asked for, without blocking the screen, except
/// for switching to another session.
struct Chat<C> {
    connector: Arc<C>,
    client: Client,
    /// The server process `client` reaches, as [`Connected::server`].
    server: Option<String>,
    /// The session on screen.
    session: String,
    env: BTreeMap<String, String>,
    client_name: String,
    home: Option<String>,
    /// The welcome header's version line, as [`Launch::version`].
    version: String,
    status_line: Option<String>,
    /// Background tasks report here; replaced, with the receiver, when the
    /// screen switches sessions, so nothing about the session before shows.
    sender: mpsc::UnboundedSender<Message>,
    /// Forwards the session's events.
    pump: tokio::task::JoinHandle<()>,
    btw: Option<(usize, tokio::task::JoinHandle<()>)>,
    naming: AutoName,
}

/// Only titles assigned by this terminal are undone. Serialize title updates
/// so a slow original PATCH cannot overwrite its clear or the next title.
struct AutoName {
    turn: Option<(String, String)>,
    clearing: Option<String>,
    eligible: bool,
    updates: mpsc::UnboundedSender<(Client, String)>,
}

impl AutoName {
    fn new(
        snapshot: &SessionSnapshot,
        named_first: bool,
        messages: mpsc::UnboundedSender<Message>,
    ) -> Self {
        let (updates, mut receive) = mpsc::unbounded_channel::<(Client, String)>();
        let session = snapshot.id.clone();
        tokio::spawn(async move {
            while let Some((client, name)) = receive.recv().await {
                let params = SessionUpdateParams {
                    name: Some(name),
                    ..Default::default()
                };
                if let Err(error) = client.update(&session, &params).await {
                    let _ = messages.send(Message::NameFailed(error.to_string()));
                }
            }
        });
        Self {
            turn: if named_first {
                snapshot
                    .turns
                    .first()
                    .zip(snapshot.settings.name.as_ref())
                    .map(|(turn, name)| (turn.id.clone(), name.clone()))
            } else {
                None
            },
            clearing: None,
            eligible: snapshot.settings.name.is_none()
                && snapshot.turns.is_empty()
                && snapshot.queued_inputs.is_empty(),
            updates,
        }
    }

    fn sent(
        &mut self,
        snapshot: &SessionSnapshot,
        client: &Client,
        seq: u64,
        turn_id: Option<String>,
        title: String,
    ) {
        let Some(turn) = turn_id else {
            return;
        };
        // A delayed input reply may arrive after this turn was retracted.
        if !self.eligible
            || snapshot.turns.first().is_some_and(|first| first.id != turn)
            || (seq <= snapshot.last_event_seq && snapshot.turns.is_empty())
            || snapshot
                .settings
                .name
                .as_ref()
                .is_some_and(|name| Some(name) != self.clearing.as_ref())
        {
            return;
        }
        self.eligible = false;
        self.turn = Some((turn, title.clone()));
        self.clearing = None;
        let _ = self.updates.send((client.clone(), title));
    }

    fn retracted(&mut self, turn: &str, snapshot: &SessionSnapshot, client: &Client) {
        if self.turn.as_ref().is_some_and(|(id, _)| id == turn) {
            let (_, title) = self.turn.take().expect("named turn");
            // Do not clear a name explicitly replaced by another client.
            self.eligible = snapshot
                .settings
                .name
                .as_ref()
                .is_none_or(|name| *name == title);
            if self.eligible {
                self.clearing = Some(title);
                let _ = self.updates.send((client.clone(), String::new()));
            }
        }
    }
}

impl<C: Connector> Chat<C> {
    /// `/new`: a session with the directory, model and permission mode of the
    /// one on screen, shown in its place. The one before stays open on the
    /// server. Returns where the new session's events come in.
    async fn new_session<B: Backend>(
        &mut self,
        ui: &mut Ui<B>,
        handoff: Option<String>,
    ) -> Option<mpsc::UnboundedReceiver<Message>> {
        let settings = &ui.app().snapshot().settings;
        let mut params = SessionCreateParams {
            cwd: settings.cwd.clone(),
            env: self.env.clone(),
            model: settings.model.clone(),
            approval_policy: settings.approval_policy,
            reasoning_effort: (settings.reasoning_effort
                != canter_protocol::ReasoningEffort::Unknown)
                .then_some(settings.reasoning_effort),
            input: None,
            name: None,
            client_name: self.client_name.clone(),
        };
        let failed = "Could not start a new session";
        let mut response = self.client.create_session(&params).await;
        let mut previous_model = None;
        if handoff.is_none()
            && matches!(&response, Err(ClientError::Server { code: ErrorCode::InvalidRequest, message, .. })
            if message == "model is not in the configured model list")
        {
            previous_model = Some(std::mem::take(&mut params.model));
            response = self.client.create_session(&params).await;
        }
        match response {
            Ok(created) => {
                let notice = previous_model.map(|model| {
                    format!(
                        "Model {model} is no longer available; new session uses default model {}.",
                        created.session.settings.model
                    )
                });
                let source = self.session.clone();
                let receiver = self.show(ui, created.session, failed).await?;
                if let Some(note) = handoff {
                    ui.app_mut().handoff_draft(&source, &note);
                }
                if let Some(notice) = notice {
                    ui.app_mut().warning(&notice);
                }
                Some(receiver)
            }
            Err(error) => {
                ui.app_mut().warning(&format!("{failed}: {error}"));
                None
            }
        }
    }

    /// `/resume`: the session list in the screen's place; the session picked
    /// is shown instead of the one before, which stays open on the server.
    /// Esc goes back to the session on screen. Returns where the picked
    /// session's events come in.
    async fn pick_session<B>(
        &mut self,
        ui: &mut Ui<B>,
        input: &mut Input,
        shutdown: &mut Shutdown,
        tab: &mut TabTitle,
    ) -> Result<ControlFlow<(), Option<mpsc::UnboundedReceiver<Message>>>, RunError>
    where
        B: FrameSync,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let sessions = match self.client.list_sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                ui.app_mut()
                    .warning(&format!("Could not list the sessions: {error}"));
                return Ok(ControlFlow::Continue(None));
            }
        };
        let cwd = ui.app().snapshot().settings.cwd.clone();
        let picker = Picker::new(sessions, cwd, self.home.clone());
        let dir = ui.app().tab_dir();
        let tab = (tab, dir.as_str());
        let id = match pick(ui.terminal_mut(), input, picker, shutdown, tab).await? {
            Pick::Open(id) if id != self.session => id,
            Pick::Exit => return Ok(ControlFlow::Break(())),
            _ => return Ok(ControlFlow::Continue(None)),
        };
        let failed = "Could not resume the session";
        let update = SessionUpdateParams::default();
        Ok(ControlFlow::Continue(
            match resume(&self.client, &id, &self.env, &update).await {
                Ok(snapshot) => self.show(ui, snapshot, failed).await,
                Err(error) => {
                    ui.app_mut().warning(&format!("{failed}: {error}"));
                    None
                }
            },
        ))
    }

    /// Shows `snapshot`'s session in place of the one on screen, following
    /// its events from the snapshot on. When that fails, the screen stays
    /// and says why after `failed`.
    async fn show<B: Backend>(
        &mut self,
        ui: &mut Ui<B>,
        snapshot: SessionSnapshot,
        failed: &str,
    ) -> Option<mpsc::UnboundedReceiver<Message>> {
        let stream = match self
            .client
            .subscribe(&snapshot.id, snapshot.last_event_seq)
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                ui.app_mut().warning(&format!("{failed}: {error}"));
                return None;
            }
        };
        let (sender, messages) = mpsc::unbounded_channel();
        self.pump.abort();
        self.cancel_btw();
        self.pump = tokio::spawn(pump(
            self.connector.clone(),
            stream,
            self.server.clone(),
            snapshot.id.clone(),
            self.env.clone(),
            sender.clone(),
        ));
        self.sender = sender;
        self.session = snapshot.id.clone();
        self.naming = AutoName::new(&snapshot, false, self.sender.clone());
        let app = App::new(snapshot, self.home.clone(), now_ms())
            .with_client_name(self.client_name.clone())
            .with_welcome(self.version.clone())
            .with_status_line(self.status_line.clone(), self.env.clone())
            .with_bar_from(ui.app());
        *ui.app_mut() = app;
        if let Ok(models) = self.client.list_models().await {
            ui.app_mut().set_models(&models);
        }
        Some(messages)
    }

    fn message<B: Backend>(&mut self, ui: &mut Ui<B>, message: Message) -> bool {
        let app = ui.app_mut();
        match message {
            Message::Btw { id, event } => {
                if matches!(
                    event,
                    canter_protocol::BtwEvent::Completed
                        | canter_protocol::BtwEvent::Cancelled
                        | canter_protocol::BtwEvent::Error { .. }
                ) && self.btw.as_ref().is_some_and(|(active, _)| *active == id)
                {
                    self.btw.take();
                }
                app.btw_event(id, event);
            }
            Message::Sent {
                event_seq,
                turn_id,
                title,
                input,
            } => {
                self.naming
                    .sent(app.snapshot(), &self.client, event_seq, turn_id, title);
                app.input_sent(event_seq, input);
            }
            Message::ImageSendFailed(error) => app.image_send_failed(&error),
            Message::Models(models) => app.set_models(&models),
            Message::Server(event) => {
                if let canter_protocol::EventPayload::TurnRetracted { turn_id } = &event.payload {
                    self.naming.retracted(turn_id, app.snapshot(), &self.client);
                }
                app.apply_event(&event, now_ms());
            }
            Message::SendFailed { error, draft } => app.text_send_failed(&error, draft),
            Message::NameFailed(error) => {
                app.warning(&format!("Could not name the session: {error}"))
            }
            Message::Warning(message) => app.warning(&message),
            Message::Diff(changes) => app.show_diff(changes),
            Message::AnswerFailed { request_id, error } => app.answer_failed(&request_id, &error),
            Message::AnswerLate => app.answer_late(),
            Message::InterruptFailed {
                turn_id,
                connection_error,
                message,
            } => app.interrupt_failed(&turn_id, connection_error, &message),
            Message::Interrupted { turn_id, retracted } => {
                app.interrupt_finished(&turn_id, retracted)
            }
            Message::Reconnecting(reason) => app.reconnecting(&reason),
            Message::Reconnected(connected) => {
                self.client = connected.client;
                self.server = connected.server;
                app.reconnected();
                if let Some(warning) = connected.warning {
                    app.warning(&warning);
                }
            }
        }
        false
    }

    fn cancel_btw(&mut self) {
        if let Some((_, task)) = self.btw.take() {
            task.abort();
        }
    }

    fn act(&mut self, app: &mut App, action: Action) {
        let client = self.client.clone();
        let session = self.session.clone();
        let sender = self.sender.clone();
        match action {
            Action::Diff => {
                let cwd = app.snapshot().settings.cwd.clone();
                tokio::spawn(async move {
                    let changes = crate::uncommitted::collect(&cwd).await;
                    let _ = sender.send(Message::Diff(changes));
                });
            }
            Action::Review(target) => {
                let cwd = app.snapshot().settings.cwd.clone();
                let env = self.env.clone();
                let client_name = self.client_name.clone();
                tokio::spawn(async move {
                    let result = async {
                        let info = client.info().await.map_err(|e| e.to_string())?;
                        if !info.features.as_ref().is_some_and(|features| features.iter().any(|feature| feature == "review")) {
                            return Err("The running server cannot enforce read-only review; run canter serve --replace and try again".into());
                        }
                        let params =
                            crate::review::prepare(&cwd, &env, &target, &client_name).await?;
                        client
                            .send_input(&session, &params)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>(())
                    }
                    .await;
                    if let Err(error) = result {
                        let _ = sender.send(Message::Warning(error));
                    }
                });
            }
            action @ (Action::Btw { .. } | Action::Handoff { .. }) => {
                let (id, question, handoff) = match action {
                    Action::Btw { id, question } => (id, question, false),
                    Action::Handoff { id, direction } => (id, direction, true),
                    _ => unreachable!(),
                };
                let task = tokio::spawn(async move {
                    use canter_protocol::BtwEvent;
                    let result: Result<(), ClientError> = async {
                        let mut stream = if handoff {
                            client.handoff(&session, question).await?
                        } else {
                            client.btw(&session, question).await?
                        };
                        while let Some(event) = stream.next().await {
                            let event = event?;
                            let done = matches!(
                                event,
                                BtwEvent::Completed | BtwEvent::Cancelled | BtwEvent::Error { .. }
                            );
                            if sender.send(Message::Btw { id, event }).is_err() || done {
                                return Ok(());
                            }
                        }
                        Err(ClientError::Protocol(
                            if handoff {
                                "handoff connection ended before the note finished"
                            } else {
                                "btw connection ended before the answer finished"
                            }
                            .into(),
                        ))
                    }
                    .await;
                    if let Err(error) = result {
                        let _ = sender.send(Message::Btw {
                            id,
                            event: BtwEvent::Error {
                                message: error.to_string(),
                            },
                        });
                    }
                });
                self.btw = Some((id, task));
            }
            Action::CancelBtw { id } => {
                if self.btw.as_ref().is_some_and(|(active, _)| *active == id) {
                    self.cancel_btw();
                }
            }
            action @ (Action::Send(_) | Action::SendImages { .. }) => {
                let has_images = matches!(&action, Action::SendImages { .. });
                let draft = if has_images {
                    None
                } else {
                    app.take_text_draft()
                };
                let (text, input) = match action {
                    Action::Send(text) => (text.clone(), vec![UserInput::Text { text }]),
                    Action::SendImages { text, input } => (text, input),
                    _ => unreachable!(),
                };
                let title = session_title(&text);
                let params = SessionInputParams {
                    review: None,
                    input,
                    mode: InputMode::Queue,
                    client_name: self.client_name.clone(),
                };
                app.input_sending();
                tokio::spawn(async move {
                    let response = match client.send_input(&session, &params).await {
                        Ok(response) => response,
                        Err(error) => {
                            let error = if has_images {
                                Message::ImageSendFailed(error.to_string())
                            } else {
                                Message::SendFailed {
                                    error: error.to_string(),
                                    draft,
                                }
                            };
                            let _ = sender.send(error);
                            return;
                        }
                    };
                    let _ = sender.send(Message::Sent {
                        event_seq: response.event_seq,
                        turn_id: response.turn_id,
                        title,
                        input: params.input,
                    });
                });
            }
            action @ (Action::Resolve { .. } | Action::ResolveUserInput { .. }) => {
                let (request_id, decision, feedback, answer) = match action {
                    Action::Resolve {
                        request_id,
                        decision,
                        feedback,
                    } => (request_id, Some(decision), feedback, None),
                    Action::ResolveUserInput { request_id, answer } => {
                        (request_id, None, None, Some(answer))
                    }
                    _ => unreachable!(),
                };
                let params = RequestResolveParams {
                    feedback,
                    decision,
                    answer,
                    client_name: self.client_name.clone(),
                };
                tokio::spawn(async move {
                    let message = match client.resolve(&session, &request_id, &params).await {
                        Ok(response) if response.already_resolved => Message::AnswerLate,
                        Ok(_) => return,
                        Err(error) => Message::AnswerFailed {
                            request_id,
                            error: error.to_string(),
                        },
                    };
                    let _ = sender.send(message);
                });
            }
            Action::Interrupt { turn_id } => {
                let retract = app
                    .snapshot()
                    .turns
                    .iter()
                    .any(|turn| turn.id == turn_id && turn.review.is_none());
                tokio::spawn(async move {
                    match client.interrupt(&session, &turn_id, retract).await {
                        Ok(response) => {
                            let _ = sender.send(Message::Interrupted {
                                turn_id,
                                retracted: response.retracted == Some(true),
                            });
                        }
                        // The turn ended on its own meanwhile.
                        Err(ClientError::Server {
                            code: ErrorCode::TurnMismatch,
                            ..
                        }) => {
                            let _ = sender.send(Message::Interrupted {
                                turn_id,
                                retracted: false,
                            });
                        }
                        Err(error) => {
                            let _ = sender.send(Message::InterruptFailed {
                                turn_id,
                                connection_error: matches!(error, ClientError::Http(_)),
                                message: error.to_string(),
                            });
                        }
                    }
                });
            }
            Action::Compact => {
                tokio::spawn(async move {
                    if let Err(error) = client.compact(&session).await {
                        let _ =
                            sender.send(Message::Warning(format!("Could not compact: {error}")));
                    }
                });
            }
            Action::Update(params) => {
                tokio::spawn(async move {
                    if let Err(error) = client.update(&session, &params).await {
                        let _ = sender.send(Message::Warning(format!(
                            "Could not change the session: {error}"
                        )));
                    }
                });
            }
            // The main loop handles these itself.
            Action::Quit
            | Action::RewindList
            | Action::RewindMode(_)
            | Action::RewindPreview(_)
            | Action::Rewind(_)
            | Action::NewSession
            | Action::PickSession
            | Action::PickModel
            | Action::Usage
            | Action::Context
            | Action::Mcp
            | Action::Files
            | Action::ListSkills => {}
        }
    }
}

/// Forwards events to the screen. When the stream drops, keeps trying to
/// reach the server again, then subscribes from the last event delivered, so
/// nothing is lost or repeated. `server` is the server process the stream
/// came from.
async fn pump<C: Connector>(
    connector: Arc<C>,
    mut stream: EventStream,
    mut server: Option<String>,
    session: String,
    env: BTreeMap<String, String>,
    sender: mpsc::UnboundedSender<Message>,
) {
    loop {
        let reason = loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    if sender.send(Message::Server(Box::new(event))).is_err() {
                        return;
                    }
                }
                Some(Err(_)) => break "connection lost".to_owned(),
                None => break "server ended the event stream".to_owned(),
            }
        };
        if sender.send(Message::Reconnecting(reason)).is_err() {
            return;
        }
        let cursor = stream.last_seq();
        stream = loop {
            tokio::time::sleep(RETRY).await;
            match reconnect(&*connector, server.as_deref(), &session, &env, cursor).await {
                Ok((connected, stream)) => {
                    if let Ok(models) = connected.client.list_models().await {
                        let _ = sender.send(Message::Models(models));
                    }
                    server.clone_from(&connected.server);
                    if sender.send(Message::Reconnected(connected)).is_err() {
                        return;
                    }
                    break stream;
                }
                Err(reason) => {
                    if sender.send(Message::Reconnecting(reason)).is_err() {
                        return;
                    }
                }
            }
        };
    }
}

/// Connects again; `previous` is the server process the last stream came from.
async fn reconnect<C: Connector>(
    connector: &C,
    previous: Option<&str>,
    session: &str,
    env: &BTreeMap<String, String>,
    cursor: u64,
) -> Result<(Connected, EventStream), String> {
    let connected = connector.connect().await?;
    let client = &connected.client;
    // Resuming on the same server would open a session someone closed.
    let same_server = connected.server.is_some() && connected.server.as_deref() == previous;
    if !same_server {
        let snapshot = client
            .read_session(session)
            .await
            .map_err(|error| error.to_string())?
            .session;
        // Old data has no reason. Keep its restart behavior, and also recover
        // non-closed sessions left behind by a crashed server.
        if snapshot.status != SessionStatus::Closed
            || matches!(
                snapshot.close_reason,
                None | Some(SessionCloseReason::ServerShutdown)
            )
        {
            client
                .resume(session, env)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    let stream = client
        .subscribe(session, cursor)
        .await
        .map_err(|error| error.to_string())?;
    Ok((connected, stream))
}

/// One signal listener for both the conversation and the nested session list.
/// The flag stays set, so returning from a list cannot consume the shutdown.
struct Shutdown {
    requested: watch::Receiver<bool>,
    listener: tokio::task::JoinHandle<()>,
}

impl Shutdown {
    fn listen() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let mut hangup = signal(SignalKind::hangup())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let (sender, requested) = watch::channel(false);
        let listener = tokio::spawn(async move {
            tokio::select! {
                _ = hangup.recv() => {}
                _ = terminate.recv() => {}
            }
            sender.send_replace(true);
        });
        Ok(Self {
            requested,
            listener,
        })
    }

    fn requested(&self) -> bool {
        *self.requested.borrow()
    }

    async fn wait(&mut self) {
        let _ = self.requested.wait_for(|requested| *requested).await;
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

/// Set while [`TerminalSession`] has the terminal; the first restore clears it.
static TAKEN_OVER: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Raw mode, the alternate screen, mouse reporting (for the wheel and for
/// selecting), bracketed paste and (when the terminal has it) the keyboard
/// protocol that tells Shift+Enter from Enter. Undone on drop, and on a
/// panic before its message is printed, so the message shows on the normal
/// screen.
struct TerminalSession;

impl TerminalSession {
    fn start() -> io::Result<Self> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
        TAKEN_OVER.store(true, Ordering::SeqCst);
        // Dropped, and so undone, if a step below fails.
        let session = Self;
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        // The alternate screen keeps its own keyboard modes, so this comes after entering it.
        if supports_keyboard_enhancement().unwrap_or(false) {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            KEYBOARD_ENHANCED.store(true, Ordering::SeqCst);
        }
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Puts the terminal back as it was before [`TerminalSession::start`], once.
fn restore_terminal() {
    if !TAKEN_OVER.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut stdout = io::stdout();
    if KEYBOARD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = disable_raw_mode();
}

/// Reads terminal input on its own thread; crossterm's reader blocks.
struct TerminalReader {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl TerminalReader {
    fn spawn(sender: InputSender) -> io::Result<Self> {
        // Match crossterm's input source, including redirected stdin.
        let tty = if io::stdin().is_terminal() {
            io::stdin().as_fd().try_clone_to_owned()?
        } else {
            std::fs::File::open("/dev/tty")?.into()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut wait_ms = 0;
            while !flag.load(Ordering::Relaxed) {
                // crossterm 0.29's mio reader loops on EOF/EIO. Wait outside
                // it so a hangup closes the input channel instead of spinning.
                if !terminal_input_open(tty.as_fd(), wait_ms).unwrap_or(false) {
                    return;
                }
                wait_ms = 0;
                match crossterm::event::poll(Duration::ZERO) {
                    Ok(true) => match crossterm::event::read() {
                        // Only the wheel and the left button are used (scrolling,
                        // selecting); moving the mouse alone would just redraw.
                        Ok(TermEvent::Mouse(mouse))
                            if !matches!(
                                mouse.kind,
                                MouseEventKind::ScrollUp
                                    | MouseEventKind::ScrollDown
                                    | MouseEventKind::Down(MouseButton::Left)
                                    | MouseEventKind::Drag(MouseButton::Left)
                                    | MouseEventKind::Up(MouseButton::Left)
                            ) => {}
                        Ok(event) => {
                            if !sender.send(event) {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    // Drain crossterm's buffered events before waiting again;
                    // polling it even on a timeout also delivers SIGWINCH.
                    Ok(false) => wait_ms = 50,
                    Err(_) => return,
                }
            }
        });
        Ok(Self { stop, thread })
    }

    async fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        // Normally the 50 ms native poll returns and the thread stops. Keep
        // the bound for a hangup racing crossterm's nonblocking event read;
        // command cleanup must not be held open by the dependency's reader.
        let _ = tokio::time::timeout(Duration::from_millis(100), async {
            while !self.thread.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        if self.thread.is_finished() {
            let _ = self.thread.join();
        }
    }
}

/// A tty's EOF is reported as POLLHUP (on Linux it can also report POLLERR).
fn terminal_input_open(tty: BorrowedFd<'_>, wait_ms: i32) -> io::Result<bool> {
    let mut poll = nix::libc::pollfd {
        fd: tty.as_raw_fd(),
        events: nix::libc::POLLIN,
        revents: 0,
    };
    // `tty` remains owned by the reader for the duration of this call.
    if unsafe { nix::libc::poll(&mut poll, 1, wait_ms) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(poll.revents & (nix::libc::POLLHUP | nix::libc::POLLERR | nix::libc::POLLNVAL) == 0)
}
