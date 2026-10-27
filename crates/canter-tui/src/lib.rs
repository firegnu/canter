//! canter's terminal client: the conversation screen, the approval box, the
//! session list, and reconnecting after the server restarts.
//!
//! The screen takes over the whole terminal (the alternate screen): the
//! conversation from the top, scrolled back with the mouse wheel and
//! PgUp/PgDn; the status line, the input or approval box and the two info rows
//! at the bottom.
//!
//! - [`client`]: HTTP + SSE over the server's unix socket.
//! - [`fake`]: a server that replays a scripted session, for tests and demos.
//! - [`theme`]: colours by purpose. [`run`] works out the terminal's colour depth
//!   and background at start; tests set them with [`theme::set`].
//! - [`Ui`]: the screen on any ratatui backend; [`run`] opens a session and
//!   drives the screen in a real terminal, [`run_on`] on any backend.

mod app;
mod approval;
mod btw;
mod cells;
pub mod client;
mod commands;
mod context;
mod copy;
mod diff;
mod editor;
mod export;
pub mod fake;
mod files;
mod git;
mod help;
mod highlight;
mod images;
mod input;
mod input_history;
mod links;
mod markdown;
mod mention;
mod picker;
mod question;
mod report;
mod retract;
pub mod review;
mod rewind;
mod run;
mod search;
mod select;
mod shimmer;
mod short_path;
mod statusline;
mod tab_title;
mod text;
pub mod theme;
mod theme_picker;
mod tui_json;
pub mod uncommitted;
pub mod usage;
mod welcome;

pub use app::{ANSWER_GUARD_MS, Action, App};
pub use export::Export;
pub use links::LinkSystem;
pub use run::{
    CLIENT_NAME, Connected, Connector, FrameSync, Input, InputSender, Launch, Open, RunError,
    SocketConnector, input, run, run_on,
};

use ratatui::Terminal;
use ratatui::backend::Backend;

/// The conversation screen on a backend: a real terminal or ratatui's `TestBackend`.
pub struct Ui<B: Backend> {
    terminal: Terminal<B>,
    app: App,
    /// Frames [`Ui::draw`] has put on screen.
    frames: u64,
}

impl<B: Backend> Ui<B> {
    pub fn new(backend: B, app: App) -> Result<Self, B::Error> {
        Ok(Self::with_terminal(Terminal::new(backend)?, app))
    }

    /// Continues on a terminal that already drew something (the session list).
    pub fn with_terminal(terminal: Terminal<B>, app: App) -> Self {
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

    /// For drawing something else in the screen's place, such as the session list.
    pub(crate) fn terminal_mut(&mut self) -> &mut Terminal<B> {
        &mut self.terminal
    }

    /// Redraws the screen on a backend that shows what it draws at once
    /// (such as `TestBackend`). [`App::handle_key`] then acts on this frame.
    pub fn draw(&mut self, now_ms: i64) -> Result<(), B::Error> {
        self.render(now_ms)?;
        self.frames += 1;
        self.app.frame_on_screen(self.frames, now_ms);
        Ok(())
    }

    /// Draws a frame without counting it as on screen yet: with synchronized
    /// output the terminal shows it only when the frame ends.
    pub(crate) fn render(&mut self, now_ms: i64) -> Result<(), B::Error> {
        self.app.update_status_line(now_ms);
        let app = &mut self.app;
        self.terminal.draw(|frame| {
            let area = frame.area();
            app.view(area.width, area.height, now_ms).render(frame);
        })?;
        Ok(())
    }
}

/// Unix time in milliseconds, the protocol's clock.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
