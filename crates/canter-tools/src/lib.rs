//! Session tools and a replaceable executor. This crate emits no protocol events.
mod executor;
pub mod mcp;
mod output;
pub use output::MAX_CLIENT_OUTPUT_BYTES;
mod permission;
mod readonly;
mod shell;
pub use executor::{CommandHandle, CommandProcess, Executor, FileReader, LocalExecutor};
pub use permission::{ExecutionPermit, PermissionCheck, PermissionGate};
pub use shell::{DEFAULT_SHELL_TIMEOUT_MS, MAX_MODEL_OUTPUT_BYTES, MAX_SHELL_TIMEOUT_MS};

use canter_protocol::{
    FileUpdateChange, Item, ItemStatus, PatchChangeKind, UserInputOption, UserInputRequest,
};
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
    AskUser,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Read,
    Write,
    Execute,
    Interaction,
}

pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub risk: Risk,
}

impl Tool {
    pub const ALL: [Self; 5] = [
        Self::Read,
        Self::Write,
        Self::Edit,
        Self::Shell,
        Self::AskUser,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Shell => "shell",
            Self::AskUser => "ask_user",
            Self::Mcp => "mcp",
        }
    }

    pub fn risk(self) -> Risk {
        match self {
            Self::Read => Risk::Read,
            Self::Write | Self::Edit => Risk::Write,
            Self::Shell | Self::Mcp => Risk::Execute,
            Self::AskUser => Risk::Interaction,
        }
    }

    pub fn definition(self) -> ToolDefinition {
        if self == Self::AskUser {
            return ToolDefinition {
                name: self.name(),
                description: "Ask the user one question with 2 to 4 options, each with a short label and a one-sentence description. Ask only when you are genuinely uncertain and the answer would change your approach. First investigate anything you can discover yourself by reading files or running read-only commands. The user can choose one option or write their own answer. Ask separate questions in separate calls.",
                parameters: json!({"type":"object","properties":{
                    "question":{"type":"string","minLength":1},
                    "options":{"type":"array","minItems":2,"maxItems":4,"items":{
                        "type":"object","properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}},
                        "required":["label","description"],"additionalProperties":false
                    }}
                },"required":["question","options"],"additionalProperties":false}),
                risk: self.risk(),
            };
        }
        if self == Self::Shell {
            return ToolDefinition {
                name: self.name(),
                description: "Run /bin/sh -c in the session directory and environment. Default timeout: 120 seconds; maximum: 600 seconds. Output retains a bounded head and tail. When the shell ends, background processes in its original group are killed; processes that moved to other process groups or sessions cannot be cleaned up.",
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
    Declined,
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
#[serde(deny_unknown_fields)]
struct AskUserArgs {
    question: String,
    options: Vec<UserInputOption>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Replacement {
    old_text: String,
    new_text: String,
}

enum Action {
    Mcp(mcp::McpTool),
    AskUser(AskUserArgs),
    Read(ReadArgs),
    Change {
        path: String,
        original: Option<Vec<u8>>,
        content: String,
        change: FileUpdateChange,
        summary: String,
    },
    Shell(shell::ShellArgs),
}

/// Keep this value unchanged between approval and execution.
pub struct PreparedCall {
    executor: Arc<dyn Executor>,
    tool: Tool,
    id: String,
    raw_arguments: String,
    arguments: Value,
    resolved_path: Option<std::path::PathBuf>,
    action: Action,
}

impl PreparedCall {
    pub fn started_item(&self) -> Item {
        self.item(ItemStatus::InProgress)
    }
    pub fn declined_output(&self) -> ToolOutput {
        ToolOutput {
            text: "Tool call declined.".into(),
            item: self.item(ItemStatus::Declined),
            outcome: ToolOutcome::Declined,
        }
    }
    fn failed_output(&self, text: String, outcome: ToolOutcome) -> ToolOutput {
        ToolOutput {
            text,
            item: self.item(ItemStatus::Failed),
            outcome,
        }
    }
    fn item(&self, status: ItemStatus) -> Item {
        match &self.action {
            Action::Mcp(tool) => Item::McpToolCall {
                id: self.id.clone(),
                server: tool.server().into(),
                tool: tool.tool.clone(),
                arguments: self.arguments.clone(),
                result: String::new(),
                status,
            },
            Action::AskUser(args) => Item::UserInput {
                id: self.id.clone(),
                question: args.question.clone(),
                answer: None,
                status,
            },
            Action::Read(read) => Item::FileRead {
                id: self.id.clone(),
                path: read.path.clone(),
                start_line: read.start_line,
                content: String::new(),
                status,
            },
            Action::Change { change, .. } => Item::FileChange {
                id: self.id.clone(),
                changes: vec![change.clone()],
                status,
            },
            Action::Shell(args) => Item::CommandExecution {
                id: self.id.clone(),
                command: args.command.clone(),
                cwd: self.executor.cwd().to_string_lossy().into_owned(),
                aggregated_output: String::new(),
                exit_code: None,
                duration_ms: None,
                status,
            },
        }
    }
    pub(crate) fn permission_key(&self) -> String {
        match &self.action {
            Action::Mcp(tool) => tool.name.clone(),
            _ => self.tool.name().into(),
        }
    }
    pub fn tool(&self) -> Tool {
        self.tool
    }
    /// Interactions are awaited by the session, never passed to the permission gate.
    pub fn user_input_request(&self) -> Option<UserInputRequest> {
        match &self.action {
            Action::AskUser(args) => Some(UserInputRequest {
                item_id: self.id.clone(),
                question: args.question.clone(),
                options: args.options.clone(),
            }),
            _ => None,
        }
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
    mcp: Vec<mcp::McpTool>,
}

impl Tools {
    pub fn new(executor: Arc<dyn Executor>) -> Self {
        Self {
            executor,
            mcp: Vec::new(),
        }
    }

    pub fn with_mcp(mut self, tools: Vec<mcp::McpTool>) -> Self {
        self.mcp = tools;
        self
    }
    pub fn mcp_tools(&self) -> &[mcp::McpTool] {
        &self.mcp
    }
    pub fn find(&self, name: &str) -> Option<Tool> {
        Tool::from_name(name).or_else(|| {
            self.mcp
                .iter()
                .any(|tool| tool.name == name)
                .then_some(Tool::Mcp)
        })
    }
    pub async fn prepare_named(
        &self,
        name: &str,
        id: String,
        raw_arguments: &str,
        cancel: &CancellationToken,
    ) -> Result<PreparedCall, ToolOutput> {
        if let Some(tool) = Tool::from_name(name) {
            return self.prepare(tool, id, raw_arguments, cancel).await;
        }
        let tool = self
            .mcp
            .iter()
            .find(|tool| tool.name == name)
            .expect("known tool")
            .clone();
        let arguments = serde_json::from_str::<Value>(raw_arguments)
            .ok()
            .filter(Value::is_object);
        let prepared = PreparedCall {
            executor: self.executor.clone(),
            tool: Tool::Mcp,
            id,
            raw_arguments: raw_arguments.into(),
            arguments: arguments.clone().unwrap_or(Value::Null),
            resolved_path: None,
            action: Action::Mcp(tool),
        };
        if arguments.is_none() {
            return Err(prepared.failed_output(
                "MCP arguments must be a JSON object".into(),
                ToolOutcome::Failed,
            ));
        }
        Ok(prepared)
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
                self.executor.cwd(),
            )
        })?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("tool cancelled".to_owned()),
            result = async {
                let resolved = if tool != Tool::Shell {
                    match arguments["path"].as_str() {
                        Some(path) => { validate_path(path)?; Some(self.executor.resolve_path(path.as_ref(), cancel).await.map_err(|e| e.to_string())?) },
                        None => None,
                    }
                } else { None };
                self.prepare_action(tool, &arguments, cancel).await.map(|action| (action, resolved))
            } => result,
        };
        let (action, resolved_path) = result.map_err(|message| {
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
                self.executor.cwd(),
            )
        })?;
        if let Action::Change { change, .. } = &action {
            arguments["diff"] = json!(change.diff);
        }
        if let Some(path) = &resolved_path
            && arguments["path"].as_str() != path.to_str()
        {
            arguments["resolvedPath"] = json!(path);
        }
        Ok(PreparedCall {
            executor: self.executor.clone(),
            tool,
            id,
            raw_arguments: raw_arguments.into(),
            arguments,
            resolved_path,
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
            Tool::Mcp => Err("prepare an MCP tool by its qualified name".into()),
            Tool::AskUser => {
                let args: AskUserArgs = parse(arguments)?;
                if args.question.trim().is_empty()
                    || !(2..=4).contains(&args.options.len())
                    || args.options.iter().any(|option| {
                        option.label.trim().is_empty() || option.description.trim().is_empty()
                    })
                    || args.options.iter().enumerate().any(|(i, option)| {
                        args.options[..i]
                            .iter()
                            .any(|previous| previous.label == option.label)
                    })
                {
                    return Err("ask_user requires a question and 2 to 4 distinct, non-empty options with descriptions".into());
                }
                Ok(Action::AskUser(args))
            }
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
                change_action(write.path, original, write.content, cancel).await
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
                change_action(edit.path, Some(original), content, cancel).await
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

    /// Execute only a permit issued by PermissionGate for this executor's call.
    ///
    /// A prepared call alone cannot bypass the gate:
    /// ```compile_fail
    /// use canter_tools::{PreparedCall, Tools};
    /// async fn bypass(tools: &Tools, call: PreparedCall) {
    ///     tools.execute(call, &tokio_util::sync::CancellationToken::new(), &mut |_| {}).await;
    /// }
    /// ```
    pub async fn execute(
        &self,
        permit: ExecutionPermit<'_>,
        cancel: &CancellationToken,
        on_output: &mut (dyn FnMut(&str) + Send),
    ) -> ToolOutput {
        let call = permit.into_call();
        if !Arc::ptr_eq(&call.executor, &self.executor) {
            return call.failed_output(
                "call belongs to a different executor".into(),
                ToolOutcome::Failed,
            );
        }
        if let Some(expected) = &call.resolved_path {
            let current = self
                .executor
                .resolve_path(
                    call.arguments["path"].as_str().unwrap_or_default().as_ref(),
                    cancel,
                )
                .await;
            let error = match current {
                Ok(path) if &path == expected => None,
                Ok(_) => Some("file target changed since preparation; prepare a new call".into()),
                Err(error) => Some(error.to_string()),
            };
            if let Some(error) = error {
                return call.failed_output(
                    error,
                    if cancel.is_cancelled() {
                        ToolOutcome::Cancelled
                    } else {
                        ToolOutcome::Failed
                    },
                );
            }
        }
        match &call.action {
            Action::Mcp(tool) => {
                let (text, result, outcome) = tool.call(&call.arguments, cancel).await;
                ToolOutput {
                    text,
                    outcome,
                    item: Item::McpToolCall {
                        id: call.id.clone(),
                        server: tool.server().into(),
                        tool: tool.tool.clone(),
                        arguments: call.arguments.clone(),
                        result,
                        status: if outcome == ToolOutcome::Completed {
                            ItemStatus::Completed
                        } else {
                            ItemStatus::Failed
                        },
                    },
                }
            }
            Action::AskUser(_) => call.failed_output(
                "User input must be handled by the session".into(),
                ToolOutcome::Failed,
            ),
            Action::Read(read) => {
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "tool cancelled")),
                    result = self.read(read, call.resolved_path.as_deref().unwrap_or_else(|| read.path.as_ref()), cancel) => result,
                };
                match result {
                    Ok(text) => ToolOutput {
                        item: Item::FileRead {
                            id: call.id.clone(),
                            path: read.path.clone(),
                            start_line: read.start_line,
                            content: text.clone(),
                            status: ItemStatus::Completed,
                        },
                        text,
                        outcome: ToolOutcome::Completed,
                    },
                    Err(error) => call.failed_output(
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
                summary,
            } => {
                match self
                    .executor
                    .write_file(
                        call.resolved_path
                            .as_deref()
                            .unwrap_or_else(|| path.as_ref()),
                        original.as_deref(),
                        content.as_bytes(),
                        cancel,
                    )
                    .await
                {
                    Ok(()) => ToolOutput {
                        text: summary.clone(),
                        item: Item::FileChange {
                            id: call.id.clone(),
                            changes: vec![change.clone()],
                            status: ItemStatus::Completed,
                        },
                        outcome: ToolOutcome::Completed,
                    },
                    Err(e) => call.failed_output(
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
                shell::run(
                    self.executor.as_ref(),
                    call.id.clone(),
                    args,
                    cancel,
                    on_output,
                )
                .await
            }
        }
    }

    async fn read(
        &self,
        args: &ReadArgs,
        path: &std::path::Path,
        cancel: &CancellationToken,
    ) -> std::io::Result<String> {
        let mut reader = BufReader::new(self.executor.open_file(path, cancel).await?);
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
        if text.is_empty() && u64::from(args.start_line) >= line_number {
            text = format!(
                "startLine={} is beyond end of file ({} lines).",
                args.start_line,
                line_number - 1
            );
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

async fn change_action(
    path: String,
    original: Option<Vec<u8>>,
    content: String,
    cancel: &CancellationToken,
) -> Result<Action, String> {
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        executor::check_cancel(&cancel).map_err(|e| e.to_string())?;
        let before = std::str::from_utf8(original.as_deref().unwrap_or_default())
            .map_err(|_| "cannot generate a text diff for a non-UTF-8 file".to_owned())?;
        let diff = similar::TextDiff::configure()
            .timeout(std::time::Duration::from_millis(200))
            .diff_lines(before, &content);
        let mut added = 0usize;
        let mut removed = 0usize;
        for change in diff.iter_all_changes() {
            executor::check_cancel(&cancel).map_err(|e| e.to_string())?;
            match change.tag() {
                similar::ChangeTag::Insert => added += 1,
                similar::ChangeTag::Delete => removed += 1,
                similar::ChangeTag::Equal => {}
            }
        }
        let mut writer = DiffWriter {
            output: output::BoundedText::new(MAX_CLIENT_OUTPUT_BYTES),
            cancel,
        };
        diff.unified_diff()
            .header(&path, &path)
            .to_writer(&mut writer)
            .map_err(|e| e.to_string())?;
        let kind = if original.is_some() {
            PatchChangeKind::Update { move_path: None }
        } else {
            PatchChangeKind::Add
        };
        let summary = format!(
            "{} {path}: {added} lines added, {removed} lines removed.",
            if original.is_some() {
                "Updated"
            } else {
                "Created"
            }
        );
        let change = FileUpdateChange {
            path: path.clone(),
            kind,
            diff: writer.output.render(true),
        };
        Ok(Action::Change {
            path,
            original,
            content,
            change,
            summary,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

struct DiffWriter {
    output: output::BoundedText,
    cancel: CancellationToken,
}
impl std::io::Write for DiffWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        executor::check_cancel(&self.cancel)?;
        self.output.push(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tool_error(
    tool: Tool,
    id: String,
    arguments: &Value,
    text: String,
    outcome: ToolOutcome,
    cwd: &std::path::Path,
) -> ToolOutput {
    let item = match tool {
        Tool::Mcp => Item::McpToolCall {
            id,
            server: String::new(),
            tool: String::new(),
            arguments: arguments.clone(),
            result: String::new(),
            status: ItemStatus::Failed,
        },
        Tool::AskUser => Item::UserInput {
            id,
            question: arguments["question"].as_str().unwrap_or_default().into(),
            answer: None,
            status: ItemStatus::Failed,
        },
        Tool::Read => Item::FileRead {
            id,
            path: arguments["path"].as_str().unwrap_or_default().into(),
            start_line: arguments["startLine"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
                .unwrap_or(1),
            content: String::new(),
            status: ItemStatus::Failed,
        },
        Tool::Write | Tool::Edit => Item::FileChange {
            id,
            changes: arguments["path"]
                .as_str()
                .map(|path| FileUpdateChange {
                    path: path.into(),
                    kind: PatchChangeKind::Unknown,
                    diff: String::new(),
                })
                .into_iter()
                .collect(),
            status: ItemStatus::Failed,
        },
        Tool::Shell => Item::CommandExecution {
            id,
            command: arguments["command"].as_str().unwrap_or_default().into(),
            cwd: cwd.to_string_lossy().into_owned(),
            status: ItemStatus::Failed,
            aggregated_output: String::new(),
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
