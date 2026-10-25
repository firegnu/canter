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
use std::io::{self, Write};
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
    Event as TermEvent, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::{cursor, execute, queue};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use tokio::sync::mpsc;

use crate::client::{Client, ClientError, EventStream};
use crate::picker::{Pick, Picker};
use crate::{Action, App, Ui, now_ms};

/// The `clientName` prefix of the terminal client.
pub const CLIENT_NAME: &str = "canter-tui";

/// Pause between attempts to reach the server again.
const RETRY: Duration = Duration::from_millis(500);
/// Longest session name taken from a first message, in characters.
const TITLE_CHARS: usize = 80;

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
}

impl<W: Write> FrameSync for CrosstermBackend<W> {
    fn begin_frame(&mut self) -> io::Result<()> {
        queue!(self, BeginSynchronizedUpdate)
    }
    fn end_frame(&mut self) -> io::Result<()> {
        execute!(self, EndSynchronizedUpdate)
    }
}

impl FrameSync for TestBackend {}

/// Runs in the real terminal, taking over the whole screen, until the user
/// quits; the screen is as it was before once this returns. Returns the
/// session's id, or `None` when the user closed the session list without
/// picking one.
pub async fn run<C: Connector>(launch: Launch<C>) -> Result<Option<String>, RunError> {
    let _session = TerminalSession::start()?;
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (sender, input) = input();
    let reader = TerminalReader::spawn(sender);
    let result = run_on(terminal, input, launch).await;
    reader.stop();
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
    let Launch {
        connector,
        connected,
        open,
        env,
        client_name,
        home,
    } = launch;
    let client = connected.client;
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
                    let picker = Picker::new(client.list_sessions().await?, cwd, home.clone());
                    pick(&mut terminal, &mut input, picker).await?
                }
            };
            match id {
                Some(id) => resume(&client, &id, &env, &update).await,
                None => return Ok(None),
            }
        }
    };
    let snapshot = opened?;

    let session = snapshot.id.clone();
    let stream = client.subscribe(&session, snapshot.last_event_seq).await?;
    let mut app = App::new(snapshot, home.clone(), now_ms()).with_client_name(client_name.clone());
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
        sender,
        pump,
    };
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    let result = 'run: loop {
        if let Err(error) = draw(&mut ui, &input) {
            break Err(error);
        }
        let mut next = None;
        let mut quit = tokio::select! {
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
                    if let Some(receiver) = chat.new_session(&mut ui).await {
                        messages = receiver;
                    }
                }
                Some(Action::PickSession) => {
                    let picked = chat.pick_session(&mut ui, &mut input).await;
                    // Not the redraws missed while the list was up, all at once.
                    tick.reset();
                    match picked {
                        Ok(Some(receiver)) => messages = receiver,
                        Ok(None) => {}
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
                Some(Action::ListSkills) => match chat.client.list_skills(&chat.session).await {
                    Ok(skills) => ui.app_mut().show_skills(skills),
                    Err(error) => ui
                        .app_mut()
                        .warning(&format!("Could not list skills: {error}")),
                },
                Some(action) => chat.act(ui.app(), action),
            }
        }
        if quit {
            break Ok(());
        }
    };
    chat.pump.abort();
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

/// Shows the session list until the user picks one (its id) or leaves (`None`).
async fn pick<B>(
    terminal: &mut Terminal<B>,
    input: &mut Input,
    mut picker: Picker,
) -> Result<Option<String>, RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
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
        input.frame_on_screen();
        // The list changes only with its own keys, so every key is for this frame.
        match input.events.recv().await {
            None => return Ok(None),
            Some((_, _, TermEvent::Key(key))) => match picker.handle_key(key) {
                Some(Pick::Open(id)) => return Ok(Some(id)),
                Some(Pick::Cancel) => return Ok(None),
                None => {}
            },
            Some(_) => {}
        }
    }
}

fn draw<B>(ui: &mut Ui<B>, input: &Input) -> Result<(), RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    ui.backend_mut().begin_frame().map_err(terminal_error)?;
    let drawn = ui.render(now_ms());
    ui.backend_mut().end_frame().map_err(terminal_error)?;
    drawn.map_err(terminal_error)?;
    // Only now is the frame on screen; keys that came in while it was drawn
    // were pressed on the one before.
    let frame = input.frame_on_screen();
    ui.app_mut().frame_on_screen(frame, now_ms());
    Ok(())
}

fn terminal_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> RunError {
    RunError::Io(io::Error::other(error))
}

/// What the background tasks tell the screen.
enum Message {
    Sent,
    ImageSendFailed(String),
    Models(Vec<canter_protocol::Model>),
    Server(Box<Event>),
    SendFailed(String),
    NameFailed(String),
    Warning(String),
    AnswerFailed { request_id: String, error: String },
    AnswerLate,
    InterruptFailed(String),
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
            app.handle_mouse(mouse);
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
    /// Background tasks report here; replaced, with the receiver, when the
    /// screen switches sessions, so nothing about the session before shows.
    sender: mpsc::UnboundedSender<Message>,
    /// Forwards the session's events.
    pump: tokio::task::JoinHandle<()>,
}

