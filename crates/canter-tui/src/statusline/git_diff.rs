//! The lines changed since the last commit, after the branch on the status
//! bar: counted by git in the background, never waited for while drawing.

use std::time::Duration;

use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

/// Counted again this often when nothing else asks for it, to catch changes
/// made in an editor.
const EVERY: Duration = Duration::from_secs(5);

/// Lines added and removed.
type Lines = (u64, u64);

pub(crate) struct GitDiff {
    /// The directory [`GitDiff::lines`] is for.
    cwd: Option<String>,
    lines: Option<Lines>,
    wanted: bool,
    started: Option<Instant>,
    running: Option<(oneshot::Receiver<Option<Lines>>, JoinHandle<()>)>,
}

impl GitDiff {
    /// Counts when first polled.
    pub fn new() -> Self {
        Self {
            cwd: None,
            lines: None,
            wanted: true,
            started: None,
            running: None,
        }
    }

    /// Lines added and removed; `None` outside a repository, when git
    /// failed or before the first count.
    pub fn lines(&self) -> Option<Lines> {
        self.lines
    }

    /// Counts again soon, as after a turn, a tool call or a `!` command; once
    /// the count running now, if any, is done.
    pub fn refresh(&mut self) {
        self.wanted = true;
    }

    /// Takes a finished count, and starts the next one when it is wanted,
    /// the directory changed or [`EVERY`] has passed since the last.
    pub fn poll(&mut self, cwd: &str) {
        if self.cwd.as_deref() != Some(cwd) {
            self.stop();
            self.cwd = Some(cwd.to_owned());
            self.lines = None;
            self.wanted = true;
        }
        if let Some((result, _)) = &mut self.running {
            match result.try_recv() {
                Ok(lines) => self.lines = lines,
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => self.lines = None,
            }
            self.running = None;
        }
        if !self.wanted && self.started.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.wanted = false;
        self.started = Some(Instant::now());
        let cwd = cwd.to_owned();
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = sender.send(crate::git::changed_lines(&cwd).await);
        });
        self.running = Some((receiver, task));
    }

    /// Drops a count in progress; git is killed with it.
    fn stop(&mut self) {
        if let Some((_, task)) = self.running.take() {
            task.abort();
        }
    }
}

impl Drop for GitDiff {
    fn drop(&mut self) {
        self.stop();
    }
}
