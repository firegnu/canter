//! canter's terminal client, part one: the conversation screen.
//!
//! The screen does not take over the terminal. Finished rows go into the
//! terminal's own scrollback; only the bottom (what is still streaming, the
//! status line, the input box and the info line) is redrawn.
//!
//! - [`client`]: HTTP + SSE over the server's unix socket.
//! - [`fake`]: a server that replays a scripted session, for tests and demos.
//! - [`Ui`]: the screen on any ratatui backend; [`run`] drives it in a real terminal.

mod app;
mod cells;
pub mod client;
pub mod fake;
mod input;
mod markdown;
mod run;
mod terminal;
mod text;

pub use app::{Action, App};
pub use run::{CLIENT_NAME, RunError, RunOptions, run};
pub use terminal::InlineTerminal;

use ratatui::backend::Backend;

/// The conversation screen on a backend: a real terminal or ratatui's `TestBackend`.
pub struct Ui<B: Backend> {
    terminal: InlineTerminal<B>,
    app: App,
}

impl<B: Backend> Ui<B> {
    /// Starts at the backend's cursor row.
    pub fn new(backend: B, app: App) -> Result<Self, B::Error> {
        Ok(Self {
            terminal: InlineTerminal::new(backend)?,
            app,
        })
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

    /// Writes newly finished rows to the scrollback, then redraws the bottom.
    pub fn draw(&mut self, now_ms: i64) -> Result<(), B::Error> {
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
