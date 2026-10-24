//! Session tools and a replaceable executor. This crate emits no protocol events.
mod executor;
mod permission;
mod shell;
pub use executor::{CommandHandle, CommandProcess, Executor, FileReader, LocalExecutor};
pub use permission::{PermissionCheck, PermissionGate};
pub use shell::{DEFAULT_SHELL_TIMEOUT_MS, MAX_MODEL_OUTPUT_BYTES, MAX_SHELL_TIMEOUT_MS};

use canter_protocol::{FileUpdateChange, Item, ItemStatus, PatchChangeKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;

pub const MAX_READ_LINES: u32 = 2_000;
pub const MAX_READ_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Read,
    Write,
    Edit,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Read,
    Write,
    Execute,
}

pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub risk: Risk,
}

impl Tool {
    pub const ALL: [Self; 4] = [Self::Read, Self::Write, Self::Edit, Self::Shell];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Shell => "shell",
        }
    }

    pub fn risk(self) -> Risk {
        match self {
            Self::Read => Risk::Read,
            Self::Write | Self::Edit => Risk::Write,
            Self::Shell => Risk::Execute,
        }
    }

    pub fn definition(self) -> ToolDefinition {
        if self == Self::Shell {
            return ToolDefinition {
                name: self.name(),
                description: "Run a command with /bin/sh -c in the session directory and environment. Output is streamed; model results retain a bounded head and tail.",
                parameters: json!({"type":"object","properties":{"command":{"type":"string","minLength":1},"timeoutMs":{"type":"integer","minimum":1,"maximum":MAX_SHELL_TIMEOUT_MS,"default":DEFAULT_SHELL_TIMEOUT_MS}},"required":["command"],"additionalProperties":false}),
                risk: self.risk(),
            };
        }
        if self == Self::Edit {
            return ToolDefinition {
                name: self.name(),
                description: "Replace exact text in a UTF-8 file. Each oldText must occur exactly once in the original file; replacements must not overlap. All edits succeed together or none are written.",
                parameters: json!({"type":"object","properties":{"path":{"type":"string","minLength":1},"edits":{"type":"array","minItems":1,"items":{"type":"object","properties":{"oldText":{"type":"string","minLength":1},"newText":{"type":"string"}},"required":["oldText","newText"],"additionalProperties":false}}},"required":["path","edits"],"additionalProperties":false}),
                risk: self.risk(),
            };
        }
        if self == Self::Write {
            return ToolDefinition {
                name: self.name(),
                description: "Write a complete UTF-8 file, creating parent directories.",
                parameters: json!({"type":"object","properties":{"path":{"type":"string","minLength":1},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
                risk: self.risk(),
            };
        }
        ToolDefinition {
            name: self.name(),
            description: "Read a UTF-8 file with 1-based line numbers. At most 2000 lines and 64 KiB per call; follow the returned startLine to continue. Binary files are rejected.",
            parameters: json!({"type":"object", "properties": {
                "path":{"type":"string", "minLength":1},
                "startLine":{"type":"integer", "minimum":1, "maximum":u32::MAX, "default":1},
                "lineCount":{"type":"integer", "minimum":1, "maximum":u32::MAX, "default":MAX_READ_LINES}
            }, "required":["path"], "additionalProperties":false}),
            risk: self.risk(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub struct ToolOutput {
    pub text: String,
    pub item: Item,
    pub outcome: ToolOutcome,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default = "first_line")]
    start_line: u32,
    #[serde(default = "max_lines")]
    line_count: u32,
}
fn first_line() -> u32 {
    1
}
fn max_lines() -> u32 {
    MAX_READ_LINES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<Replacement>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Replacement {
    old_text: String,
    new_text: String,
}

enum Action {
    Read(ReadArgs),
    Change {
        path: String,
        original: Option<Vec<u8>>,
        content: String,
        change: FileUpdateChange,
    },
    Shell(shell::ShellArgs),
}

/// Keep this value unchanged between approval and execution.
pub struct PreparedCall {
    tool: Tool,
    id: String,
    raw_arguments: String,
    arguments: Value,
    action: Action,
}

impl PreparedCall {
    pub fn tool(&self) -> Tool {
        self.tool
    }
    pub fn raw_arguments(&self) -> &str {
        &self.raw_arguments
    }
    pub fn approval_arguments(&self) -> &Value {
        &self.arguments
    }
}

pub struct Tools {
    executor: Arc<dyn Executor>,
}

impl Tools {
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self { executor }
    }

    pub async fn prepare(
        &self,
        tool: Tool,
        id: impl Into<String>,
        raw_arguments: &str,
        cancel: &CancellationToken,
    ) -> Result<PreparedCall, ToolOutput> {
        let id = id.into();
        let mut arguments = serde_json::from_str::<Value>(raw_arguments).map_err(|e| {
            tool_error(
                tool,
                id.clone(),
                &Value::Null,
                format!("Invalid JSON arguments: {e}"),
                ToolOutcome::Failed,
            )
        })?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("tool cancelled".to_owned()),
            result = self.prepare_action(tool, &arguments, cancel) => result,
        };
        let action = result.map_err(|message| {
            tool_error(
                tool,
                id.clone(),
                &arguments,
                message,
                if cancel.is_cancelled() {
                    ToolOutcome::Cancelled
                } else {
                    ToolOutcome::Failed
                },
            )
        })?;
        if let Action::Change { change, .. } = &action {
            arguments["diff"] = json!(change.diff);
        }
        Ok(PreparedCall {
            tool,
            id,
            raw_arguments: raw_arguments.into(),
            arguments,
            action,
        })
    }

    async fn prepare_action(
        &self,
        tool: Tool,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> Result<Action, String> {
        match tool {
            Tool::Read => {
                let read: ReadArgs = parse(arguments)?;
                validate_path(&read.path)?;
                if read.start_line == 0 || read.line_count == 0 {
                    return Err("startLine and lineCount must be positive".into());
                }
                Ok(Action::Read(read))
            }
            Tool::Write => {
                let write: WriteArgs = parse(arguments)?;
                validate_path(&write.path)?;
                let original = match self.read_bytes(&write.path, cancel).await {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.to_string()),
                };
                change_action(write.path, original, write.content)
            }
            Tool::Edit => {
                let edit: EditArgs = parse(arguments)?;
                validate_path(&edit.path)?;
                if edit.edits.is_empty() {
                    return Err("edits must contain at least one replacement".into());
                }
                let original = self
                    .read_bytes(&edit.path, cancel)
                    .await
                    .map_err(|e| e.to_string())?;
                let before = std::str::from_utf8(&original)
                    .map_err(|_| "edit requires a UTF-8 text file".to_owned())?;
                let mut ranges = Vec::new();
                for (index, replacement) in edit.edits.iter().enumerate() {
                    executor::check_cancel(cancel).map_err(|e| e.to_string())?;
                    if replacement.old_text.is_empty() {
                        return Err(format!("edit {}: oldText must not be empty", index + 1));
                    }
                    let start = before
                        .find(&replacement.old_text)
                        .ok_or_else(|| format!("edit {}: oldText not found", index + 1))?;
                    // Advance one character, not the match length: aaa contains aa twice.
                    let next = start
                        + replacement
                            .old_text
                            .chars()
                            .next()
                            .expect("nonempty oldText")
                            .len_utf8();
                    if before[next..].contains(&replacement.old_text) {
                        return Err(format!(
                            "edit {}: oldText has multiple matches; supply unique text",
                            index + 1
                        ));
                    }
                    ranges.push((start, start + replacement.old_text.len(), index));
                }
                ranges.sort_unstable();
                for pair in ranges.windows(2) {
                    if pair[0].1 > pair[1].0 {
                        return Err(format!(
                            "edit {} overlaps edit {}; combine them into one replacement",
                            pair[1].2 + 1,
                            pair[0].2 + 1
                        ));
                    }
                }
                let mut content = before.to_owned();
                for (start, end, index) in ranges.into_iter().rev() {
                    content.replace_range(start..end, &edit.edits[index].new_text);
                }
                change_action(edit.path, Some(original), content)
            }
            Tool::Shell => {
                let args: shell::ShellArgs = parse(arguments)?;
                if args.command.is_empty() || args.command.contains('\0') {
                    return Err("command must be nonempty and contain no NUL bytes".into());
                }
                if args.timeout_ms == 0 || args.timeout_ms > MAX_SHELL_TIMEOUT_MS {
                    return Err(format!(
                        "timeoutMs must be between 1 and {MAX_SHELL_TIMEOUT_MS}"
                    ));
                }
                Ok(Action::Shell(args))
            }
        }
    }

    async fn read_bytes(&self, path: &str, cancel: &CancellationToken) -> std::io::Result<Vec<u8>> {
        let mut reader = self.executor.open_file(path.as_ref(), cancel).await?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    pub async fn execute(
        &self,
        call: PreparedCall,
        cancel: &CancellationToken,
        on_output: &mut (dyn FnMut(&str) + Send),
    ) -> ToolOutput {
        match &call.action {
            Action::Read(read) => {
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "tool cancelled")),
                    result = self.read(read, cancel) => result,
                };
                match result {
                    Ok(text) => ToolOutput {
                        item: Item::FileRead {
                            id: call.id,
                            path: read.path.clone(),
                            start_line: read.start_line,
                            content: text.clone(),
                            status: ItemStatus::Completed,
                        },
                        text,
                        outcome: ToolOutcome::Completed,
                    },
                    Err(error) => tool_error(
                        call.tool,
                        call.id,
                        &call.arguments,
                        error.to_string(),
                        if cancel.is_cancelled() {
                            ToolOutcome::Cancelled
                        } else {
                            ToolOutcome::Failed
                        },
                    ),
                }
            }
            Action::Change {
                path,
                original,
                content,
                change,
            } => {
                match self
                    .executor
                    .write_file(
                        path.as_ref(),
                        original.as_deref(),
                        content.as_bytes(),
                        cancel,
                    )
                    .await
                {
                    Ok(()) => ToolOutput {
                        text: format!("Wrote {path}.\n{}", change.diff),
                        item: Item::FileChange {
                            id: call.id,
                            changes: vec![change.clone()],
                            status: ItemStatus::Completed,
                        },
                        outcome: ToolOutcome::Completed,
                    },
                    Err(e) => tool_error(
                        call.tool,
                        call.id,
                        &call.arguments,
                        e.to_string(),
                        if cancel.is_cancelled() {
                            ToolOutcome::Cancelled
                        } else {
                            ToolOutcome::Failed
                        },
                    ),
                }
            }
            Action::Shell(args) => {
                shell::run(self.executor.as_ref(), call.id, args, cancel, on_output).await
            }
        }
    }

    async fn read(&self, args: &ReadArgs, cancel: &CancellationToken) -> std::io::Result<String> {
        let mut reader = BufReader::new(self.executor.open_file(args.path.as_ref(), cancel).await?);
        let mut text = String::new();
        let mut line_number = 1u64;
        let end = u64::from(args.start_line) + u64::from(args.line_count.min(MAX_READ_LINES));
        loop {
            executor::check_cancel(cancel)?;
            if reader.fill_buf().await?.is_empty() {
                break;
            }
            if line_number >= end {
                text.push_str(&format!(
                    "\nMore content: read with startLine={line_number}."
                ));
                break;
            }
            let skip = line_number < u64::from(args.start_line);
            let mut line = Vec::new();
            loop {
                executor::check_cancel(cancel)?;
                let buffer = reader.fill_buf().await?;
                if buffer.is_empty() {
                    break;
                }
                let newline = buffer.iter().position(|&b| b == b'\n');
                let length = newline.map_or(buffer.len(), |n| n + 1);
                if buffer[..length].contains(&0) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "binary file: read supports UTF-8 text only",
                    ));
                }
                if !skip {
                    if text.len() + line.len() + length + 16 > MAX_READ_BYTES {
                        text.push_str(&format!("\nByte limit reached: read with startLine={line_number}. If this line alone exceeds {MAX_READ_BYTES} bytes, use shell to inspect byte ranges."));
                        return Ok(text);
                    }
                    line.extend_from_slice(&buffer[..length]);
                }
                reader.consume(length);
                if newline.is_some() {
                    break;
                }
            }
            if !skip {
                let line = std::str::from_utf8(&line).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "binary or non-UTF-8 file: read supports UTF-8 text only",
                    )
                })?;
                text.push_str(&format!("{line_number}: {line}"));
                if !line.ends_with('\n') {
                    text.push('\n');
                }
            }
            line_number += 1;
        }
        Ok(text)
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("Invalid arguments: {e}"))
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.contains('\0') {
        Err("path must be nonempty and contain no NUL bytes".into())
    } else {
        Ok(())
    }
}

