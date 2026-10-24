//! canter's terminal client: the conversation screen, the approval box, the
//! session list, and reconnecting after the server restarts.
//!
//! The screen does not take over the terminal. Finished rows go into the
//! terminal's own scrollback; only the bottom (what is still streaming, the
//! status line, the input or approval box and the info line) is redrawn.
//!
//! - [`client`]: HTTP + SSE over the server's unix socket.
//! - [`fake`]: a server that replays a scripted session, for tests and demos.
//! - [`Ui`]: the screen on any ratatui backend; [`run`] opens a session and
//!   drives the screen in a real terminal, [`run_on`] on any backend.

mod app;
mod approval;
mod cells;
pub mod client;
pub mod fake;
mod input;
mod markdown;
mod picker;
mod run;
mod terminal;
mod text;

pub use app::{ANSWER_GUARD_MS, Action, App};
pub use run::{
    CLIENT_NAME, Connected, Connector, FrameSync, Input, InputSender, Launch, Open, RunError,
    SocketConnector, input, run, run_on,
};
pub use terminal::InlineTerminal;

use ratatui::backend::Backend;

/// The conversation screen on a backend: a real terminal or ratatui's `TestBackend`.
pub struct Ui<B: Backend> {
    terminal: InlineTerminal<B>,
    app: App,
    /// Frames [`Ui::draw`] has put on screen.
    frames: u64,
}

impl<B: Backend> Ui<B> {
    /// Starts at the backend's cursor row.
    pub fn new(backend: B, app: App) -> Result<Self, B::Error> {
        Ok(Self::with_terminal(InlineTerminal::new(backend)?, app))
    }

    /// Continues on a terminal that already drew something (the session list).
    pub fn with_terminal(terminal: InlineTerminal<B>, app: App) -> Self {
        Self {
            terminal,
            app,
            frames: 0,
        }
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn backend(&self) -> &B {
        self.terminal.backend()
    }

    pub fn backend_mut(&mut self) -> &mut B {
        self.terminal.backend_mut()
    }

    /// Writes newly finished rows to the scrollback, then redraws the bottom,
    /// on a backend that shows what it draws at once (such as `TestBackend`).
    /// [`App::handle_key`] then acts on this frame.
    pub fn draw(&mut self, now_ms: i64) -> Result<(), B::Error> {
        self.render(now_ms)?;
        self.frames += 1;
        self.app.frame_on_screen(self.frames, now_ms);
        Ok(())
    }

    /// Draws a frame without counting it as on screen yet: with synchronized
    /// output the terminal shows it only when the frame ends.
    pub(crate) fn render(&mut self, now_ms: i64) -> Result<(), B::Error> {
        let size = self.terminal.size()?;
        let rows = self.app.take_history(size.width);
        self.terminal.insert_history(&rows)?;
        let view = self.app.view(size.width, size.height, now_ms);
        self.terminal
            .draw(view.height(), |frame| view.render(frame))
    }

    /// Clears the bottom area and leaves the cursor below the history.
    pub fn finish(&mut self) -> Result<(), B::Error> {
        self.terminal.finish()
    }
}

/// Unix time in milliseconds, the protocol's clock.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
