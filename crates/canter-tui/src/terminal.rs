//! A viewport at the bottom of the normal screen, not the alternate screen.
//!
//! Finished rows are written above the viewport and scroll into the
//! terminal's own scrollback, so its scrolling, copying and searching keep
//! working. Only the viewport is redrawn. The viewport grows and shrinks with
//! its content, which ratatui's own inline viewport cannot do, so this wraps a
//! fixed viewport and moves it.

use ratatui::backend::{Backend, ClearType};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::Line;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

pub struct InlineTerminal<B: Backend> {
    terminal: Terminal<B>,
    /// Where the viewport is now; everything below its top belongs to us.
    area: Rect,
    screen: Size,
}

impl<B: Backend> InlineTerminal<B> {
    /// Starts the viewport on the cursor's row (the next row if the cursor is
    /// mid-line).
    pub fn new(mut backend: B) -> Result<Self, B::Error> {
        let screen = backend.size()?;
        let cursor = backend.get_cursor_position()?;
        let mut top = cursor.y;
        if cursor.x > 0 {
            if top + 1 < screen.height {
                top += 1;
            } else {
                scroll_up(&mut backend, screen.height, 1)?;
            }
        }
        let area = Rect::new(0, top, screen.width, 0);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )?;
        Ok(Self {
            terminal,
            area,
            screen,
        })
    }

    pub fn backend(&self) -> &B {
        self.terminal.backend()
    }

    pub fn backend_mut(&mut self) -> &mut B {
        self.terminal.backend_mut()
    }

    pub fn size(&self) -> Result<Size, B::Error> {
        self.terminal.size()
    }

    /// Writes rows above the viewport. Rows must already fit the width.
    pub fn insert_history(&mut self, rows: &[Line]) -> Result<(), B::Error> {
        if rows.is_empty() {
            return Ok(());
        }
        let screen = self.terminal.size()?;
        let backend = self.terminal.backend_mut();
        // The viewport is redrawn afterwards, so write over it.
        clear_from(backend, self.area.y, screen.height)?;
        let mut y = self.area.y;
        let mut next = 0;
        while next < rows.len() {
            if y >= screen.height {
                let scroll = (rows.len() - next).min(usize::from(screen.height)) as u16;
                scroll_up(backend, screen.height, scroll)?;
                y = screen.height - scroll;
            }
            let count = (rows.len() - next).min(usize::from(screen.height - y));
            let area = Rect::new(0, y, screen.width, count as u16);
            let mut buffer = Buffer::empty(area);
            for (i, row) in rows[next..next + count].iter().enumerate() {
                buffer.set_line(0, y + i as u16, row, screen.width);
            }
            // The rows are blank, so drawing the difference from blank draws
            // the rows and skips cells covered by wide characters.
            backend.draw(Buffer::empty(area).diff(&buffer).into_iter())?;
            next += count;
            y += count as u16;
        }
        self.area.y = y;
        self.place(self.area.height, true)
    }

    /// Resizes the viewport to `height` rows and draws it.
    pub fn draw<F: FnOnce(&mut Frame)>(&mut self, height: u16, render: F) -> Result<(), B::Error> {
        self.place(height, false)?;
        self.terminal.draw(render)?;
        Ok(())
    }

    /// Clears the viewport and leaves the cursor where it started, so the
    /// shell continues right after the history.
    pub fn finish(&mut self) -> Result<(), B::Error> {
        let screen = self.terminal.size()?;
        let backend = self.terminal.backend_mut();
        clear_from(backend, self.area.y, screen.height)?;
        backend.set_cursor_position(Position::new(
            0,
            self.area.y.min(screen.height.saturating_sub(1)),
        ))?;
        backend.show_cursor()?;
        backend.flush()
    }

    /// Keeps the viewport on screen at `height` rows, scrolling history up
    /// into the scrollback when it grows past the bottom.
    fn place(&mut self, height: u16, force: bool) -> Result<(), B::Error> {
        let screen = self.terminal.size()?;
        let height = height.min(screen.height);
        let mut area = Rect::new(0, self.area.y.min(screen.height), screen.width, height);
        if area.bottom() > screen.height {
            let overflow = area.bottom() - screen.height;
            scroll_up(self.terminal.backend_mut(), screen.height, overflow)?;
            area.y -= overflow;
        }
        if force || area != self.area || screen != self.screen {
            clear_from(self.terminal.backend_mut(), area.y, screen.height)?;
            // Clears the viewport and makes the next draw repaint all of it.
            // When the screen got narrower, ratatui clears the whole screen
            // and moves the viewport to the top.
            self.terminal.resize(area)?;
            self.area = self.terminal.get_frame().area();
            self.screen = screen;
        }
        Ok(())
    }
}

fn clear_from<B: Backend>(backend: &mut B, y: u16, screen_height: u16) -> Result<(), B::Error> {
    if y < screen_height {
        backend.set_cursor_position(Position::new(0, y))?;
        backend.clear_region(ClearType::AfterCursor)?;
    }
    Ok(())
}

/// Scrolls the whole screen up; the top rows go into the scrollback.
fn scroll_up<B: Backend>(backend: &mut B, screen_height: u16, rows: u16) -> Result<(), B::Error> {
    backend.set_cursor_position(Position::new(0, screen_height.saturating_sub(1)))?;
    backend.append_lines(rows)
}
