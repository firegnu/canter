//! Workspace branch and changed-line counts fetched without blocking drawing.
use crate::client::Client;
use canter_protocol::WorkspaceSummary;
use std::time::Duration;
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

const EVERY: Duration = Duration::from_secs(5);
type Pending = (
    oneshot::Receiver<Option<WorkspaceSummary>>,
    JoinHandle<()>,
    bool,
);

pub(crate) struct GitDiff {
    cwd: Option<String>,
    branch: Option<String>,
    lines: Option<(u64, u64)>,
    wanted: bool,
    started: Option<Instant>,
    running: Option<Pending>,
}
impl GitDiff {
    pub fn new() -> Self {
        Self {
            cwd: None,
            branch: None,
            lines: None,
            wanted: true,
            started: None,
            running: None,
        }
    }
    pub fn lines(&self) -> Option<(u64, u64)> {
        self.lines
    }
    pub fn branch(&self) -> Option<String> {
        self.branch.clone()
    }
    pub fn set(&mut self, cwd: &str, summary: WorkspaceSummary) {
        self.cwd = Some(cwd.into());
        self.branch = summary.branch;
        self.lines = summary.changed_lines;
        self.started = Some(Instant::now());
        self.wanted = self.lines.is_none();
    }
    pub fn refresh(&mut self) {
        self.wanted = true;
    }
    pub fn poll(&mut self, client: &Client, session: &str, cwd: &str, branch_due: bool) {
        if self.cwd.as_deref() != Some(cwd) {
            self.stop();
            self.cwd = Some(cwd.into());
            self.branch = None;
            self.lines = None;
            self.wanted = true;
        }
        if let Some((result, _, counted)) = &mut self.running {
            match result.try_recv() {
                Ok(summary) => {
                    self.branch = summary.as_ref().and_then(|s| s.branch.clone());
                    if *counted {
                        self.lines = summary.and_then(|s| s.changed_lines);
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.branch = None;
                    self.lines = None;
                }
            }
            self.running = None;
        }
        let count = self.wanted || self.started.is_none_or(|at| at.elapsed() >= EVERY);
        if !count && !branch_due {
            return;
        }
        self.wanted = false;
        if count {
            self.started = Some(Instant::now());
        }
        let client = client.clone();
        let session = session.to_owned();
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = sender.send(client.workspace_summary(&session, count).await.ok());
        });
        self.running = Some((receiver, task, count));
    }
    fn stop(&mut self) {
        if let Some((_, task, _)) = self.running.take() {
            task.abort();
        }
    }
}
impl Drop for GitDiff {
    fn drop(&mut self) {
        self.stop();
    }
}
