use crate::{Executor, ToolOutcome, ToolOutput};
use canter_protocol::{Item, ItemStatus};
use serde::Deserialize;
use std::{io, time::Instant};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
pub const MAX_SHELL_TIMEOUT_MS: u64 = 600_000;
pub const MAX_MODEL_OUTPUT_BYTES: usize = 32 * 1024;

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
    let mut output = String::new();
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
                _ => format!("Exit code: {:?}", status.code()),
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
        text: format!("{}\n{message}", truncate(&output)),
        item: Item::CommandExecution {
            id,
            command: args.command.clone(),
            cwd: executor.cwd().to_string_lossy().into_owned(),
            status: if outcome == ToolOutcome::Completed {
                ItemStatus::Completed
            } else {
                ItemStatus::Failed
            },
            aggregated_output: output,
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
    output: &mut String,
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
    let deadline = tokio::time::sleep(std::time::Duration::from_millis(args.timeout_ms));
    tokio::pin!(deadline);
    while !stdout_done || !stderr_done || status.is_none() {
        tokio::select! {
            biased;
            _ = cancel.cancelled(), if interruption.is_none() && status.is_none() => {
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                interruption = Some(ToolOutcome::Cancelled);
            }
            _ = &mut deadline, if interruption.is_none() && status.is_none() => {
                handle.process.kill().await?;
                status = Some(handle.process.wait().await?);
                interruption = Some(ToolOutcome::TimedOut);
            }
            result = handle.process.wait(), if status.is_none() => {
                status = Some(result?);
                handle.process.kill().await?;
            }
            result = handle.stdout.read(&mut stdout), if !stdout_done => {
                let count = result?;
                stdout_done = count == 0;
                let chunk = stdout_decoder.push(&stdout[..count], stdout_done);
                output.push_str(&chunk);
                if !chunk.is_empty() { on_output(&chunk); }
            }
            result = handle.stderr.read(&mut stderr), if !stderr_done => {
                let count = result?;
                stderr_done = count == 0;
                let chunk = stderr_decoder.push(&stderr[..count], stderr_done);
                output.push_str(&chunk);
                if !chunk.is_empty() { on_output(&chunk); }
            }
        }
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

fn truncate(text: &str) -> String {
    if text.len() <= MAX_MODEL_OUTPUT_BYTES {
        return text.into();
    }
    let mut head = MAX_MODEL_OUTPUT_BYTES / 2;
    let mut tail = text.len() - MAX_MODEL_OUTPUT_BYTES / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[truncated {} bytes]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}
