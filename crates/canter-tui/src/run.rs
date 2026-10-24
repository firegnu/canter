//! Opens a session (a new one, a given one, or one picked from the list) and
//! runs the conversation screen until the user quits. [`run`] drives the real
//! terminal; [`run_on`] does the same on any backend.
//!
//! When the event stream drops, the screen keeps going and says 「重连中」:
//! it finds the server again through the [`Connector`] and subscribes from the
//! last event it had. Only a new server process gets a `resume` with this
//! terminal's environment first (a restarted server has not opened the
//! session yet). The same server still has the session open, unless another
//! client closed it, and then it stays closed.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use canter_protocol::{
    ErrorCode, Event, InputMode, PermissionMode, RequestResolveParams, SessionCreateParams,
    SessionInputParams, SessionSnapshot, SessionUpdateParams, UserInput,
};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute, queue};
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use tokio::sync::mpsc;

use crate::client::{Client, ClientError, EventStream};
use crate::picker::{Pick, Picker};
use crate::{Action, App, InlineTerminal, Ui, now_ms};

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
    /// the connector cannot tell, and then every reconnect resumes.
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
    #[error("没有这个会话：{0}")]
    NoSession(String),
    #[error("terminal: {0}")]
    Io(#[from] io::Error),
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

/// Runs in the real terminal until the user quits. Returns the session's id,
/// or `None` when the user closed the session list without picking one.
pub async fn run<C: Connector>(launch: Launch<C>) -> Result<Option<String>, RunError> {
    let _session = TerminalSession::start()?;
    // Reads the cursor position, so it must come before the input thread starts.
    let terminal = InlineTerminal::new(CrosstermBackend::new(io::stdout()))?;
    let (sender, keys) = mpsc::unbounded_channel();
    let reader = TerminalReader::spawn(sender);
    let result = run_on(terminal, keys, launch).await;
    reader.stop();
    result
}

/// [`run`] on any backend, with terminal events from `keys`.
pub async fn run_on<B, C>(
    mut terminal: InlineTerminal<B>,
    mut keys: mpsc::UnboundedReceiver<TermEvent>,
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
                    pick(&mut terminal, &mut keys, picker).await?
                }
            };
            match id {
                Some(id) => resume(&client, &id, &env, &update).await,
                None => {
                    terminal.finish().map_err(terminal_error)?;
                    return Ok(None);
                }
            }
        }
    };
    let snapshot = match opened {
        Ok(snapshot) => snapshot,
        Err(error) => {
            terminal.finish().map_err(terminal_error)?;
            return Err(error);
        }
    };

    let session = snapshot.id.clone();
    let stream = client.subscribe(&session, snapshot.last_event_seq).await?;
    let mut app = App::new(snapshot, home, now_ms()).with_client_name(client_name.clone());
    if let Some(warning) = &connected.warning {
        app.warning(warning);
    }
    let mut ui = Ui::with_terminal(terminal, app);
    let (sender, mut messages) = mpsc::unbounded_channel();
    let pump = tokio::spawn(pump(
        connector,
        stream,
        connected.server,
        session.clone(),
        env,
        sender.clone(),
    ));
    let mut chat = Chat {
        client,
        session: session.clone(),
        client_name,
        sender,
    };
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    let result = loop {
        if let Err(error) = draw(&mut ui) {
            break Err(error);
        }
        let mut quit = tokio::select! {
            event = keys.recv() => match event {
                Some(event) => chat.terminal(&mut ui, event),
                None => true,
            },
            Some(message) = messages.recv() => chat.message(&mut ui, message),
            _ = tick.tick() => false,
        };
        while let Ok(message) = messages.try_recv() {
            quit |= chat.message(&mut ui, message);
        }
        while let Ok(event) = keys.try_recv() {
            quit |= chat.terminal(&mut ui, event);
        }
        if quit {
            break Ok(());
        }
    };
    pump.abort();
    ui.finish().map_err(terminal_error)?;
    result.map(|()| Some(session))
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
    terminal: &mut InlineTerminal<B>,
    keys: &mut mpsc::UnboundedReceiver<TermEvent>,
    mut picker: Picker,
) -> Result<Option<String>, RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        let size = terminal.size().map_err(terminal_error)?;
        let view = picker.view(size.width, size.height);
        terminal
            .backend_mut()
            .begin_frame()
            .map_err(terminal_error)?;
        let drawn = terminal.draw(view.height(), |frame| view.render(frame));
        terminal.backend_mut().end_frame().map_err(terminal_error)?;
        drawn.map_err(terminal_error)?;
        match keys.recv().await {
            None => return Ok(None),
            Some(TermEvent::Key(key)) => match picker.handle_key(key) {
                Some(Pick::Open(id)) => return Ok(Some(id)),
                Some(Pick::Cancel) => return Ok(None),
                None => {}
            },
            Some(_) => {}
        }
    }
}

