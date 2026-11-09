//! Send the task's chat at the moments the task writes: after counted commits,
//! or after turns of one session (the opening turn, then each chat's own turn,
//! and a sub-agent report waking the session).

use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use canter_protocol::{AgentStatus, Event, EventPayload, Item, SessionStatus};

use crate::{
    Result,
    paths::{State, StateRepo},
    session_data::{self, ChatRecord, Progress},
    task::{ChatAfter, Task},
};

const POLL: Duration = Duration::from_secs(2);
/// The whole chat schedule gives up after this long; the run continues without
/// the lines it could not send.
const TIMEOUT: Duration = Duration::from_secs(1800);

pub struct SteerOptions {
    pub home: crate::paths::Home,
    pub round: String,
    pub run: String,
    /// One-based session of a multi-session task.
    pub session: usize,
    pub poll: Duration,
    /// How long the chat schedule may wait before giving up.
    pub timeout: Duration,
}

impl SteerOptions {
    pub fn new(home: crate::paths::Home, round: String, run: String, session: usize) -> Self {
        Self {
            home,
            round,
            run,
            session,
            poll: POLL,
            timeout: TIMEOUT,
        }
    }
}

pub async fn steer(options: SteerOptions) -> Result<()> {
    let state = State::load(&options.home, &options.round, &options.run)?;
    let task = state.task()?;
    let session_count = state.session_count.max(1);
    if options.session == 0 || options.session > session_count {
        return Err(format!(
            "--session must be between 1 and {session_count} for this run"
        ));
    }
    let registration = crate::server::registration(&options.home)?;
    // Each repository has its own worktree; a task without a main repository
    // runs its session in the directory between them.
    let cwd = state.session_cwd(options.session)?;
    let progress_path = options
        .home
        .steer_progress_path(&options.round, &options.run);
    let mut progress = read_progress(&progress_path)?;
    // The whole chat schedule, including finding the session, shares one time
    // limit; a task whose chat is sent after counted commits keeps its own
    // waits.
    let deadline = (!task.chats.is_empty()).then(|| tokio::time::Instant::now() + options.timeout);
    let Some(session) = wait_for_session(
        &options.home,
        &registration,
        &state,
        &cwd,
        options.session,
        options.poll,
        deadline,
    )
    .await?
    else {
        progress.timed_out = true;
        save_progress(&progress_path, &progress)?;
        println!(
            "timeout: no session appeared within {} seconds",
            options.timeout.as_secs()
        );
        return Ok(());
    };
    if !task.chats.is_empty() {
        return chat(
            &options,
            &state,
            &task,
            &session,
            &progress_path,
            progress,
            deadline.expect("chat tasks have a deadline"),
        )
        .await;
    }
    if task.steers.is_empty() {
        println!("task {} defines no steers", task.id);
        return Ok(());
    }
    let repo = state.session_checkout(options.session)?.ok_or_else(|| {
        format!(
            "task {} sends steers after commits, so its session must run in a repository",
            task.id
        )
    })?;
    commit_steers(&options, &task, &session, &repo, &progress_path, progress).await
}

/// The `after_commits` schedule: one line per counted commit.
async fn commit_steers(
    options: &SteerOptions,
    task: &Task,
    session: &str,
    repo: &StateRepo,
    progress_path: &Path,
    mut progress: Progress,
) -> Result<()> {
    for (index, steer) in task.steers.iter().enumerate() {
        if progress.sent.contains(&index) {
            continue;
        }
        let count = wait_for_commits(repo, steer.after_commits, options.poll).await?;
        println!(
            "steer {}: {} ritual: commits ({}, wanted {})",
            index + 1,
            count,
            task.id,
            steer.after_commits
        );
        let registration = crate::server::registration(&options.home)?;
        let (status, body) = crate::server::steer(&registration, session, &steer.text).await?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "the eval server rejected steer {} for {session}: HTTP {status} {body}",
                index + 1
            ));
        }
        progress.sent.push(index);
        save_progress(progress_path, &progress)?;
        println!("steer {} sent", index + 1);
    }
    println!("all {} steers sent", task.steers.len());
    Ok(())
}

