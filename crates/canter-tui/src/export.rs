//! `/export`: the session as Markdown, written to a file or put on the
//! clipboard. What was said, in full; each tool call as one line (the
//! command, file or address, and how it went) with a few lines of its
//! output; no thinking; images as placeholders.
//!
//! Follows Codex `rust-v0.163.0` `/export` (`codex-rs/tui/src/app/transcript_export.rs`,
//! `chatwidget/transcript_export.rs`); no code is taken from it.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use canter_protocol::{
    FileUpdateChange, Item, ItemStatus, PatchChangeKind, SessionSnapshot, Turn, TurnStatus,
    UserInput, UserInputAnswer,
};

use crate::cells::display_path;
use crate::diff::Diff;
use crate::text::sanitize;

/// Lines of a tool's output kept.
const OUTPUT_LINES: usize = 8;
/// Characters kept of each of those lines, and of a one-line summary.
const LINE_CHARS: usize = 200;
/// Characters of a session name kept in a file name.
const NAME_CHARS: usize = 60;

/// Where `/export` puts the Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Export {
    /// This file, relative to the session's directory; `None` for the
    /// default name there.
    File(Option<String>),
    Clipboard,
}

/// The session as Markdown, exported at `exported` (shown as is). `None`
/// when nothing has been said yet.
pub(crate) fn markdown(snapshot: &SessionSnapshot, exported: &str) -> Option<String> {
    let cwd = snapshot.settings.cwd.as_str();
    let mut body = Body::default();
    for turn in &snapshot.turns {
        body.turn(turn, snapshot, cwd);
    }
    if body.text.is_empty() {
        return None;
    }
    let settings = &snapshot.settings;
    let title = settings
        .name
        .as_deref()
        .map(|name| one_line(name, usize::MAX))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| snapshot.id.clone());
    let mut out = format!("# {title}\n\n");
    out.push_str(&format!("- Directory: {}\n", one_line(cwd, usize::MAX)));
    out.push_str(&format!(
        "- Model: {}\n",
        one_line(&settings.model, usize::MAX)
    ));
    out.push_str(&format!("- Exported: {exported}\n"));
    out.push_str(&body.text);
    Some(out)
}

/// `canter-<session name>-<time>.md`: characters a file name cannot hold
/// become `-`; the session id when there is no name.
pub(crate) fn default_name(snapshot: &SessionSnapshot, time: &str) -> String {
    let name = snapshot
        .settings
        .name
        .as_deref()
        .map(file_safe)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| file_safe(&snapshot.id));
    format!("canter-{name}-{time}.md")
}

/// Where `path` points: relative to `cwd`, `~/` to `home`.
pub(crate) fn resolve(path: &str, cwd: &str, home: Option<&str>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        _ => Path::new(cwd).join(path),
    }
}

/// Writes `text` to a new file at `path`; one already there is left alone.
pub(crate) fn write(path: &Path, text: &str) -> Result<(), String> {
    let shown = path.display();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| match error.kind() {
            ErrorKind::AlreadyExists => format!("{shown} already exists; not overwritten"),
            _ => format!("cannot create {shown}: {error}"),
        })?;
    file.write_all(text.as_bytes()).map_err(|error| {
        let _ = std::fs::remove_file(path);
        format!("cannot write {shown}: {error}")
    })
}

/// The conversation after the header, one turn after another.
#[derive(Default)]
struct Body {
    text: String,
    /// Whose words come last: a new heading when that changes.
    speaker: Option<&'static str>,
}

