use crate::background::{self, BackgroundCommands, Control, Tail};
use crate::output::{BoundedText, MAX_CLIENT_OUTPUT_BYTES, OutputStore, SavedOutput};
use crate::scratch::ScratchDirectory;
use crate::{Executor, ToolOutcome, ToolOutput};
use canter_protocol::{ExecCommandSource, Item, ItemStatus};
use serde::Deserialize;
use std::{io, os::unix::process::ExitStatusExt, sync::Arc, time::Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
pub const MAX_SHELL_TIMEOUT_MS: u64 = 600_000;
pub const MAX_MODEL_OUTPUT_BYTES: usize = 32 * 1024;
pub const IO_DRAIN_TIMEOUT_MS: u64 = 2_000;
// The cleanup of a finished command's process group stops services started with `&`;
// say so where it happens, so the model doesn't debug a server that is gone.
const BACKGROUND_HINT: &str = "To keep a server or watcher running, use background=true.";
const LEFTOVER_NOTICE: &str = "Processes this command left running in the background were stopped when the shell exited. To keep a server or watcher running, use background=true.";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ShellArgs {
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub sandbox_request: Option<canter_protocol::SandboxRequest>,
    #[serde(default)]
    pub justification: Option<String>,
}
fn default_timeout() -> u64 {
    DEFAULT_SHELL_TIMEOUT_MS
}

pub(crate) async fn run(
    executor: Arc<dyn Executor>,
    background: &BackgroundCommands,
    id: String,
    args: &ShellArgs,
    source: ExecCommandSource,
    cancel: &CancellationToken,
    on_output: &mut (dyn FnMut(&str) + Send),
) -> ToolOutput {
    let control =
        match background.control(id.clone(), args.command.clone(), args.background, source) {
            Ok(control) => control,
            Err(message) => return failed_start(executor.as_ref(), id, args, source, message),
        };
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let args = args.clone();
    let cancel = cancel.clone();
    let abandoned = CancellationToken::new();
    let _guard = abandoned.clone().drop_guard();
    tokio::spawn(async move {
        let mut control = control;
        let output = run_command(
            executor.as_ref(),
            id,
            &args,
            source,
            &cancel,
            &abandoned,
            &mut control,
            &sender,
        )
        .await;
        let _ = sender.send(Stream::Result(Box::new(output)));
    });
    loop {
        match receiver
            .recv()
            .await
            .expect("shell worker returns a result")
        {
            Stream::Output(text) => on_output(&text),
            Stream::Result(output) => return *output,
        }
    }
}

enum Stream {
    Output(String),
    Result(Box<ToolOutput>),
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    executor: &dyn Executor,
    id: String,
    args: &ShellArgs,
    source: ExecCommandSource,
    cancel: &CancellationToken,
    abandoned: &CancellationToken,
    control: &mut Control,
    sender: &mpsc::UnboundedSender<Stream>,
) -> ToolOutput {
    let start = Instant::now();
    let mut output = Output::new(executor, &id);
    let mut on_output = |text: &str| {
        let _ = sender.send(Stream::Output(text.into()));
    };
    let mut yielded = false;
    let mut on_background = |output: canter_protocol::BackgroundCommandOutput| {
        yielded = true;
        let _ = sender.send(Stream::Result(Box::new(background_output(
            executor,
            id.clone(),
            args,
            source,
            output,
            start,
        ))));
    };
    let result = capture(
        executor,
        args,
        source,
        cancel,
        abandoned,
        control,
        &mut output,
        &mut on_output,
        &mut on_background,
    )
    .await;
    if let Err(error) = &result {
        control.push(&format!("\n{error}"));
    }
    let (outcome, exit_code, message) = match result {
        Ok((status, interruption)) => {
            let outcome = interruption.unwrap_or(if status.success() {
                ToolOutcome::Completed
            } else {
                ToolOutcome::Failed
            });
            let message = match outcome {
                ToolOutcome::Cancelled => "Command cancelled; process group killed.".into(),
                ToolOutcome::TimedOut if source == ExecCommandSource::UserShell => format!(
                    "Command timed out after {} ms; process group killed.",
                    args.timeout_ms
                ),
                ToolOutcome::TimedOut => format!(
                    "Command timed out after {} ms; process group killed. {BACKGROUND_HINT}",
                    args.timeout_ms
                ),
                _ => match status.code() {
                    Some(code) => format!("Exit code: {code}"),
                    None => format!("Killed by signal {}", status.signal().unwrap_or_default()),
                },
            };
            (outcome, status.code(), message)
        }
        Err(e) => (
            if cancel.is_cancelled() {
                ToolOutcome::Cancelled
            } else {
                ToolOutcome::Failed
            },
            None,
            e.to_string(),
        ),
    };
    control.finish(exit_code);
    control.reconcile().await;
    if control.background_id.is_some() {
        return background_output(executor, id, args, source, control.output(!yielded), start);
    }
    let saved = output.finish_saved();
    ToolOutput {
        output_size: output.model.original_bytes_if_truncated().into(),
        change_ticket: None,
        images: Vec::new(),
        text: if source == ExecCommandSource::UserShell
            && exit_code.is_some()
            && matches!(outcome, ToolOutcome::Completed | ToolOutcome::Failed)
        {
            output.model.render_saved(false, saved.as_ref())
        } else {
            format!(
                "{}\n{message}",
                output.model.render_saved(false, saved.as_ref())
            )
        },
        item: Item::CommandExecution {
            id,
            source,
            command: args.command.clone(),
            cwd: executor.cwd().to_string_lossy().into_owned(),
            status: if outcome == ToolOutcome::Completed {
                ItemStatus::Completed
            } else {
                ItemStatus::Failed
            },
            aggregated_output: output.client.render(true),
            exit_code,
            duration_ms: Some(start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        },
        outcome,
    }
}

fn failed_start(
    executor: &dyn Executor,
    id: String,
    args: &ShellArgs,
    source: ExecCommandSource,
    message: String,
) -> ToolOutput {
    ToolOutput {
        output_size: Default::default(),
        change_ticket: None,
        images: Vec::new(),
        text: message.clone(),
        outcome: ToolOutcome::Failed,
        item: Item::CommandExecution {
            id,
            source,
            command: args.command.clone(),
            cwd: executor.cwd().to_string_lossy().into_owned(),
            status: ItemStatus::Failed,
            aggregated_output: message,
            exit_code: None,
            duration_ms: Some(0),
        },
    }
}

fn background_output(
    executor: &dyn Executor,
    id: String,
    args: &ShellArgs,
    source: ExecCommandSource,
    output: canter_protocol::BackgroundCommandOutput,
    start: Instant,
) -> ToolOutput {
    ToolOutput {
        output_size: (output.skipped_bytes > 0)
            .then_some(output.output.len() as u64 + output.skipped_bytes)
            .into(),
        change_ticket: None,
        images: Vec::new(),
        text: background::render(&output),
        outcome: ToolOutcome::Completed,
        item: Item::CommandExecution {
            id,
            source,
            command: args.command.clone(),
            cwd: executor.cwd().to_string_lossy().into_owned(),
            status: ItemStatus::Completed,
            aggregated_output: output.output,
            exit_code: None,
            duration_ms: Some(start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn capture(
    executor: &dyn Executor,
    args: &ShellArgs,
    source: ExecCommandSource,
    cancel: &CancellationToken,
    abandoned: &CancellationToken,
    control: &mut Control,
    output: &mut Output,
    on_output: &mut (dyn FnMut(&str) + Send),
    on_background: &mut (dyn FnMut(canter_protocol::BackgroundCommandOutput) + Send),
) -> io::Result<(std::process::ExitStatus, Option<ToolOutcome>)> {
    let mut handle = if source == ExecCommandSource::UserShell {
        executor.spawn_user(&args.command, cancel).await?
    } else if let Some(request) = &args.sandbox_request {
        executor
            .spawn_with_sandbox_request(&args.command, request, cancel)
            .await?
    } else {
        executor.spawn(&args.command, cancel).await?
    };
    control.started();
    let mut stdout = [0; 8192];
    let mut stderr = [0; 8192];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_decoder = Decoder::default();
    let mut stderr_decoder = Decoder::default();
    let mut denial_tails = [String::new(), String::new()];
    let mut reported_denial = false;
    let mut setup_failed = false;
    let mut status = None;
    let mut interruption = None;
    let mut cancellation_seen = false;
    let mut timeout_seen = false;
    let mut drain_deadline = None;
    let mut drain_stop_reason = None;
    let mut left_running = false;
    let deadline = tokio::time::sleep(std::time::Duration::from_millis(args.timeout_ms));
    tokio::pin!(deadline);
    let grace = tokio::time::sleep(std::time::Duration::from_secs(1));
    tokio::pin!(grace);
    let mut returned = false;
    while !stdout_done || !stderr_done || status.is_none() {
        tokio::select! {
            biased;
            _ = async {
                tokio::select! {
                    _ = control.shutdown.cancelled() => {},
                    _ = control.stop.cancelled() => {},
                    _ = cancel.cancelled(), if control.background_id.is_none() => {},
                    _ = abandoned.cancelled(), if control.background_id.is_none() => {},
                }
            }, if !cancellation_seen => {
                cancellation_seen = true;
                if status.is_some() {
                    drain_stop_reason = Some("Output reading was cancelled after the shell exited.");
                    break;
                }
                interruption.get_or_insert(ToolOutcome::Cancelled);
                handle.process.terminate().await?;
                if let Ok(result) = tokio::time::timeout(std::time::Duration::from_millis(50), handle.process.wait()).await { result?; }
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            _ = &mut deadline, if !timeout_seen && control.background_id.is_none() => {
                timeout_seen = true;
                if status.is_some() {
                    drain_stop_reason = Some("Output timeout reached after the shell exited.");
                    break;
                }
                interruption.get_or_insert(ToolOutcome::TimedOut);
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            _ = tokio::time::sleep_until(drain_deadline.unwrap_or_else(tokio::time::Instant::now)), if drain_deadline.is_some() => {
                drain_stop_reason = Some("Output drain limit reached after the shell exited.");
                break;
            }
            result = handle.process.wait(), if status.is_none() => {
                status = Some(result?);
                left_running = handle.process.kill().await?;
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            Some(reply) = control.promotions.recv(), if control.background_id.is_none() && status.is_none() => {
                let result = control.detach(&mut output.tail);
                if result.is_ok() {
                    // The foreground head/tail buffers are no longer needed, and
                    // a background result never refers to a saved file.
                    output.clear();
                    returned = true;
                    on_background(control.output(true));
                }
                let _ = reply.send(result.map(Some));
            }
            _ = &mut grace, if control.background_id.is_some() && !returned => {
                returned = true;
                on_background(control.output(true));
            }
            result = handle.stdout.read(&mut stdout), if !stdout_done => {
                let count = result?;
                stdout_done = count == 0;
                let chunk = stdout_decoder.push(&stdout[..count], stdout_done);
                if handle.sandboxed && !reported_denial { reported_denial = mentions_denial(&mut denial_tails[0], &chunk, &mut setup_failed); }
                if control.background_id.is_some() { control.push(&chunk); } else { output.push(&chunk, on_output); }
            }
            result = handle.stderr.read(&mut stderr), if !stderr_done => {
                let count = result?;
                stderr_done = count == 0;
                let chunk = stderr_decoder.push(&stderr[..count], stderr_done);
                if handle.sandboxed && !reported_denial { reported_denial = mentions_denial(&mut denial_tails[1], &chunk, &mut setup_failed); }
                if control.background_id.is_some() { control.push(&chunk); } else { output.push(&chunk, on_output); }
            }
        }
    }
    if let Some(reason) = drain_stop_reason {
        let notice =
            format!("\nBackground processes still hold output pipes; stopped reading. {reason}\n");
        if control.background_id.is_some() {
            control.push(&notice);
        } else {
            output.push(&notice, on_output);
        }
    }
    if left_running
        && interruption.is_none()
        && control.background_id.is_none()
        && source != ExecCommandSource::UserShell
    {
        output.push(&format!("\n{LEFTOVER_NOTICE}\n"), on_output);
    }
    if handle.sandboxed && interruption.is_none() && reported_denial {
        let notice = if handle.sandbox_approval_allowed {
            "\nThe sandbox blocked access outside the permitted paths or network scope. If the command really needs it, retry it with a sandbox request and a justification, or ask the user to run it with !.\n"
        } else {
            "\nThe sandbox blocks access outside the permitted paths and network scope. If the user needs that access, they can run it themselves with !.\n"
        };
        #[cfg(target_os = "linux")]
        let setup_notice = format!("\n{}\n", crate::LINUX_SANDBOX_HELP);
        #[cfg(target_os = "linux")]
        let notice = if setup_failed { &setup_notice } else { notice };
        if control.background_id.is_some() {
            control.push(notice);
        } else {
            output.model.push(notice.as_bytes());
        }
    }
    status
        .map(|status| (status, interruption))
        .ok_or_else(|| io::Error::other("command has no exit status"))
}

fn mentions_denial(tail: &mut String, chunk: &str, setup_failed: &mut bool) -> bool {
    tail.push_str(chunk);
    tail.make_ascii_lowercase();
    *setup_failed |= cfg!(target_os = "linux") && tail.contains("bwrap:");
    let found = *setup_failed
        || [
            "operation not permitted",
            "permission denied",
            "sandbox-exec:",
        ]
        .iter()
        .any(|message| tail.contains(message))
        || (cfg!(target_os = "linux")
            && [
                "read-only file system",
                "device or resource busy",
                "network is unreachable",
                "address family not supported",
            ]
            .iter()
            .any(|message| tail.contains(message)));
    // Retain enough to recognize an error split across pipe reads, before the
    // ordinary output limits discard it. stdout and stderr stay separate.
    let mut start = tail.len().saturating_sub(64);
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    tail.drain(..start);
    found
}

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8], eof: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.pending[consumed..end]));
                    consumed = end;
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{fffd}');
                            consumed += length;
                        }
                        None if eof => {
                            text.push('\u{fffd}');
                            consumed = self.pending.len();
                        }
                        None => break,
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        text
    }
}

struct Output {
    tail: Tail,
    client: BoundedText,
    model: BoundedText,
    streamed: usize,
    /// The output so far, until it is saved to a file or the result stays short.
    pending: Option<Vec<u8>>,
    saved: Option<OutputStore>,
    scratch: Option<Arc<ScratchDirectory>>,
    id: String,
}

impl Output {
    fn new(executor: &dyn Executor, id: &str) -> Self {
        Self {
            tail: Tail::default(),
            client: BoundedText::new(MAX_CLIENT_OUTPUT_BYTES),
            model: BoundedText::new(MAX_MODEL_OUTPUT_BYTES),
            streamed: 0,
            pending: Some(Vec::new()),
            saved: None,
            scratch: executor.scratch_directory(),
            id: id.to_owned(),
        }
    }

    /// Drop the foreground buffers and any saved file. Used when the caller's
    /// result becomes a background one, which never names a file.
    fn clear(&mut self) {
        self.tail = Tail::default();
        self.client = BoundedText::new(MAX_CLIENT_OUTPUT_BYTES);
        self.model = BoundedText::new(MAX_MODEL_OUTPUT_BYTES);
        self.streamed = 0;
        self.pending = None;
        self.saved = None;
    }

    fn push(&mut self, text: &str, on_output: &mut (dyn FnMut(&str) + Send)) {
        self.tail.push(text);
        self.client.push(text.as_bytes());
        self.model.push(text.as_bytes());
        self.save(text.as_bytes());
        let mut end = text.len().min(MAX_CLIENT_OUTPUT_BYTES - self.streamed);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            on_output(&text[..end]);
        }
        // Once a chunk crosses the limit, no later bytes are streamed.
        self.streamed = (self.streamed + text.len()).min(MAX_CLIENT_OUTPUT_BYTES);
    }

    /// Copy the whole output into the session scratch directory once the model
    /// result is truncated, so the discarded middle can still be read.
    fn save(&mut self, bytes: &[u8]) {
        if let Some(saved) = self.saved.as_mut() {
            let pushed = saved.push(bytes);
            if let Err(error) = pushed {
                self.save_failed(error);
            }
            return;
        }
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        pending.extend_from_slice(bytes);
        if self.model.original_bytes_if_truncated().is_none() {
            return;
        }
        let Some(scratch) = self.scratch.clone() else {
            self.pending = None;
            return;
        };
        let mut store = match OutputStore::create(scratch, &self.id) {
            Ok(store) => store,
            Err(error) => {
                self.save_failed(error);
                return;
            }
        };
        let prefix = self.pending.take().unwrap_or_default();
        match store.push(&prefix) {
            // Dropping the store removes the partial file.
            Err(error) => self.save_failed(error),
            Ok(()) => self.saved = Some(store),
        }
    }

    fn save_failed(&mut self, error: io::Error) {
        // Dropping the store removes the partial file.
        self.saved = None;
        self.pending = None;
        eprintln!("Full command output was not saved: {error}");
    }

    /// Flush a saved file before the result text names it; the model sees the
    /// path only for a file that was written.
    fn finish_saved(&mut self) -> Option<SavedOutput> {
        let saved = self.saved.take()?;
        match saved.finish() {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("Full command output was not saved: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalExecutor, PermissionGate, Tool, Tools};
    use canter_protocol::PermissionMode;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn a_full_session_budget_truncates_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = Arc::new(ScratchDirectory::temporary());
        assert!(scratch.claim_saved_output(crate::scratch::MAX_SESSION_SAVED_OUTPUT_BYTES));
        let environment = BTreeMap::from([
            ("HOME".into(), dir.path().to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        let tools = Tools::new(Arc::new(
            LocalExecutor::with_scratch_directory(dir.path().into(), environment, vec![], scratch)
                .unwrap(),
        ));
        std::fs::write(dir.path().join("large"), "x".repeat(100_000)).unwrap();
        let cancel = CancellationToken::new();
        let call = tools
            .prepare(Tool::Shell, "full", r#"{"command":"cat large"}"#, &cancel)
            .await
            .unwrap();
        let permit = match PermissionGate::default().evaluate(&call, PermissionMode::AllowAll) {
            crate::PermissionCheck::Execute(permit) => permit,
            _ => panic!("allowAll must issue a permit"),
        };
        let result = tools.execute(permit, &cancel, &mut |_| {}).await;
        assert_eq!(
            result.text,
            format!(
                "{}\n[truncated 67232 bytes]\n{}\nExit code: 0",
                "x".repeat(16_384),
                "x".repeat(16_384)
            )
        );
        let scratch = tools.scratch_directory().unwrap().path().unwrap();
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }
}
