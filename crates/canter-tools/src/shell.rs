use crate::output::{BoundedText, MAX_CLIENT_OUTPUT_BYTES};
use crate::{Executor, ToolOutcome, ToolOutput};
use canter_protocol::{Item, ItemStatus};
use serde::Deserialize;
use std::{io, os::unix::process::ExitStatusExt, time::Instant};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
pub const MAX_SHELL_TIMEOUT_MS: u64 = 600_000;
pub const MAX_MODEL_OUTPUT_BYTES: usize = 32 * 1024;
pub const IO_DRAIN_TIMEOUT_MS: u64 = 2_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ShellArgs {
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}
fn default_timeout() -> u64 {
    DEFAULT_SHELL_TIMEOUT_MS
}

pub(crate) async fn run(
    executor: &dyn Executor,
    id: String,
    args: &ShellArgs,
    cancel: &CancellationToken,
    on_output: &mut (dyn FnMut(&str) + Send),
) -> ToolOutput {
    let start = Instant::now();
    let mut output = Output::new();
    let result = capture(executor, args, cancel, &mut output, on_output).await;
    let (outcome, exit_code, message) = match result {
        Ok((status, interruption)) => {
            let outcome = interruption.unwrap_or(if status.success() {
                ToolOutcome::Completed
            } else {
                ToolOutcome::Failed
            });
            let message = match outcome {
                ToolOutcome::Cancelled => "Command cancelled; process group killed.".into(),
                ToolOutcome::TimedOut => format!(
                    "Command timed out after {} ms; process group killed.",
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
    ToolOutput {
        text: format!("{}\n{message}", output.model.render(false)),
        item: Item::CommandExecution {
            id,
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

async fn capture(
    executor: &dyn Executor,
    args: &ShellArgs,
    cancel: &CancellationToken,
    output: &mut Output,
    on_output: &mut (dyn FnMut(&str) + Send),
) -> io::Result<(std::process::ExitStatus, Option<ToolOutcome>)> {
    let mut handle = executor.spawn(&args.command, cancel).await?;
    let mut stdout = [0; 8192];
    let mut stderr = [0; 8192];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_decoder = Decoder::default();
    let mut stderr_decoder = Decoder::default();
    let mut status = None;
    let mut interruption = None;
    let mut cancellation_seen = false;
    let mut timeout_seen = false;
    let mut drain_deadline = None;
    let mut stopped_reading = false;
    let deadline = tokio::time::sleep(std::time::Duration::from_millis(args.timeout_ms));
    tokio::pin!(deadline);
    while !stdout_done || !stderr_done || status.is_none() {
        tokio::select! {
            biased;
            _ = cancel.cancelled(), if !cancellation_seen => {
                cancellation_seen = true;
                interruption.get_or_insert(ToolOutcome::Cancelled);
                if status.is_some() { stopped_reading = true; break; }
                handle.process.terminate().await?;
                if let Ok(result) = tokio::time::timeout(std::time::Duration::from_millis(50), handle.process.wait()).await { result?; }
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            _ = &mut deadline, if !timeout_seen => {
                timeout_seen = true;
                interruption.get_or_insert(ToolOutcome::TimedOut);
                if status.is_some() { stopped_reading = true; break; }
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            _ = tokio::time::sleep_until(drain_deadline.unwrap_or_else(tokio::time::Instant::now)), if drain_deadline.is_some() => {
                stopped_reading = true;
                break;
            }
            result = handle.process.wait(), if status.is_none() => {
                status = Some(result?);
                handle.process.kill().await?;
                drain_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(IO_DRAIN_TIMEOUT_MS));
            }
            result = handle.stdout.read(&mut stdout), if !stdout_done => {
                let count = result?;
                stdout_done = count == 0;
                let chunk = stdout_decoder.push(&stdout[..count], stdout_done);
                output.push(&chunk, on_output);
            }
            result = handle.stderr.read(&mut stderr), if !stderr_done => {
                let count = result?;
                stderr_done = count == 0;
                let chunk = stderr_decoder.push(&stderr[..count], stderr_done);
                output.push(&chunk, on_output);
            }
        }
    }
    if stopped_reading {
        let notice = "\nBackground processes still hold output pipes; stopped reading.\n";
        output.push(notice, on_output);
    }
    status
        .map(|status| (status, interruption))
        .ok_or_else(|| io::Error::other("command has no exit status"))
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
    client: BoundedText,
    model: BoundedText,
    streamed: usize,
}

impl Output {
    fn new() -> Self {
        Self {
            client: BoundedText::new(MAX_CLIENT_OUTPUT_BYTES),
            model: BoundedText::new(MAX_MODEL_OUTPUT_BYTES),
            streamed: 0,
        }
    }

    fn push(&mut self, text: &str, on_output: &mut (dyn FnMut(&str) + Send)) {
        self.client.push(text.as_bytes());
        self.model.push(text.as_bytes());
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
}
