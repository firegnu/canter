//! Optional side requests. Network work never owns the session state lock.
use crate::{
    Session, context,
    session::{Shared, State},
};
use canter_model::Message;
use canter_protocol::{BtwEvent, EventPayload, RequestPurpose};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

const PROMPT: &str = "Suggest the user's most likely next prompt, in the user's language. Return only one short sentence on a single line, at most 200 characters. Do not call tools, explain, quote, or add formatting. If there is no obvious useful next step, return nothing.";

#[derive(Default)]
pub(crate) struct Suggestions {
    pub active: Option<Active>,
    unused: u32,
    reduced_rounds: u32,
    example_started: bool,
}
pub(crate) struct Active {
    pub cancel: CancellationToken,
    pub done: watch::Receiver<bool>,
}
impl Suggestions {
    pub fn used(&mut self) {
        self.unused = 0;
        self.reduced_rounds = 0;
    }
    pub fn cancel(&self) {
        if let Some(active) = &self.active {
            active.cancel.cancel();
        }
    }
}

fn eligible(shared: &Shared, state: &State) -> bool {
    shared.runtime().options.prompt_suggestions
        && state.btw.is_none()
        && state.active.is_none()
        && !state.closing
        && !state.journal.snapshot.settings.plan_mode
        && state.journal.snapshot.parent.is_none()
        && state.requests.is_empty()
        && state.journal.snapshot.agent_requests.is_empty()
        && state.queue.is_empty()
        && state.goal_deadline.is_none()
        && state.goal_summary.is_none()
        && !state
            .background_events
            .has_pending_wake(shared.runtime().options.auto_wake_limit)
}

pub(crate) fn start(shared: &Shared, state: &mut State, turn_id: String) {
    if !eligible(shared, state)
        || state.suggestions.active.is_some()
        || shared
            .input_waiters
            .load(std::sync::atomic::Ordering::SeqCst)
            != 0
    {
        return;
    }
    if state.suggestions.unused >= 20 {
        state.suggestions.reduced_rounds += 1;
        if state.suggestions.reduced_rounds == 1 {
            state
                .journal
                .ephemeral(EventPayload::SessionPromptSuggestionsReduced);
        }
        if !state.suggestions.reduced_rounds.is_multiple_of(4) {
            return;
        }
    }
    let mut request = context::request(state, shared.runtime().options.max_output_tokens);
    request.messages.push(Message::User {
        text: PROMPT.into(),
    });
    let shared = shared.frozen();
    let cancel = shared.lifetime.child_token();
    let (done, receiver) = watch::channel(false);
    state.suggestions.active = Some(Active {
        cancel: cancel.clone(),
        done: receiver,
    });
    tokio::spawn(async move {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let answer = async {
            let result = crate::btw::answer(
                &shared,
                request,
                RequestPurpose::Suggestion,
                &cancel,
                &sender,
            )
            .await;
            drop(sender);
            result
        };
        let collect = async {
            let mut text = String::new();
            while let Some(event) = receiver.recv().await {
                match event {
                    BtwEvent::Delta { delta } => {
                        let remaining = 201usize.saturating_sub(text.chars().count());
                        text.extend(delta.chars().take(remaining));
                    }
                    BtwEvent::Replace {
                        text: replacement, ..
                    } => text = replacement.chars().take(201).collect(),
                    _ => {}
                }
            }
            text
        };
        let (result, text) = tokio::join!(answer, collect);
        let mut state = shared.state.lock().await;
        if result.is_ok()
            && !cancel.is_cancelled()
            && eligible(&shared, &state)
            && state
                .journal
                .snapshot
                .turns
                .last()
                .is_some_and(|turn| turn.id == turn_id)
            && let Some(text) = valid_text(&text)
        {
            state
                .journal
                .ephemeral(EventPayload::SessionPromptSuggestion {
                    turn_id: Some(turn_id),
                    text,
                });
            state.suggestions.unused = state.suggestions.unused.saturating_add(1);
        }
        let _ = state.journal.flush_private_telemetry().await;
        state.suggestions.active = None;
        done.send_replace(true);
    });
}

fn valid_text(text: &str) -> Option<String> {
    // The collector retains 201 characters as an overflow sentinel. Check it
    // before trimming, so a space at the boundary cannot hide a longer reply.
    if text.chars().count() > 200 {
        return None;
    }
    let text = text.trim();
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

impl Session {
    pub(crate) async fn suggest_example(&self) {
        if self.shared.runtime().context.restricted_mode {
            return;
        }
        let mut state = self.shared.state.lock().await;
        if !self.shared.next_options().prompt_suggestions
            || state.closing
            || state.suggestions.example_started
            || state.suggestions.active.is_some()
            || state.btw.is_some()
            || state.active.is_some()
            || !state.history.is_empty()
            || !state.journal.snapshot.turns.is_empty()
            || state.journal.snapshot.parent.is_some()
        {
            return;
        }
        state.suggestions.example_started = true;
        let cwd = state.journal.snapshot.settings.cwd.clone();
        let environment = self.shared.runtime().context.environment.clone();
        let shared = self.shared.frozen();
        let cancel = shared.lifetime.child_token();
        let (done, receiver) = watch::channel(false);
        state.suggestions.active = Some(Active {
            cancel: cancel.clone(),
            done: receiver,
        });
        tokio::spawn(async move {
            let text = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                result = tokio::time::timeout(std::time::Duration::from_secs(2), git_example(&cwd, &environment)) => result.ok().flatten(),
            };
            let mut state = shared.state.lock().await;
            if !cancel.is_cancelled()
                && !state.closing
                && state.history.is_empty()
                && state.active.is_none()
                && state.journal.snapshot.turns.is_empty()
                && let Some(text) = text
            {
                state
                    .journal
                    .ephemeral(EventPayload::SessionPromptSuggestion {
                        turn_id: None,
                        text,
                    });
            }
            state.suggestions.active = None;
            done.send_replace(true);
        });
    }
}

async fn git_example(
    cwd: &str,
    environment: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "log",
            "--no-show-signature",
            "-30",
            "--format=",
            "--name-only",
            "-z",
            "--relative",
            "--diff-filter=AM",
            "--",
            ".",
        ])
        .current_dir(cwd)
        .env_clear()
        .envs(environment)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    // Even a large initial commit must not allocate unbounded output.
    const MAX_BYTES: u64 = 512 * 1024;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() as u64 > MAX_BYTES || !child.wait().await.ok()?.success() {
        return None;
    }
    let log = String::from_utf8(bytes).ok()?;
    let mut counts = std::collections::HashMap::new();
    for (index, path) in log.split('\0').enumerate() {
        let path = path.trim_start_matches('\n');
        if path.is_empty() || path.chars().any(char::is_control) {
            continue;
        }
        let entry = counts.entry(path).or_insert((0usize, index));
        entry.0 += 1;
    }
    let mut files: Vec<_> = counts.into_iter().collect();
    files.sort_by_key(|(_, (count, index))| (std::cmp::Reverse(*count), *index));
    for (file, _) in files {
        if tokio::fs::metadata(std::path::Path::new(cwd).join(file))
            .await
            .is_ok_and(|m| m.is_file())
            && let Some(text) = valid_text(&format!("explain how {file} works"))
        {
            return Some(text);
        }
    }
    None
}
