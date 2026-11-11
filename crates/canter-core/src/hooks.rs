//! Global command hooks. Reference: Claude Code hooks and Codex rust-v0.163.0
//! hooks/src/schema.rs (field names only; no upstream code is copied).
use crate::{
    CoreError,
    session::{Shared, State},
};
use canter_protocol::{EventPayload, PermissionMode};
use futures_util::future::join_all;
use regex_automata::meta::Regex;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const OUTPUT_LIMIT: usize = 32 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Output {
    reason: Option<String>,
    context: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Event {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    Stop,
    StopFailure,
    Interrupt,
    SubagentStart,
    SubagentStop,
}
impl Event {
    fn tool(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::PermissionRequest | Self::PostToolUse
        )
    }
}

#[derive(Clone, Default)]
pub struct Hooks(BTreeMap<Event, Vec<Group>>);
// Commands can contain credentials; do not expose them through CoreOptions' Debug.
impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}
#[derive(Clone)]
struct Group {
    matcher: Option<Regex>,
    hooks: Vec<Command>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    #[serde(rename = "type")]
    _kind: CommandType,
    command: String,
    #[serde(default = "default_timeout")]
    timeout: f64,
}
#[derive(Clone, Deserialize)]
enum CommandType {
    #[serde(rename = "command")]
    Command,
}
fn default_timeout() -> f64 {
    60.0
}

impl Hooks {
    pub(crate) fn has(&self, event: Event) -> bool {
        self.0
            .get(&event)
            .is_some_and(|groups| groups.iter().any(|g| !g.hooks.is_empty()))
    }

    pub(crate) fn matches(&self, event: Event, tool: &str) -> bool {
        self.0.get(&event).is_some_and(|groups| {
            groups
                .iter()
                .any(|g| !g.hooks.is_empty() && g.matcher.as_ref().is_none_or(|m| m.is_match(tool)))
        })
    }
}

impl<'de> Deserialize<'de> for Hooks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Entry {
            #[serde(default)]
            matcher: String,
            hooks: Vec<Command>,
        }
        let entries = BTreeMap::<Event, Vec<Entry>>::deserialize(deserializer)?;
        let mut result = BTreeMap::new();
        for (event, entries) in entries {
            let mut groups = Vec::new();
            for entry in entries {
                let matcher = if event.tool() && !matches!(entry.matcher.as_str(), "" | "*") {
                    Some(
                        Regex::new(&format!("\\A(?:{})\\z", entry.matcher))
                            .map_err(|_| serde::de::Error::custom("invalid hook matcher"))?,
                    )
                } else {
                    None
                };
                for hook in &entry.hooks {
                    if hook.command.trim().is_empty()
                        || hook.timeout <= 0.0
                        || Duration::try_from_secs_f64(hook.timeout).is_err()
                    {
                        return Err(serde::de::Error::custom(
                            "hooks require a command and a positive timeout in seconds",
                        ));
                    }
                }
                groups.push(Group {
                    matcher,
                    hooks: entry.hooks,
                });
            }
            result.insert(event, groups);
        }
        Ok(Self(result))
    }
}

