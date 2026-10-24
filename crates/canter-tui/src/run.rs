//! Runs the conversation screen in the real terminal against a server.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use canter_protocol::{Event, InputMode, SessionInputParams, UserInput};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute, queue};
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::client::{Client, ClientError, EventStream};
use crate::{Action, App, Ui, now_ms};

/// Sent as `clientName` with every input.
pub const CLIENT_NAME: &str = "canter-tui";

pub struct RunOptions {
    /// The server's unix socket.
    pub socket: PathBuf,
    pub session_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("terminal: {0}")]
    Io(#[from] io::Error),
}

enum Message {
    Terminal(TermEvent),
    Server(Box<Event>),
    SendFailed(String),
    Disconnected(String),
}

/// Shows the session until the user quits (Ctrl+C or Ctrl+D on an empty input).
pub async fn run(options: RunOptions) -> Result<(), RunError> {
    let client = Client::new(&options.socket)?;
    let snapshot = client.read_session(&options.session_id).await?.session;
    let stream = client
        .subscribe(&options.session_id, snapshot.last_event_seq)
        .await?;

    let _session = TerminalSession::start()?;
    let app = App::new(snapshot, std::env::var("HOME").ok(), now_ms());
    // Reads the cursor position, so it must come before the input thread starts.
    let mut ui = Ui::new(CrosstermBackend::new(io::stdout()), app)?;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let reader = TerminalReader::spawn(sender.clone());
    let forward = tokio::spawn(forward_events(stream, sender.clone()));
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    let result = loop {
        if let Err(error) = draw(&mut ui) {
            break Err(error);
        }
        let message = tokio::select! {
            message = receiver.recv() => message,
            _ = tick.tick() => continue,
        };
        let Some(message) = message else {
            break Ok(());
        };
        let mut quit = handle(&mut ui, &client, &options.session_id, &sender, message);
        while let Ok(message) = receiver.try_recv() {
            quit |= handle(&mut ui, &client, &options.session_id, &sender, message);
        }
        if quit {
            break Ok(());
        }
    };
    reader.stop();
    forward.abort();
    ui.finish()?;
    Ok(result?)
}

fn draw(ui: &mut Ui<CrosstermBackend<Stdout>>) -> io::Result<()> {
    // One frame, history included, appears at once in terminals that support it.
    queue!(io::stdout(), BeginSynchronizedUpdate)?;
    let result = ui.draw(now_ms());
    execute!(io::stdout(), EndSynchronizedUpdate)?;
    result
}

/// Returns true when the user asked to quit.
fn handle(
    ui: &mut Ui<CrosstermBackend<Stdout>>,
    client: &Client,
    session: &str,
    sender: &mpsc::UnboundedSender<Message>,
    message: Message,
) -> bool {
    let app = ui.app_mut();
    match message {
        Message::Terminal(TermEvent::Key(key)) => match app.handle_key(key) {
            Some(Action::Quit) => return true,
            Some(Action::Send(text)) => {
                let client = client.clone();
                let session = session.to_owned();
                let sender = sender.clone();
                tokio::spawn(async move {
                    let params = SessionInputParams {
                        input: vec![UserInput::Text { text }],
                        mode: InputMode::Queue,
                        client_name: CLIENT_NAME.to_owned(),
                    };
                    if let Err(error) = client.send_input(&session, &params).await {
                        let _ = sender.send(Message::SendFailed(error.to_string()));
                    }
                });
            }
            None => {}
        },
        Message::Terminal(TermEvent::Paste(text)) => app.handle_paste(&text),
        Message::Terminal(_) => {}
        Message::Server(event) => app.apply_event(&event, now_ms()),
        Message::SendFailed(error) => app.send_failed(&error),
        Message::Disconnected(reason) => app.disconnected(&reason),
    }
    false
}

/// Reconnecting is part two's job; for now a broken stream is only reported.
async fn forward_events(mut stream: EventStream, sender: mpsc::UnboundedSender<Message>) {
    let reason = loop {
        match stream.next().await {
            Some(Ok(event)) => {
                if sender.send(Message::Server(Box::new(event))).is_err() {
                    return;
                }
            }
            Some(Err(error)) => break error.to_string(),
            None => break "服务端结束了事件流".to_owned(),
        }
    };
    let _ = sender.send(Message::Disconnected(reason));
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
    fn spawn(sender: mpsc::UnboundedSender<Message>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(event) => {
                            if sender.send(Message::Terminal(event)).is_err() {
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