impl Body {
    fn turn(&mut self, turn: &Turn, snapshot: &SessionSnapshot, cwd: &str) {
        match &turn.review {
            Some(title) => self.block("Review", &sanitize(title)),
            None if !turn.input.is_empty() => self.block("User", &user_text(&turn.input)),
            None => {}
        }
        // The item that repeats the turn's input is not said twice.
        let echo = (!turn.input.is_empty())
            .then(|| {
                turn.items.iter().position(
                    |item| matches!(item, Item::UserMessage { content, .. } if *content == turn.input),
                )
            })
            .flatten();
        for (index, item) in turn.items.iter().enumerate() {
            if Some(index) == echo
                || (matches!(item, Item::Plan { .. })
                    && snapshot.settings.plan_enabled == Some(false))
            {
                continue;
            }
            if matches!(item, Item::Diagnostics { .. }) {
                if let Some(line) = crate::lsp::text(&turn.items, index, cwd) {
                    self.block("Assistant", &line);
                }
                continue;
            }
            let feedback = item
                .id()
                .and_then(|id| snapshot.approval_feedback.get(id))
                .map(String::as_str);
            let mut shown = item.clone();
            if let Item::CommandExecution {
                id,
                status,
                exit_code,
                ..
            } = &mut shown
                && let Some(command) = snapshot
                    .background_commands
                    .iter()
                    .find(|command| command.item_id == *id)
            {
                *status = if command.running {
                    ItemStatus::InProgress
                } else if command.exit_code == Some(0) {
                    ItemStatus::Completed
                } else {
                    ItemStatus::Failed
                };
                *exit_code = command.exit_code;
            }
            self.item(&shown, cwd, feedback);
        }
        match turn.status {
            TurnStatus::Interrupted => self.block("Assistant", "_Interrupted_"),
            TurnStatus::Failed => {
                let reason = turn.error.as_ref().map_or(String::new(), |error| {
                    format!(": {}", sanitize(&error.message))
                });
                self.block("Assistant", &format!("_Failed{reason}_"));
            }
            _ => {}
        }
    }