/// The `[[chat]]` schedule: each line waits for its moment and for the session
/// to be idle, then the next one waits for the turn it started to end.
async fn chat(
    options: &SteerOptions,
    state: &State,
    task: &Task,
    session: &str,
    progress_path: &Path,
    mut progress: Progress,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let events_path = options.home.session_events(session);
    let texts = task
        .chats
        .iter()
        .map(|entry| crate::task::render(&entry.text, state))
        .collect::<Result<Vec<_>>>()?;
    // A report wake is due a moment after a child finishes; stay idle a little
    // longer before deciding the session has handled every report.
    let grace = options
        .poll
        .saturating_mul(3)
        .max(Duration::from_millis(150));
    let mut idle_since = None;
    println!("task {} has {} chat lines", task.id, task.chats.len());
    loop {
        let view = View::read(&events_path)?;
        if let Some(view) = &view {
            let mut wrote = match_turns(&mut progress, view, &texts);
            if view.closed {
                save_progress(progress_path, &progress)?;
                println!("the session closed; stopping the chat schedule");
                return Ok(());
            }
            for index in 1..=task.chats.len() {
                if progress
                    .chats
                    .iter()
                    .any(|record| record.index == index && record.is_resolved())
                {
                    continue;
                }
                if progress
                    .chats
                    .iter()
                    .any(|record| record.index < index && !record.is_resolved())
                {
                    break;
                }
                let entry = &task.chats[index - 1];
                match ready(entry.after, index, view, &progress.chats) {
                    Ready::Wait => break,
                    Ready::Skip(reason) => {
                        println!("chat {index} not sent: {reason}");
                        progress.chats.push(ChatRecord {
                            index,
                            sent_at: None,
                            turn_id: None,
                            skipped: Some(reason.to_string()),
                        });
                        wrote = true;
                        continue;
                    }
                    Ready::Send => {
                        if !view.idle() {
                            break;
                        }
                        let registration = crate::server::registration(&options.home)?;
                        let (status, body) =
                            crate::server::chat(&registration, session, &texts[index - 1]).await?;
                        if !(200..300).contains(&status) {
                            return Err(format!(
                                "the eval server rejected chat {index} for {session}: HTTP {status} {body}"
                            ));
                        }
                        let turn_id = body
                            .get("turnId")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                        println!(
                            "chat {index} sent{}",
                            turn_id
                                .as_ref()
                                .map(|id| format!(" (turn {id})"))
                                .unwrap_or_default()
                        );
                        progress.chats.push(ChatRecord {
                            index,
                            sent_at: Some(crate::now_ms()),
                            turn_id,
                            skipped: None,
                        });
                        wrote = true;
                        break;
                    }
                }
            }
            if wrote {
                save_progress(progress_path, &progress)?;
            }
            let all_resolved = progress.chats.len() == task.chats.len()
                && progress.chats.iter().all(ChatRecord::is_resolved);
            if all_resolved && view.idle() && view.all_children_done() {
                let now = tokio::time::Instant::now();
                let since = *idle_since.get_or_insert(now);
                if now.duration_since(since) >= grace {
                    println!("every chat line is resolved and the session is idle");
                    return Ok(());
                }
            } else {
                idle_since = None;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            progress.timed_out = true;
            save_progress(progress_path, &progress)?;
            println!(
                "timeout: the chat schedule gave up after {} seconds",
                options.timeout.as_secs()
            );
            return Ok(());
        }
        tokio::time::sleep(options.poll).await;
    }
}

/// Fill in the turn each already-sent chat line started, when its reply did not
/// name it and the session's events do.
fn match_turns(progress: &mut Progress, view: &View, texts: &[String]) -> bool {
    let taken: Vec<String> = progress
        .chats
        .iter()
        .filter_map(|record| record.turn_id.clone())
        .collect();
    let mut changed = false;
    for record in progress.chats.iter_mut() {
        if record.sent_at.is_none() || record.turn_id.is_some() {
            continue;
        }
        let Some(text) = texts.get(record.index.saturating_sub(1)) else {
            continue;
        };
        if let Some(turn) = view.turn_after(text, &taken) {
            record.turn_id = Some(turn.id.clone());
            changed = true;
        }
    }
    changed
}

enum Ready {
    Wait,
    Skip(&'static str),
    Send,
}

fn ready(after: ChatAfter, index: usize, view: &View, chats: &[ChatRecord]) -> Ready {
    match after {
        ChatAfter::Turn => {
            let previous = chats
                .iter()
                .filter(|record| record.index < index && record.sent_at.is_some())
                .max_by_key(|record| record.index);
            let done = match previous {
                None => view.turns.first(),
                Some(record) => record.turn_id.as_deref().and_then(|id| view.turn(id)),
            };
            match done {
                Some(turn) if turn.ended_at.is_some() => Ready::Send,
                _ => Ready::Wait,
            }
        }
        ChatAfter::Report => {
            if !view.has_children() {
                let opening_ended = view
                    .turns
                    .first()
                    .is_some_and(|turn| turn.ended_at.is_some());
                if opening_ended {
                    Ready::Skip("没有子 agent 的报告")
                } else {
                    Ready::Wait
                }
            } else if view.all_children_done() {
                // The children may have finished during an earlier turn: the
                // report can have been handled without a wake turn of its own.
                Ready::Skip("所有子 agent 都回来了")
            } else if view.report_woke {
                Ready::Send
            } else {
                Ready::Wait
            }
        }
    }
}

fn read_progress(path: &Path) -> Result<Progress> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Progress::default()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn save_progress(path: &Path, progress: &Progress) -> Result<()> {
    crate::paths::write_private(
        path,
        serde_json::to_vec_pretty(progress)
            .map_err(|error| format!("cannot encode steer progress: {error}"))?
            .as_slice(),
    )
}

/// What the session's events say about the chat schedule.
struct View {
    turns: Vec<SeenTurn>,
    closed: bool,
    /// A turn a sub-agent report woke.
    report_woke: bool,
    /// Every sub-agent started from this session, by id: whether it is done.
    children: BTreeMap<String, bool>,
}

struct SeenTurn {
    id: String,
    ended_at: Option<i64>,
    input: String,
}

impl View {
    fn read(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        Ok(Some(Self::from_events(&session_data::parse_events(&text))))
    }

    fn from_events(events: &[Event]) -> Self {
        let mut view = Self {
            turns: Vec::new(),
            closed: false,
            report_woke: false,
            children: BTreeMap::new(),
        };
        for event in events {
            match &event.payload {
                EventPayload::TurnStarted { turn, .. } => {
                    let input = turn
                        .input
                        .iter()
                        .filter_map(|part| match part {
                            canter_protocol::UserInput::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if turn.client_name == "background" && input.is_empty() {
                        view.report_woke = true;
                    }
                    view.turns.push(SeenTurn {
                        id: turn.id.clone(),
                        ended_at: None,
                        input,
                    });
                }
                EventPayload::TurnCompleted { turn } => {
                    if let Some(row) = view.turns.iter_mut().find(|row| row.id == turn.id) {
                        row.ended_at = Some(event.timestamp);
                    }
                }
                EventPayload::SessionStatus { status, .. } => {
                    if *status == SessionStatus::Closed {
                        view.closed = true;
                    }
                }
                EventPayload::AgentUpdated { agent } => {
                    view.agent(&agent.id, Some(agent.status));
                }
                EventPayload::ItemCompleted { item, .. } => match item {
                    Item::CollabAgentToolCall { agents, .. } => {
                        for agent in agents {
                            view.agent(&agent.id, agent.status);
                        }
                    }
                    Item::BackgroundEvent {
                        agent: Some(agent), ..
                    } => view.agent(&agent.id, agent.status),
                    _ => {}
                },
                _ => {}
            }
        }
        view
    }

    fn agent(&mut self, id: &str, status: Option<AgentStatus>) {
        // A failed spawn returns a placeholder with no id: it started nothing.
        if id.is_empty() {
            return;
        }
        let done = status.is_some_and(is_done);
        self.children
            .entry(id.to_string())
            .and_modify(|entry| *entry |= done)
            .or_insert(done);
    }

    fn turn(&self, id: &str) -> Option<&SeenTurn> {
        self.turns.iter().find(|turn| turn.id == id)
    }

    fn turn_after(&self, text: &str, taken: &[String]) -> Option<&SeenTurn> {
        self.turns
            .iter()
            .find(|turn| turn.input.trim() == text.trim() && !taken.iter().any(|id| id == &turn.id))
    }

    /// No turn is running.
    fn idle(&self) -> bool {
        self.turns.iter().all(|turn| turn.ended_at.is_some())
    }

    fn has_children(&self) -> bool {
        !self.children.is_empty()
    }

    fn all_children_done(&self) -> bool {
        self.children.values().all(|done| *done)
    }
}

fn is_done(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Completed
            | AgentStatus::Failed
            | AgentStatus::Closed
            | AgentStatus::Interrupted
    )
}

async fn wait_for_commits(repo: &StateRepo, wanted: u32, poll: Duration) -> Result<u32> {
    let mut last = None;
    loop {
        let count = ritual_commits(repo)?;
        if count >= wanted {
            return Ok(count);
        }
        if last != Some(count) {
            println!("waiting for ritual: commits: {count}/{wanted}");
            last = Some(count);
        }
        tokio::time::sleep(poll).await;
    }
}

/// Commits after the base commit whose subject starts with `ritual: `.
pub fn ritual_commits(repo: &StateRepo) -> Result<u32> {
    let output = crate::prepare::git(
        Path::new(&repo.worktree),
        &[
            "log",
            "--reverse",
            "--format=%s",
            &format!("{}..HEAD", repo.base_commit),
        ],
    )?;
    let count = output
        .lines()
        .filter(|line| line.starts_with("ritual: "))
        .count();
    u32::try_from(count).map_err(|_| "too many commits".into())
}

/// The session id from this session's `session.json`, or else the new session
/// the eval server lists in the session's directory, saved for `finish` and
/// `collect`.
async fn wait_for_session(
    home: &crate::paths::Home,
    registration: &crate::server::Registration,
    state: &State,
    cwd: &Path,
    index: usize,
    poll: Duration,
    deadline: Option<tokio::time::Instant>,
) -> Result<Option<String>> {
    let path = crate::paths::session_id_file(home, state, index);
    loop {
        if let Some(session) = crate::paths::read_session_id(&path)? {
            return Ok(Some(session));
        }
        let exclude = home.recorded_session_ids(&state.round, &state.run)?;
        if let Some(session) =
            crate::server::find_session(registration, cwd, &exclude, &path).await?
        {
            let body = serde_json::json!({ "sessionId": session });
            crate::paths::write_private(&path, body.to_string().as_bytes())?;
            println!("session {session}");
            return Ok(Some(session));
        }
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Ok(None);
        }
        println!("waiting for a session in {}", cwd.display());
        tokio::time::sleep(poll).await;
    }
}