impl<C: Connector> Chat<C> {
    /// `/new`: a session with the directory, model and permission mode of the
    /// one on screen, shown in its place. The one before stays open on the
    /// server. Returns where the new session's events come in.
    async fn new_session<B: Backend>(
        &mut self,
        ui: &mut Ui<B>,
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
        if matches!(&response, Err(ClientError::Server { code: ErrorCode::InvalidRequest, message, .. })
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
                let receiver = self.show(ui, created.session, failed).await?;
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
    ) -> Result<Option<mpsc::UnboundedReceiver<Message>>, RunError>
    where
        B: FrameSync,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let sessions = match self.client.list_sessions().await {
            Ok(sessions) => sessions,
            Err(error) => {
                ui.app_mut()
                    .warning(&format!("Could not list the sessions: {error}"));
                return Ok(None);
            }
        };
        let cwd = ui.app().snapshot().settings.cwd.clone();
        let picker = Picker::new(sessions, cwd, self.home.clone());
        let id = match pick(ui.terminal_mut(), input, picker).await? {
            Some(id) if id != self.session => id,
            _ => return Ok(None),
        };
        let failed = "Could not resume the session";
        let update = SessionUpdateParams::default();
        Ok(match resume(&self.client, &id, &self.env, &update).await {
            Ok(snapshot) => self.show(ui, snapshot, failed).await,
            Err(error) => {
                ui.app_mut().warning(&format!("{failed}: {error}"));
                None
            }
        })
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
        *ui.app_mut() = App::new(snapshot, self.home.clone(), now_ms())
            .with_client_name(self.client_name.clone());
        if let Ok(models) = self.client.list_models().await {
            ui.app_mut().set_models(&models);
        }
        Some(messages)
    }

    fn message<B: Backend>(&mut self, ui: &mut Ui<B>, message: Message) -> bool {
        let app = ui.app_mut();
        match message {
            Message::Sent => app.sent(),
            Message::ImageSendFailed(error) => app.image_send_failed(&error),
            Message::Models(models) => app.set_models(&models),
            Message::Server(event) => app.apply_event(&event, now_ms()),
            Message::SendFailed(error) => app.send_failed(&error),
            Message::NameFailed(error) => {
                app.warning(&format!("Could not name the session: {error}"))
            }
            Message::Warning(message) => app.warning(&message),
            Message::AnswerFailed { request_id, error } => app.answer_failed(&request_id, &error),
            Message::AnswerLate => app.answer_late(),
            Message::InterruptFailed(error) => app.interrupt_failed(&error),
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

    fn act(&self, app: &App, action: Action) {
        let client = self.client.clone();
        let session = self.session.clone();
        let sender = self.sender.clone();
        match action {
            action @ (Action::Send(_) | Action::SendImages { .. }) => {
                let has_images = matches!(&action, Action::SendImages { .. });
                let (text, input) = match action {
                    Action::Send(text) => (text.clone(), vec![UserInput::Text { text }]),
                    Action::SendImages { text, input } => (text, input),
                    _ => unreachable!(),
                };
                let snapshot = app.snapshot();
                // Name a new session after its first message, for the session list.
                let unnamed = snapshot.settings.name.is_none()
                    && snapshot.turns.is_empty()
                    && snapshot.queued_inputs.is_empty();
                let name = unnamed.then(|| session_title(&text));
                let params = SessionInputParams {
                    input,
                    mode: InputMode::Queue,
                    client_name: self.client_name.clone(),
                };
                tokio::spawn(async move {
                    if let Err(error) = client.send_input(&session, &params).await {
                        let error = if has_images {
                            Message::ImageSendFailed(error.to_string())
                        } else {
                            Message::SendFailed(error.to_string())
                        };
                        let _ = sender.send(error);
                        return;
                    }
                    if has_images {
                        let _ = sender.send(Message::Sent);
                    }
                    if let Some(name) = name {
                        let update = SessionUpdateParams {
                            name: Some(name),
                            ..SessionUpdateParams::default()
                        };
                        if let Err(error) = client.update(&session, &update).await {
                            let _ = sender.send(Message::NameFailed(error.to_string()));
                        }
                    }
                });
            }
            action @ (Action::Resolve { .. } | Action::ResolveUserInput { .. }) => {
                let (request_id, decision, answer) = match action {
                    Action::Resolve {
                        request_id,
                        decision,
                    } => (request_id, Some(decision), None),
                    Action::ResolveUserInput { request_id, answer } => {
                        (request_id, None, Some(answer))
                    }
                    _ => unreachable!(),
                };
                let params = RequestResolveParams {
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
                tokio::spawn(async move {
                    match client.interrupt(&session, &turn_id).await {
                        // The turn ended on its own meanwhile.
                        Ok(_)
                        | Err(ClientError::Server {
                            code: ErrorCode::TurnMismatch,
                            ..
                        }) => {}
                        Err(error) => {
                            let _ = sender.send(Message::InterruptFailed(error.to_string()));
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
            | Action::NewSession
            | Action::PickSession
            | Action::PickModel
            | Action::Usage
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

/// Set while [`TerminalSession`] has the terminal; the first restore clears it.
static TAKEN_OVER: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Raw mode, the alternate screen, mouse reporting (for the wheel), bracketed
/// paste and (when the terminal has it) the keyboard protocol that tells
/// Shift+Enter from Enter. Undone on drop, and on a panic before its message
/// is printed, so the message shows on the normal screen.
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
    fn spawn(sender: InputSender) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => match crossterm::event::read() {
                        // Only the wheel is used; moving the mouse would just redraw.
                        Ok(TermEvent::Mouse(mouse))
                            if !matches!(
                                mouse.kind,
                                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                            ) => {}
                        Ok(event) => {
                            if !sender.send(event) {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        });
        Self { stop, thread }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}
