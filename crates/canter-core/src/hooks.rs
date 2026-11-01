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
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    fields
}

pub(crate) async fn run(
    shared: &Shared,
    event: Event,
    fields: Value,
    cancel: &CancellationToken,
) -> Result<Option<String>, CoreError> {
    let runtime = shared.runtime();
    if runtime.context.restricted_mode {
        return Ok(None);
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
) -> Result<Option<String>, CoreError> {
    let Some(groups) = runtime.options.hooks.0.get(&event) else {
        return Ok(None);
    };
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let commands = groups
        .iter()
        .filter(|g| g.matcher.as_ref().is_none_or(|m| m.is_match(tool)))
        .flat_map(|g| &g.hooks);
    let results = join_all(commands.map(|command| {
        let input = &input;
        async move { (command, execute(command, input, cancel).await) }
    }))
    .await;
    let mut reasons = Vec::new();
    let mut warnings = Vec::new();
    for (command, result) in results {
        match result {
            Ok(Some(reason)) => reasons.push(reason),
            Ok(None) => {}
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
    Ok((!reasons.is_empty()).then(|| reasons.join("\n")))
}

pub(crate) fn notify(
    shared: &Shared,
    state: &State,
    event: Event,
    fields: Value,
    cancel: CancellationToken,
) {
    let runtime = shared.runtime();
    if runtime.context.restricted_mode || !runtime.options.hooks.has(event) {
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
    join_all(tasks).await;
}

pub(crate) fn tool_fields(call: &canter_model::ToolCall) -> Value {
    json!({"tool_name":call.name,"tool_input":serde_json::from_str::<Value>(&call.arguments).unwrap_or_default()})
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
        let pending = self
            .state
            .lock()
            .await
            .active
            .as_mut()
            .expect("active turn")
            .pending_prompt
            .take();
        let Some((input, skills)) = pending else {
            return Ok(());
        };
        let reason = run(
            self,
            Event::UserPromptSubmit,
            json!({"prompt":prompt_text(&input)}),
            cancel,
        )
        .await?;
        let mut state = self.state.lock().await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(reason) = reason {
            state.active.as_mut().unwrap().prompt_blocked = true;
            state
                .journal
                .append(vec![EventPayload::Warning {
                    message: format!("UserPromptSubmit blocked: {}", one_line(&reason)),
                }])
                .await?;
        } else {
            let turn_id = state.active.as_ref().unwrap().turn.id.clone();
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
            let reason = run(
                self,
                Event::UserPromptSubmit,
                json!({"prompt":prompt_text(&input.input)}),
                cancel,
            )
            .await?;
            if cancel.is_cancelled() {
                break;
            }
            if let Some(reason) = reason {
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
                accepted.push(input);
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
    cancel: &CancellationToken,
) -> Result<Option<String>, String> {
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let mut child = tokio::process::Command::new("/bin/sh")
        .args(["-c", &command.command])
        .current_dir(input["cwd"].as_str().unwrap())
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
        _ = cancel.cancelled() => return Ok(None),
        result = tokio::time::timeout(Duration::from_secs_f64(command.timeout), work) => result,
    }
    .map_err(|_| "timed out".to_string())?
    .map_err(|e| format!("process failed: {e}"))?;
    let (status, stdout, stderr) = result;
    match status.code() {
        Some(2) => return Ok(Some(String::from_utf8_lossy(&stderr).trim().to_owned())),
        Some(0) => {}
        code => return Err(format!("unexpected exit status {code:?}")),
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let output: Value =
        serde_json::from_slice(&stdout).map_err(|_| "invalid JSON output".to_string())?;
    if output["decision"] == "block" {
        return Ok(Some(
            output["reason"]
                .as_str()
                .unwrap_or("Blocked by hook")
                .to_owned(),
        ));
    }
    if input["hook_event_name"] == "PreToolUse"
        && output["hookSpecificOutput"]["permissionDecision"] == "deny"
    {
        return Ok(Some(
            output["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("Blocked by hook")
                .to_owned(),
        ));
    }
    Ok(None)
}