    fn item(&mut self, item: &Item, cwd: &str, feedback: Option<&str>) {
        let assistant = "Assistant";
        match item {
            Item::ModelRetry { .. } => {
                self.block("Retry", &sanitize(&item.retry_message().unwrap()))
            }
            Item::BackgroundEvent { text, .. } => {
                let text = sanitize(text);
                let fence = "`".repeat((longest_run(&text, '`') + 1).max(3));
                self.block("Event", &format!("{fence}text\n{text}\n{fence}"));
            }
            Item::BackgroundCommandAction {
                command_id,
                action,
                result,
                ..
            } => {
                self.block(
                    assistant,
                    &tool(
                        &format!(
                            "{} #{command_id}",
                            if action == "monitor" {
                                action.clone()
                            } else {
                                format!("shell_{action}")
                            }
                        ),
                        result,
                        "",
                    ),
                );
            }
            Item::PlanProposal {
                plan,
                answer,
                status,
                ..
            } => {
                let mut text = format!(
                    "**{}**\n\n{}",
                    crate::plan_mode::result_label(answer.as_ref(), *status),
                    sanitize(plan)
                );
                if let Some(feedback) = answer
                    .as_ref()
                    .and_then(|answer| answer.feedback.as_deref())
                {
                    text.push_str(&format!("\n\nFeedback: {}", sanitize(feedback)));
                }
                self.block(assistant, &text);
            }
            Item::UserMessage { content, .. } => self.block("User", &user_text(content)),
            Item::AgentMessage { text, status, .. } if !text.trim().is_empty() => {
                self.block(assistant, sanitize(text).trim_end());
                if *status == Some(ItemStatus::Interrupted) {
                    self.block(assistant, "_Interrupted_");
                }
            }
            Item::CommandExecution {
                command,
                source,
                status,
                aggregated_output,
                exit_code,
                ..
            } => {
                let mut outcome = outcome(*status, feedback);
                if let Some(code) = exit_code {
                    outcome.push_str(&format!(", exit {code}"));
                }
                if crate::cells::no_matches(command, *exit_code, aggregated_output) {
                    outcome = "No matches".into();
                }
                let summary = format!(
                    "{} {} · {outcome}",
                    if *source == canter_protocol::ExecCommandSource::UserShell {
                        "User ran"
                    } else {
                        "Ran"
                    },
                    code_span(&one_line(command, LINE_CHARS))
                );
                self.block(
                    if *source == canter_protocol::ExecCommandSource::UserShell {
                        "User"
                    } else {
                        assistant
                    },
                    &tool(&summary, aggregated_output, ""),
                );
            }
            Item::FileChange {
                changes, status, ..
            } => {
                for change in changes {
                    let summary = format!(
                        "{} · {}",
                        file_change(change, cwd),
                        outcome(*status, feedback)
                    );
                    self.block(assistant, &tool(&summary, &change.diff, "diff"));
                }
            }
            Item::FileRead {
                path,
                start_line,
                content,
                status,
                content_items,
                ..
            } => {
                let lines = if content_items.is_empty() {
                    content.lines().count()
                } else {
                    0
                };
                let range = match lines {
                    0 => String::new(),
                    _ => format!(
                        " (lines {start_line}–{})",
                        u64::from(*start_line) + lines as u64 - 1
                    ),
                };
                let summary = format!(
                    "Read {}{range} · {}",
                    code_span(&display_path(path, cwd)),
                    outcome(*status, feedback)
                );
                let images = tool_image_placeholders(content_items);
                self.block(
                    assistant,
                    &tool(
                        &summary,
                        if images.is_empty() { content } else { &images },
                        "",
                    ),
                );
            }
            Item::McpToolSearch {
                query,
                tools,
                status,
                ..
            } => {
                let summary = format!(
                    "Searched MCP tools {} · {}",
                    code_span(&one_line(query, LINE_CHARS)),
                    if *status == ItemStatus::Completed {
                        format!("{} found", tools.len())
                    } else {
                        outcome(*status, feedback).to_string()
                    }
                );
                self.block(assistant, &tool(&summary, &tools.join("\n"), ""));
            }

            Item::WebSearch {
                query,
                results,
                status,
                error,
                ..
            } => {
                let summary = format!(
                    "Searched {} · {} results · {}",
                    code_span(&one_line(query, LINE_CHARS)),
                    results.len(),
                    outcome(*status, feedback)
                );
                let details = error.clone().unwrap_or_else(|| {
                    results
                        .iter()
                        .map(|result| {
                            format!(
                                "{}\n{}",
                                one_line(&result.title, LINE_CHARS),
                                one_line(&result.url, LINE_CHARS)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                self.block(assistant, &tool(&summary, &details, ""));
            }
            Item::WebFetch {
                url,
                resolved_url,
                result,
                status,
                ..
            } => {
                let url = resolved_url.as_deref().unwrap_or(url);
                let summary = format!(
                    "Fetched {} · {}",
                    code_span(&one_line(url, LINE_CHARS)),
                    outcome(*status, feedback)
                );
                self.block(assistant, &tool(&summary, result, ""));
            }
            Item::McpToolCall {
                server,
                tool: name,
                arguments,
                result,
                status,
                content_items,
                ..
            } => {
                let call = one_line(&format!("{server}/{name} {arguments}"), LINE_CHARS);
                let summary = format!(
                    "Called {} · {}",
                    code_span(&call),
                    outcome(*status, feedback)
                );
                self.block(assistant, &tool(&summary, result, ""));
                let images = tool_image_placeholders(content_items);
                if !images.is_empty() {
                    self.block(assistant, &images);
                }
            }
            Item::UserInput {
                question, answer, ..
            } => {
                let answer = match answer {
                    Some(UserInputAnswer::Option { label }) => sanitize(label),
                    Some(UserInputAnswer::Text { text }) => sanitize(text),
                    _ => "_No answer_".to_owned(),
                };
                self.block(
                    assistant,
                    &format!(
                        "**Question:** {}\n\n**Answer:** {answer}",
                        sanitize(question)
                    ),
                );
            }
            Item::ContextCompaction { .. } => self.block(assistant, "_Context compacted_"),
            Item::Plan {
                plan,
                explanation,
                status,
                ..
            } if *status != ItemStatus::InProgress => {
                self.block(
                    assistant,
                    &crate::plan::markdown(plan, explanation.as_deref(), *status),
                );
            }
            // No thinking or unfinished plan updates.
            Item::Plan { .. }
            | Item::Reasoning { .. }
            | Item::AgentMessage { .. }
            | Item::Diagnostics { .. }
            | Item::Unknown => {}
        }
    }

    /// `text` after a `## speaker` heading, or right after the speaker's last words.
    fn block(&mut self, speaker: &'static str, text: &str) {
        if self.speaker != Some(speaker) {
            self.text.push_str(&format!("\n## {speaker}\n"));
            self.speaker = Some(speaker);
        }
        self.text.push('\n');
        self.text.push_str(text);
        self.text.push('\n');
    }
}

/// What the user sent: the text in full, images as placeholders.
fn tool_image_placeholders(items: &[canter_protocol::ToolContentItem]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            canter_protocol::ToolContentItem::LocalImage { name, .. } => {
                Some(format!("[Image: {}]", sanitize(name)))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_text(content: &[UserInput]) -> String {
    let parts: Vec<String> = content
        .iter()
        .filter_map(|part| match part {
            UserInput::Skill { name } => Some(format!("Used skill `{}`", sanitize(name))),
            UserInput::Text { text } => Some(sanitize(text).trim_end().to_owned()),
            UserInput::LocalImage { path, name } => {
                let name = name.clone().or_else(|| {
                    Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                });
                Some(match name {
                    Some(name) => format!("[Image: {}]", one_line(&name, LINE_CHARS)),
                    None => "[Image]".to_owned(),
                })
            }
            UserInput::Unknown => None,
        })
        .filter(|part| !part.is_empty())
        .collect();
    parts.join("\n\n")
}

/// `**Tool** · <summary>`, then the first lines of `output` in a fence.
fn tool(summary: &str, output: &str, language: &str) -> String {
    let mut text = format!("**Tool** · {summary}");
    let output = sanitize(output);
    let lines: Vec<&str> = output.trim_end().lines().collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return text;
    }
    let mut kept: Vec<String> = lines
        .iter()
        .take(OUTPUT_LINES)
        .map(|line| cut(line, LINE_CHARS))
        .collect();
    if lines.len() > OUTPUT_LINES {
        kept.push(format!("… {} more lines", lines.len() - OUTPUT_LINES));
    }
    let kept = kept.join("\n");
    let fence = "`".repeat(longest_run(&kept, '`').max(2) + 1);
    text.push_str(&format!("\n\n{fence}{language}\n{kept}\n{fence}"));
    text
}

/// `Edited src/a.rs (+3 −1)`, `Created …`, `Deleted …`, `Moved a → b`.
fn file_change(change: &FileUpdateChange, cwd: &str) -> String {
    let path = code_span(&display_path(&change.path, cwd));
    let diff = Diff::parse(&change.diff, &change.kind, &change.path);
    let counts = format!("(+{} −{})", diff.added(), diff.removed());
    match &change.kind {
        PatchChangeKind::Add => format!("Created {path} {counts}"),
        PatchChangeKind::Delete => format!("Deleted {path}"),
        PatchChangeKind::Update {
            move_path: Some(to),
        } => format!(
            "Moved {path} → {} {counts}",
            code_span(&display_path(to, cwd))
        ),
        PatchChangeKind::Update { move_path: None } | PatchChangeKind::Unknown => {
            format!("Edited {path} {counts}")
        }
    }
}

/// How a tool call went, with the reason given when it was declined.
fn outcome(status: ItemStatus, feedback: Option<&str>) -> String {
    match status {
        ItemStatus::Interrupted => "interrupted".into(),
        ItemStatus::Blocked => "blocked (Plan mode)".into(),
        ItemStatus::Completed => "ok".into(),
        ItemStatus::Failed => "failed".into(),
        ItemStatus::Declined => match feedback {
            Some(feedback) => format!("declined: {}", one_line(feedback, LINE_CHARS)),
            None => "declined".into(),
        },
        ItemStatus::InProgress => "running".into(),
        ItemStatus::Unknown => "unknown".into(),
    }
}

/// `text` as inline code, with enough backticks around it.
fn code_span(text: &str) -> String {
    let ticks = "`".repeat(longest_run(text, '`') + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for next in text.chars() {
        run = if next == c { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// `text` on one line: the first line, then `…` when there was more.
fn one_line(text: &str, max: usize) -> String {
    let text = sanitize(text);
    let text = text.trim();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let line = cut(first, max);
    if lines.next().is_some() && !line.ends_with('…') {
        format!("{line} …")
    } else {
        line
    }
}

/// At most `max` characters, the last of them `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A session name fit for a file name: anything but letters, digits, `.`,
/// `_` and `-` becomes `-`, runs of them one; no `-` or `.` at either end.
fn file_safe(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
            c
        } else {
            '-'
        };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    let out: String = out.chars().take(NAME_CHARS).collect();
    out.trim_matches(|c| c == '-' || c == '.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use canter_protocol::{ErrorInfo, SessionSettings};

    fn turn(id: &str, input: &str, items: Vec<Item>) -> Turn {
        let input = vec![UserInput::Text { text: input.into() }];
        let mut all = vec![Item::UserMessage {
            id: format!("{id}-u"),
            content: input.clone(),
        }];
        all.extend(items);
        Turn {
            review: None,
            id: id.into(),
            items: all,
            status: TurnStatus::Completed,
            error: None,
            input,
            client_name: "canter-tui a".into(),
            usage: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }

    fn snapshot(name: Option<&str>, turns: Vec<Turn>) -> SessionSnapshot {
        let mut snapshot = SessionSnapshot::empty("4f2a-id");
        snapshot.settings = SessionSettings {
            tags: Default::default(),
            additional_directories: Vec::new(),
            plan_mode: false,
            pre_plan_approval_policy: None,
            cwd: "/work/proj".into(),
            name: name.map(str::to_owned),
            model: "deepseek-v4".into(),
            ..snapshot.settings
        };
        snapshot.turns = turns;
        snapshot
    }

    fn answer(id: &str, text: &str) -> Item {
        Item::AgentMessage {
            status: None,
            id: id.into(),
            text: text.into(),
        }
    }

    /// The whole conversation: the header, what was said in full, a line per
    /// tool call with the first lines of its output, no thinking, images as
    /// placeholders, and the reason a call was declined.
    #[test]
    fn markdown_has_the_conversation_and_tool_summaries_but_no_thinking() {
        let output: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let mut first = turn(
            "t1",
            "跑一下测试",
            vec![
                Item::Reasoning {
                    status: None,
                    id: "r1".into(),
                    summary: vec!["SECRET THOUGHT".into()],
                    content: vec!["SECRET CONTENT".into()],
                    duration_ms: None,
                },
                Item::CommandExecution {
                    id: "c1".into(),
                    source: Default::default(),
                    command: "cargo test".into(),
                    cwd: "/work/proj".into(),
                    status: ItemStatus::Completed,
                    aggregated_output: output,
                    exit_code: Some(0),
                    duration_ms: Some(1200),
                },
                Item::FileChange {
                    id: "f1".into(),
                    changes: vec![FileUpdateChange {
                        path: "/work/proj/src/a.rs".into(),
                        kind: PatchChangeKind::Update { move_path: None },
                        diff: "@@ -1,2 +1,2 @@\n-old\n+new\n same\n".into(),
                    }],
                    status: ItemStatus::Completed,
                },
                Item::WebFetch {
                    id: "w1".into(),
                    url: "https://example.com/doc".into(),
                    resolved_url: None,
                    result: "Example Domain".into(),
                    status: ItemStatus::Failed,
                },
                Item::CommandExecution {
                    id: "c2".into(),
                    source: Default::default(),
                    command: "rm -rf target".into(),
                    cwd: "/work/proj".into(),
                    status: ItemStatus::Declined,
                    aggregated_output: String::new(),
                    exit_code: None,
                    duration_ms: None,
                },
                answer("a1", "测试都过了。\n\n```rust\nfn main() {}\n```"),
            ],
        );
        first.input.push(UserInput::LocalImage {
            path: "/tmp/canter-img/shot.png".into(),
            name: None,
        });
        if let Item::UserMessage { content, .. } = &mut first.items[0] {
            *content = first.input.clone();
        }
        let mut second = turn("t2", "再说一遍", vec![answer("a2", "好的")]);
        second.status = TurnStatus::Failed;
        second.error = Some(ErrorInfo {
            message: "model unreachable".into(),
            additional_details: None,
        });
        let mut snapshot = snapshot(Some("跑测试"), vec![first, second]);
        snapshot
            .approval_feedback
            .insert("c2".into(), "keep the build".into());

        let text = markdown(&snapshot, "2026-10-27 14:30:05").unwrap();
        assert_eq!(
            text,
            "# 跑测试

- Directory: /work/proj
- Model: deepseek-v4
- Exported: 2026-10-27 14:30:05

## User

跑一下测试

[Image: shot.png]

## Assistant

**Tool** · Ran `cargo test` · ok, exit 0

```
line 1
line 2
line 3
line 4
line 5
line 6
line 7
line 8
… 4 more lines
```

**Tool** · Edited `src/a.rs` (+1 −1) · ok

```diff
@@ -1,2 +1,2 @@
-old
+new
 same
```

**Tool** · Fetched `https://example.com/doc` · failed

```
Example Domain
```

**Tool** · Ran `rm -rf target` · declined: keep the build

测试都过了。

```rust
fn main() {}
```

## User

再说一遍

## Assistant

好的

_Failed: model unreachable_
"
        );
        assert!(!text.contains("SECRET"));
    }

    /// Output with a code fence in it stays inside a longer fence; a command
    /// with backticks stays one inline code span.
    #[test]
    fn fences_and_code_spans_hold_backticks() {
        let snapshot = snapshot(
            None,
            vec![turn(
                "t1",
                "go",
                vec![Item::CommandExecution {
                    id: "c1".into(),
                    source: Default::default(),
                    command: "echo `date`\necho two".into(),
                    cwd: "/work/proj".into(),
                    status: ItemStatus::Completed,
                    aggregated_output: "```\ninside\n```\n".into(),
                    exit_code: Some(0),
                    duration_ms: None,
                }],
            )],
        );
        let text = markdown(&snapshot, "now").unwrap();
        assert!(text.starts_with("# 4f2a-id\n"), "{text}");
        assert!(
            text.contains(
                "**Tool** · Ran ``echo `date` …`` · ok, exit 0\n\n````\n```\ninside\n```\n````\n"
            ),
            "{text}"
        );
    }

    /// A search that found nothing is exported as `No matches`, not as
    /// `failed, exit 1`; another command's exit code 1 stays a failure.
    #[test]
    fn a_search_that_finds_nothing_is_not_exported_as_failed() {
        let snapshot = snapshot(
            None,
            vec![turn(
                "t1",
                "search",
                vec![
                    Item::CommandExecution {
                        id: "c1".into(),
                        source: Default::default(),
                        command: "rg missing".into(),
                        cwd: "/work/proj".into(),
                        status: ItemStatus::Failed,
                        aggregated_output: String::new(),
                        exit_code: Some(1),
                        duration_ms: None,
                    },
                    Item::CommandExecution {
                        id: "c2".into(),
                        source: Default::default(),
                        command: "diff a b".into(),
                        cwd: "/work/proj".into(),
                        status: ItemStatus::Failed,
                        aggregated_output: "< a\n> b\n".into(),
                        exit_code: Some(1),
                        duration_ms: None,
                    },
                ],
            )],
        );
        let text = markdown(&snapshot, "now").unwrap();
        assert!(
            text.contains("**Tool** · Ran `rg missing` · No matches\n"),
            "{text}"
        );
        assert!(
            text.contains("**Tool** · Ran `diff a b` · failed, exit 1\n"),
            "{text}"
        );
    }

    #[test]
    fn wake_event_export_keeps_all_matching_lines_and_pause_notice() {
        let lines = (1..=20).map(|i| format!("error {i}\n")).collect::<String>();
        let mut wake = turn("wake", "", vec![]);
        wake.input.clear();
        wake.items = vec![
            Item::BackgroundEvent {
                id: "event".into(),
                command_id: Some(1),
                text: lines.clone(),
            },
            Item::BackgroundEvent {
                id: "limit".into(),
                command_id: None,
                text: "Auto wake paused: send a message to resume.".into(),
            },
        ];
        let exported = markdown(&snapshot(None, vec![wake]), "now").unwrap();
        assert!(
            exported.contains(&lines) && exported.contains("Auto wake paused"),
            "{exported}"
        );
        assert!(
            exported.contains("Event") && !exported.contains("User"),
            "{exported}"
        );
    }

    #[test]
    fn background_export_uses_process_state_instead_of_tool_call_state() {
        let mut snapshot = snapshot(
            None,
            vec![turn(
                "t1",
                "go",
                vec![Item::CommandExecution {
                    id: "c1".into(),
                    source: Default::default(),
                    command: "long-test".into(),
                    cwd: "/work/proj".into(),
                    status: ItemStatus::Completed,
                    aggregated_output: "ready".into(),
                    exit_code: None,
                    duration_ms: Some(1000),
                }],
            )],
        );
        snapshot
            .background_commands
            .push(canter_protocol::BackgroundCommand {
                id: 1,
                item_id: "c1".into(),
                command: "long-test".into(),
                started_at: 0,
                completed_at: None,
                running: true,
                exit_code: None,
            });
        assert!(
            markdown(&snapshot, "now")
                .unwrap()
                .contains("Ran `long-test` · running")
        );
        let command = &mut snapshot.background_commands[0];
        command.running = false;
        command.exit_code = Some(7);
        command.completed_at = Some(2000);
        let text = markdown(&snapshot, "now").unwrap();
        assert!(text.contains("Ran `long-test` · failed, exit 7"), "{text}");
        assert!(!text.contains("running"));
    }

    #[test]
    fn nothing_said_is_nothing_to_export() {
        assert_eq!(markdown(&snapshot(Some("x"), Vec::new()), "now"), None);
    }

    #[test]
    fn default_name_uses_a_file_safe_session_name_or_the_id() {
        let named = snapshot(Some("修 bug: a/b <c>?"), Vec::new());
        assert_eq!(
            default_name(&named, "20261027-143005"),
            "canter-修-bug-a-b-c-20261027-143005.md"
        );
        let unnamed = snapshot(None, Vec::new());
        assert_eq!(
            default_name(&unnamed, "20261027-143005"),
            "canter-4f2a-id-20261027-143005.md"
        );
        let dots = snapshot(Some("../.."), Vec::new());
        assert_eq!(default_name(&dots, "t"), "canter-4f2a-id-t.md");
    }

    #[test]
    fn paths_are_relative_to_the_session_directory() {
        assert_eq!(
            resolve("notes/out.md", "/work/proj", Some("/home/me")),
            PathBuf::from("/work/proj/notes/out.md")
        );
        assert_eq!(
            resolve("/tmp/out.md", "/work/proj", None),
            PathBuf::from("/tmp/out.md")
        );
        assert_eq!(
            resolve("~/out.md", "/work/proj", Some("/home/me")),
            PathBuf::from("/home/me/out.md")
        );
    }

    #[test]
    fn writing_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.md");
        write(&path, "first").unwrap();
        let error = write(&path, "second").unwrap_err();
        assert!(
            error.ends_with("already exists; not overwritten"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        let error = write(&dir.path().join("missing/out.md"), "x").unwrap_err();
        assert!(error.starts_with("cannot create"), "{error}");
    }

    /// P2-135: the diagnostics line goes with its file change, a late result
    /// is its own line.
    #[test]
    fn diagnostics_are_exported_as_their_lines() {
        let item = |value: serde_json::Value| -> Item { serde_json::from_value(value).unwrap() };
        let file = serde_json::json!([{ "batchId": 1, "path": "src/a.rs", "serverId": "lsp-1",
            "source": "push", "freshness": "current", "incomplete": false, "diagnostics": [] }]);
        let first = turn(
            "t1",
            "fix it",
            vec![
                item(
                    serde_json::json!({ "type": "fileChange", "id": "f1", "status": "completed",
                    "changes": [{ "path": "/work/proj/src/a.rs",
                        "kind": { "type": "update", "movePath": null }, "diff": "-a\n+b\n" }] }),
                ),
                item(
                    serde_json::json!({ "type": "diagnostics", "id": "d1", "sourceItemId": "f1",
                    "status": "available", "files": file, "totalCount": 2, "shownCount": 0,
                    "omittedCount": 0, "errorCount": 2, "warningCount": 0, "delivery": "inline" }),
                ),
                answer("a1", "Edited."),
                item(
                    serde_json::json!({ "type": "diagnostics", "id": "d2", "sourceItemId": "f1",
                    "status": "available", "files": file, "totalCount": 0, "shownCount": 0,
                    "omittedCount": 0, "errorCount": 0, "warningCount": 0, "delivery": "late" }),
                ),
            ],
        );
        let text = markdown(&snapshot(None, vec![first]), "now").unwrap();
        assert!(text.contains("\nLSP: 2 errors\n"), "{text}");
        assert!(
            text.contains("\nLSP update: src/a.rs · no errors or warnings\n"),
            "{text}"
        );
    }
}