fn draw<B>(ui: &mut Ui<B>) -> Result<(), RunError>
where
    B: FrameSync,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    ui.backend_mut().begin_frame().map_err(terminal_error)?;
    let drawn = ui.draw(now_ms());
    ui.backend_mut().end_frame().map_err(terminal_error)?;
    drawn.map_err(terminal_error)
}

fn terminal_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> RunError {
    RunError::Io(io::Error::other(error))
}

/// What the background tasks tell the screen.
enum Message {
    Server(Box<Event>),
    SendFailed(String),
    NameFailed(String),
    AnswerFailed { request_id: String, error: String },
    AnswerLate,
    InterruptFailed(String),
    Reconnecting(String),
    Reconnected(Connected),
}

/// Carries out what the user asked for, without blocking the screen.
struct Chat {
    client: Client,
    session: String,
    client_name: String,
    sender: mpsc::UnboundedSender<Message>,
}

impl Chat {
    /// Returns true when the user asked to quit.
    fn terminal<B: Backend>(&mut self, ui: &mut Ui<B>, event: TermEvent) -> bool {
        match event {
            TermEvent::Key(key) => match ui.app_mut().handle_key(key) {
                Some(Action::Quit) => true,
                Some(action) => {
                    self.act(ui.app(), action);
                    false
                }
                None => false,
            },
            TermEvent::Paste(text) => {
                ui.app_mut().handle_paste(&text);
                false
            }
            _ => false,
        }
    }

    fn message<B: Backend>(&mut self, ui: &mut Ui<B>, message: Message) -> bool {
        let app = ui.app_mut();
        match message {
            Message::Server(event) => app.apply_event(&event, now_ms()),
            Message::SendFailed(error) => app.send_failed(&error),
            Message::NameFailed(error) => app.warning(&format!("会话没起好名字：{error}")),
            Message::AnswerFailed { request_id, error } => app.answer_failed(&request_id, &error),
            Message::AnswerLate => app.answer_late(),
            Message::InterruptFailed(error) => app.interrupt_failed(&error),
            Message::Reconnecting(reason) => app.reconnecting(&reason),
            Message::Reconnected(connected) => {
                self.client = connected.client;
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
            Action::Send(text) => {
                let snapshot = app.snapshot();
                // Name a new session after its first message, for the session list.
                let unnamed = snapshot.settings.name.is_none()
                    && snapshot.turns.is_empty()
                    && snapshot.queued_inputs.is_empty();
                let name = unnamed.then(|| session_title(&text));
                let params = SessionInputParams {
                    input: vec![UserInput::Text { text }],
                    mode: InputMode::Queue,
                    client_name: self.client_name.clone(),
                };
                tokio::spawn(async move {
                    if let Err(error) = client.send_input(&session, &params).await {
                        let _ = sender.send(Message::SendFailed(error.to_string()));
                        return;
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
            Action::Resolve {
                request_id,
                decision,
            } => {
                let params = RequestResolveParams {
                    decision,
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
            Action::Quit => {}
        }
    }
}

/// Forwards events to the screen. When the stream drops, keeps trying to
/// reach the server again, then subscribes from the last event delivered, so
/// nothing is lost or repeated. `server` is the server process the stream
/// came from.
async fn pump<C: Connector>(
    connector: C,
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
                Some(Err(_)) => break "连接断了".to_owned(),
                None => break "服务端结束了事件流".to_owned(),
            }
        };
        if sender.send(Message::Reconnecting(reason)).is_err() {
            return;
        }
        let cursor = stream.last_seq();
        stream = loop {
            tokio::time::sleep(RETRY).await;
            match reconnect(&connector, server.as_deref(), &session, &env, cursor).await {
                Ok((connected, stream)) => {
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
        client
            .resume(session, env)
            .await
            .map_err(|error| error.to_string())?;
    }
    let stream = client
        .subscribe(session, cursor)
        .await
        .map_err(|error| error.to_string())?;
    Ok((connected, stream))
}

/// Raw mode, bracketed paste and (when the terminal has it) the keyboard
/// protocol that tells Shift+Enter from Enter. Undone on drop, panics included.
struct TerminalSession {
    keyboard_enhanced: bool,
}

impl TerminalSession {
    fn start() -> io::Result<Self> {
        let mut session = Self {
            keyboard_enhanced: false,
        };
        enable_raw_mode()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        if supports_keyboard_enhancement().unwrap_or(false) {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            session.keyboard_enhanced = true;
        }
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.keyboard_enhanced {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout, DisableBracketedPaste, cursor::Show);
        let _ = disable_raw_mode();
    }
}

/// Reads terminal input on its own thread; crossterm's reader blocks.
struct TerminalReader {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl TerminalReader {
    fn spawn(sender: mpsc::UnboundedSender<TermEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(event) => {
                            if sender.send(event).is_err() {
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