fn change_action(
    path: String,
    original: Option<Vec<u8>>,
    content: String,
) -> Result<Action, String> {
    let before = std::str::from_utf8(original.as_deref().unwrap_or_default())
        .map_err(|_| "cannot generate a text diff for a non-UTF-8 file".to_owned())?;
    let diff = similar::TextDiff::from_lines(before, &content)
        .unified_diff()
        .header(&path, &path)
        .to_string();
    let kind = if original.is_some() {
        PatchChangeKind::Update { move_path: None }
    } else {
        PatchChangeKind::Add
    };
    let change = FileUpdateChange {
        path: path.clone(),
        kind,
        diff,
    };
    Ok(Action::Change {
        path,
        original,
        content,
        change,
    })
}

fn tool_error(
    tool: Tool,
    id: String,
    arguments: &Value,
    text: String,
    outcome: ToolOutcome,
) -> ToolOutput {
    let item = match tool {
        Tool::Read => Item::FileRead {
            id,
            path: arguments["path"].as_str().unwrap_or_default().into(),
            start_line: arguments["startLine"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
                .unwrap_or(1),
            content: text.clone(),
            status: ItemStatus::Failed,
        },
        Tool::Write | Tool::Edit => Item::FileChange {
            id,
            changes: vec![],
            status: ItemStatus::Failed,
        },
        Tool::Shell => Item::CommandExecution {
            id,
            command: arguments["command"].as_str().unwrap_or_default().into(),
            cwd: String::new(),
            status: ItemStatus::Failed,
            aggregated_output: text.clone(),
            exit_code: None,
            duration_ms: None,
        },
    };
    ToolOutput {
        item,
        text,
        outcome,
    }
}