pub(crate) fn input(state: &State, event: Event, mut fields: Value) -> Value {
    let transcript_path = std::path::absolute(
        state
            .journal
            .store
            .session_dir(&state.journal.snapshot.id)
            .expect("valid session id")
            .join("events.jsonl"),
    )
    .expect("absolute session transcript path");
    let settings = &state.journal.snapshot.settings;
    let mode = if settings.plan_mode {
        "plan"
    } else {
        match state.approval_policy() {
            PermissionMode::AlwaysAsk => "alwaysAsk",
            PermissionMode::AskForExecution => "askForExecution",
            PermissionMode::AllowAll => "allowAll",
            PermissionMode::Unknown => "unknown",
        }
    };
    fields.as_object_mut().expect("hook fields").extend(
        json!({
            "session_id":state.journal.snapshot.id, "cwd":settings.cwd,
            "hook_event_name":format!("{event:?}"), "permission_mode":mode,
            "transcript_path":transcript_path,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    if let Some(parent) = &state.journal.snapshot.parent {
        fields["agent_id"] = json!(state.journal.snapshot.id);
        fields["agent_name"] = json!(settings.name);
        fields["agent_type"] = json!(parent.agent_type);
        fields["parent_session_id"] = json!(parent.session_id);
    }
    fields
}

pub(crate) async fn run(
    shared: &Shared,
    event: Event,
    fields: Value,
    cancel: &CancellationToken,
) -> Result<Option<String>, CoreError> {
    Ok(run_context(shared, event, fields, cancel).await?.reason)
}

async fn run_context(
    shared: &Shared,
    event: Event,
    fields: Value,
    cancel: &CancellationToken,
) -> Result<Output, CoreError> {
    let runtime = shared.runtime();
    if runtime.context.restricted_mode {
        return Ok(Output::default());
    }
    let input = input(&*shared.state.lock().await, event, fields);
    run_input(shared, &runtime, event, input, cancel).await
}

async fn run_input(
    shared: &Shared,
    runtime: &crate::configuration::Runtime,
    event: Event,
    input: Value,
    cancel: &CancellationToken,
) -> Result<Output, CoreError> {
    let Some(groups) = runtime.options.hooks.0.get(&event) else {
        return Ok(Output::default());
    };
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let commands = groups
        .iter()
        .filter(|g| g.matcher.as_ref().is_none_or(|m| m.is_match(tool)))
        .flat_map(|g| &g.hooks);
    let results = join_all(commands.map(|command| {
        let input = &input;
        async move {
            (
                command,
                execute(command, input, &runtime.context.environment, cancel).await,
            )
        }
    }))
    .await;
    let mut reasons = Vec::new();
    let mut contexts = Vec::new();
    let mut warnings = Vec::new();
    for (command, result) in results {
        match result {
            Ok(output) => {
                reasons.extend(output.reason);
                contexts.extend(output.context);
            }
            Err(error) => warnings.push(EventPayload::Warning {
                message: format!(
                    "{event:?} hook `{}`: {}. Continuing.",
                    one_line(&command.command),
                    one_line(&error)
                ),
            }),
        }
    }
    if !warnings.is_empty() {
        shared.state.lock().await.journal.append(warnings).await?;
    }
    Ok(Output {
        reason: (!reasons.is_empty()).then(|| reasons.join("\n")),
        context: (!contexts.is_empty()).then(|| contexts.join("\n")),
    })
}

/// Opening stays asynchronous. Only a model-bound turn waits for this result.
pub(crate) fn session_start(shared: &Shared, state: &mut State, source: &str) {
    let runtime = shared.runtime();
    if runtime.context.restricted_mode
        || state.journal.snapshot.parent.is_some()
        || !runtime.options.hooks.has(Event::SessionStart)
    {
        return;
    }
    let input = input(state, Event::SessionStart, json!({"source":source}));
    let frozen = shared.frozen();
    let cancel = shared.lifetime.child_token();
    let (sender, receiver) = tokio::sync::watch::channel(None);
    state.session_start_hook = Some(receiver);
    let task = tokio::spawn(async move {
        let context = run_input(&frozen, &runtime, Event::SessionStart, input, &cancel)
            .await
            .ok()
            .and_then(|output| output.context);
        sender.send_replace(context);
    });
    let mut tasks = shared.hook_tasks.lock().unwrap();
    tasks.retain(|task| !task.is_finished());
    tasks.push(task);
}

fn context_header(event: &str) -> String {
    format!(
        "[Hook context: {event}]\nInternal context supplied by a hook, not the user's own words.\n\n"
    )
}

pub(crate) fn context_message(event: &str, text: &str) -> canter_model::Message {
    canter_model::Message::User {
        text: format!("{}{text}", context_header(event)),
    }
}

pub(crate) fn retracted_start_contexts(
    history: &[canter_model::Message],
    hook_contexts: &std::collections::BTreeSet<usize>,
    boundary: usize,
) -> Vec<String> {
    let header = context_header("SessionStart");
    // Only private HookContext indices qualify; user-authored markers do not.
    hook_contexts
        .range(boundary..)
        .filter_map(|&index| {
            let canter_model::Message::User { text } = &history[index] else {
                return None;
            };
            text.strip_prefix(&header).map(str::to_owned)
        })
        .collect()
}

impl State {
    pub(crate) async fn hook_context(
        &mut self,
        event: &str,
        text: String,
        turn_id: &str,
    ) -> Result<(), CoreError> {
        let message = context_message(event, &text);
        self.journal
            .record(crate::context::HistoryEntry::HookContext {
                event: event.into(),
                text: text.clone(),
            })
            .await?;
        self.hook_contexts.insert(self.history.len());
        self.history.push(message);
        self.journal
            .append(vec![EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item: canter_protocol::Item::HookContext {
                    id: crate::session::id("hook-context"),
                    event: event.into(),
                    char_count: text.chars().count() as u64,
                },
            }])
            .await?;
        Ok(())
    }

    pub(crate) async fn session_start_context(&mut self, turn_id: &str) -> Result<(), CoreError> {
        while let Some(text) = self.retracted_session_start.front().cloned() {
            self.hook_context("SessionStart", text, turn_id).await?;
            self.retracted_session_start.pop_front();
        }
        // A completed sender is dropped even on cancellation or execution failure.
        if self
            .session_start_hook
            .as_ref()
            .is_some_and(|receiver| !matches!(receiver.has_changed(), Ok(false)))
        {
            let context = self.session_start_hook.take().unwrap().borrow().clone();
            if let Some(text) = context {
                self.hook_context("SessionStart", text, turn_id).await?;
            }
        }
        Ok(())
    }
}

pub(crate) fn notify(
    shared: &Shared,
    state: &State,
    event: Event,
    fields: Value,
    cancel: CancellationToken,
) {
    let runtime = shared.runtime();
    if runtime.context.restricted_mode
        || !runtime.options.hooks.has(event)
        || (state.journal.snapshot.parent.is_some()
            && matches!(
                event,
                Event::SessionStart
                    | Event::SessionEnd
                    | Event::Stop
                    | Event::StopFailure
                    | Event::Interrupt
            ))
    {
        return;
    }
    let input = input(state, event, fields);
    let frozen = shared.frozen();
    let task = tokio::spawn(async move {
        let _ = run_input(&frozen, &runtime, event, input, &cancel).await;
    });
    let mut tasks = shared.hook_tasks.lock().unwrap();
    tasks.retain(|task| !task.is_finished());
    tasks.push(task);
}

pub(crate) async fn drain(shared: &Shared) {
    let tasks = std::mem::take(&mut *shared.hook_tasks.lock().unwrap());
    let aborts: Vec<_> = tasks.iter().map(|task| task.abort_handle()).collect();
    let drain = join_all(tasks);
    tokio::pin!(drain);
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut drain)
        .await
        .is_err()
    {
        for task in aborts {
            task.abort();
        }
        // Await cancellation so every command's process-group guard has run.
        drain.await;
    }
}

pub(crate) fn tool_fields(call: &canter_model::ToolCall) -> Value {
    json!({"tool_name":call.name,"tool_input":serde_json::from_str::<Value>(&call.arguments).unwrap_or_default()})
}

pub(crate) fn last_assistant_message(state: &State, turn_id: &str) -> String {
    state
        .journal
        .snapshot
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .and_then(|turn| {
            turn.items.iter().rev().find_map(|item| match item {
                canter_protocol::Item::AgentMessage { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

fn prompt_text(input: &[canter_protocol::UserInput]) -> String {
    input
        .iter()
        .filter_map(|part| match part {
            canter_protocol::UserInput::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Shared {
    pub(crate) async fn check_prompt(&self, cancel: &CancellationToken) -> Result<(), CoreError> {
        let receiver = {
            let state = self.state.lock().await;
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.user_shell.is_some())
            {
                return Ok(());
            }
            state.session_start_hook.clone()
        };
        if let Some(mut receiver) = receiver {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                _ = receiver.changed() => {},
            }
        }
        let pending = self
            .state
            .lock()
            .await
            .active
            .as_mut()
            .expect("active turn")
            .pending_prompt
            .take();
        let Some((input, skills, check_hook)) = pending else {
            let mut state = self.state.lock().await;
            if !cancel.is_cancelled() {
                let turn_id = state.active.as_ref().unwrap().turn.id.clone();
                state.session_start_context(&turn_id).await?;
            }
            return Ok(());
        };
        let output = if check_hook {
            run_context(
                self,
                Event::UserPromptSubmit,
                json!({"prompt":prompt_text(&input)}),
                cancel,
            )
            .await?
        } else {
            Output::default()
        };
        let mut state = self.state.lock().await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(reason) = output.reason {
            state.active.as_mut().unwrap().prompt_blocked = true;
            state
                .journal
                .append(vec![EventPayload::Warning {
                    message: format!("UserPromptSubmit blocked: {}", one_line(&reason)),
                }])
                .await?;
        } else {
            let turn_id = state.active.as_ref().unwrap().turn.id.clone();
            state.session_start_context(&turn_id).await?;
            if let Some(text) = output.context {
                state
                    .hook_context("UserPromptSubmit", text, &turn_id)
                    .await?;
            }
            state.user_input(&input, &turn_id, skills).await?;
            state.active.as_mut().unwrap().turn.input = input.clone();
            state
                .journal
                .append(vec![EventPayload::ItemCompleted {
                    turn_id,
                    item: canter_protocol::Item::UserMessage {
                        id: crate::session::id("user"),
                        content: input,
                    },
                }])
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn consume_steering(
        &self,
        turn_id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, CoreError> {
        let inputs = self.state.lock().await.steers.clone();
        let mut accepted = Vec::new();
        for input in inputs {
            if cancel.is_cancelled() {
                break;
            }
            let output = run_context(
                self,
                Event::UserPromptSubmit,
                json!({"prompt":prompt_text(&input.input)}),
                cancel,
            )
            .await?;
            if cancel.is_cancelled() {
                break;
            }
            if let Some(reason) = output.reason {
                let mut state = self.state.lock().await;
                state
                    .journal
                    .append(vec![
                        EventPayload::InputRemoved {
                            input_ids: vec![input.id.clone()],
                            reason: canter_protocol::InputRemovalReason::Blocked,
                            turn_id: Some(turn_id.into()),
                        },
                        EventPayload::Warning {
                            message: format!("UserPromptSubmit blocked: {}", one_line(&reason)),
                        },
                    ])
                    .await?;
                state.steers.retain(|queued| queued.id != input.id);
                state.queued_skills.remove(&input.id);
            } else {
                accepted.push((input, output.context));
            }
        }
        let consumed = !accepted.is_empty();
        let mut state = self.state.lock().await;
        if cancel.is_cancelled() {
            return Ok(false);
        }
        state.consume_steering(turn_id, accepted).await?;
        Ok(consumed)
    }
}

pub(crate) fn one_line(text: &str) -> String {
    text.chars()
        .take(512)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

// The guard also kills descendants when the future is dropped or the leader
// exits while a child still has the pipes open. The PID is only our own child.
struct ProcessGroup(i32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
async fn capture(mut pipe: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let n = pipe.read(&mut buffer).await?;
        if n == 0 {
            return Ok(bytes);
        }
        let keep = n.min(OUTPUT_LIMIT - bytes.len());
        bytes.extend_from_slice(&buffer[..keep]);
    }
}
async fn execute(
    command: &Command,
    input: &Value,
    environment: &BTreeMap<String, String>,
    cancel: &CancellationToken,
) -> Result<Output, String> {
    if cancel.is_cancelled() {
        return Ok(Output::default());
    }
    let mut child = tokio::process::Command::new("/bin/sh")
        .args(["-c", &command.command])
        .current_dir(input["cwd"].as_str().unwrap())
        .env_clear()
        .envs(environment)
        .env("CANTER_SESSION_ID", input["session_id"].as_str().unwrap())
        .env("CANTER_PROJECT_DIR", input["cwd"].as_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start: {e}"))?;
    let group = ProcessGroup(child.id().unwrap() as i32);
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let work = async {
        let write = async {
            let _ = stdin.write_all(input.to_string().as_bytes()).await;
            drop(stdin);
        };
        let wait = async {
            let status = child.wait().await;
            drop(group);
            status
        };
        let ((), status, stdout, stderr) =
            tokio::join!(write, wait, capture(stdout), capture(stderr));
        Ok::<_, std::io::Error>((status?, stdout?, stderr?))
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(Output::default()),
        result = tokio::time::timeout(Duration::from_secs_f64(command.timeout), work) => result,
    }
    .map_err(|_| "timed out".to_string())?
    .map_err(|e| format!("process failed: {e}"))?;
    let (status, stdout, stderr) = result;
    match status.code() {
        Some(2) => {
            return Ok(Output {
                reason: Some(String::from_utf8_lossy(&stderr).trim().to_owned()),
                context: None,
            });
        }
        Some(0) => {}
        code => return Err(format!("unexpected exit status {code:?}")),
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Output::default());
    }
    let output: Value =
        serde_json::from_slice(&stdout).map_err(|_| "invalid JSON output".to_string())?;
    let reason = if output["decision"] == "block" {
        Some(
            output["reason"]
                .as_str()
                .unwrap_or("Blocked by hook")
                .to_owned(),
        )
    } else if input["hook_event_name"] == "PreToolUse"
        && output["hookSpecificOutput"]["permissionDecision"] == "deny"
    {
        Some(
            output["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("Blocked by hook")
                .to_owned(),
        )
    } else {
        None
    };
    let context = if matches!(
        input["hook_event_name"].as_str(),
        Some("SessionStart" | "UserPromptSubmit")
    ) {
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    } else {
        None
    };
    Ok(Output { reason, context })
}
