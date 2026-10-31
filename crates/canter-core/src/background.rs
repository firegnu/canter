use crate::{
    CoreError, Session,
    context::HistoryEntry,
    session::{Shared, State, owned},
};
use canter_model::Message;
use canter_protocol::*;
use std::sync::{Arc, Weak};
use tokio::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct PendingEvents {
    pub all: std::collections::BTreeMap<String, Item>,
    cards: Vec<Item>,
    deadline: Option<Instant>,
    wake_pending: bool,
    pub(crate) changed: Arc<tokio::sync::Notify>,
    output_due: Option<Instant>,
    // Match count and wake eligibility; Esc suppresses the observed batch,
    // while later matching lines can make the command eligible again.
    output_wakes: std::collections::BTreeMap<u64, (usize, bool)>,
    count: u32,
    reported: bool,
    pub draining: bool,
}

impl PendingEvents {
    pub fn user_input(&mut self) {
        self.count = 0;
        self.reported = false;
        self.interrupt();
    }
    pub fn interrupt(&mut self) {
        self.deadline = None;
        self.wake_pending = false;
        self.output_due = None;
        for (_, wake) in self.output_wakes.values_mut() {
            *wake = false;
        }
    }
    pub fn turn_finished(&mut self) {
        if self.draining {
            return;
        }
        if self.wake_pending {
            self.deadline = Some(Instant::now() + Duration::from_secs(2));
        }
        self.changed.notify_one();
    }
}

const EVENT_MARKER: &str = "\n[Background event: ";

pub(crate) fn notice_text(text: &str) -> &str {
    text.rsplit_once(EVENT_MARKER)
        .map_or(text, |(text, _)| text)
}

fn event_id(message: &Message) -> Option<&str> {
    let Message::User { text } = message else {
        return None;
    };
    text.rsplit_once(EVENT_MARKER)?.1.strip_suffix(']')
}

fn card_message(item: &Item) -> Message {
    let Item::BackgroundEvent { id, text, .. } = item else {
        unreachable!()
    };
    Message::User {
        text: format!("{text}{EVENT_MARKER}{id}]"),
    }
}

pub(crate) fn notice_id(message: &Message) -> Option<u64> {
    let Message::User { text } = message else {
        return None;
    };
    text.strip_prefix("[Background command ")?
        .split_once(' ')?
        .0
        .parse()
        .ok()
}

impl State {
    async fn new_background_notice(
        &mut self,
        command_id: u64,
        text: String,
        visible: bool,
        wake_at: Option<Instant>,
    ) -> Result<(), CoreError> {
        let item = Item::BackgroundEvent {
            id: crate::session::id("background-event"),
            command_id: Some(command_id),
            text,
        };
        let message = card_message(&item);
        if visible {
            self.background_events
                .all
                .insert(item.id().unwrap().into(), item.clone());
            self.background_events.cards.push(item);
        }
        if let Some(deadline) = wake_at {
            self.background_events.wake_pending = true;
            if self.active.is_none()
                && self.queue.is_empty()
                && self.requests.is_empty()
                && !self.background_events.draining
            {
                let previous = self.background_events.deadline.get_or_insert(deadline);
                *previous = (*previous).min(deadline);
            }
        }
        self.background_notice(message).await
    }
    fn has_background_card(&self, item: &Item) -> bool {
        self.journal
            .snapshot
            .turns
            .iter()
            .any(|turn| turn.items.iter().any(|old| old.id() == item.id()))
    }
    pub(crate) fn requeue_background_cards(&mut self) {
        self.background_events.cards.retain(|item| {
            self.history.contains(&card_message(item))
                || self.plan_mode_notices.contains(&card_message(item))
        });
        for item in self.background_events.all.values() {
            if self.history.contains(&card_message(item))
                && !self.has_background_card(item)
                && !self.background_events.cards.contains(item)
            {
                self.background_events.cards.push(item.clone());
            }
        }
    }
    pub(crate) fn pending_background_messages(&self) -> Vec<Message> {
        self.background_events
            .cards
            .iter()
            .map(card_message)
            .collect()
    }
    pub(crate) fn consume_background_notices(&mut self, messages: &[Message]) {
        self.background_events
            .cards
            .retain(|item| !messages.contains(&card_message(item)));
        if self.background_events.cards.is_empty() {
            self.background_events.wake_pending = false;
            self.background_events.deadline = None;
        }
    }
    pub(crate) async fn background_cards(&mut self, turn_id: &str) -> Result<(), CoreError> {
        let payloads = self
            .background_events
            .cards
            .iter()
            .filter(|item| !self.has_background_card(item))
            .flat_map(|item| {
                [
                    EventPayload::ItemStarted {
                        turn_id: turn_id.into(),
                        item: item.clone(),
                    },
                    EventPayload::ItemCompleted {
                        turn_id: turn_id.into(),
                        item: item.clone(),
                    },
                ]
            })
            .collect();
        self.journal.append(payloads).await?;
        Ok(())
    }
    async fn report_wake_limit(&mut self, limit: u32) -> Result<(), CoreError> {
        if limit == 0 || self.background_events.reported {
            return Ok(());
        }
        let Some(turn_id) = self
            .journal
            .snapshot
            .turns
            .last()
            .map(|turn| turn.id.clone())
        else {
            return Ok(());
        };
        let text = format!(
            "Auto wake paused: reached {limit} consecutive wake-ups without user input. Events are kept; send a message to resume."
        );
        let item = Item::BackgroundEvent {
            id: crate::session::id("wake-limit"),
            command_id: None,
            text,
        };
        self.journal
            .append(vec![
                EventPayload::ItemStarted {
                    turn_id: turn_id.clone(),
                    item: item.clone(),
                },
                EventPayload::ItemCompleted { turn_id, item },
            ])
            .await?;
        self.background_events.reported = true;
        Ok(())
    }
    pub(crate) async fn restate_background_notices(
        &mut self,
        removed: Vec<Message>,
    ) -> Result<(), CoreError> {
        for message in removed {
            let Some(id) = notice_id(&message) else {
                continue;
            };
            let retained = self
                .journal
                .snapshot
                .background_commands
                .iter()
                .find(|command| command.id == id)
                .is_some_and(|command| {
                    self.journal.snapshot.turns.iter().any(|turn| {
                        turn.items
                            .iter()
                            .any(|item| item.id() == Some(command.item_id.as_str()))
                    })
                });
            if retained
                && !self.history.contains(&message)
                && !self.plan_mode_notices.contains(&message)
            {
                self.background_notice(message).await?;
            }
        }
        self.requeue_background_cards();
        Ok(())
    }

    pub(crate) async fn background_notice(&mut self, message: Message) -> Result<(), CoreError> {
        if self.history.contains(&message) || self.plan_mode_notices.contains(&message) {
            return Ok(());
        }
        let event_id = event_id(&message).map(str::to_owned);
        let visible = event_id
            .as_ref()
            .is_some_and(|id| self.background_events.all.contains_key(id));
        self.journal
            .record(HistoryEntry::BackgroundNotice {
                command_id: notice_id(&message),
                event_id,
                visible,
                message: message.clone(),
            })
            .await?;
        self.plan_mode_notices.push(message);
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }
    async fn background_update(
        &mut self,
        command: BackgroundCommand,
        recovered: bool,
        wake: bool,
    ) -> Result<(), CoreError> {
        if !command.running {
            let text = format!(
                "[Background command {} finished: {}. Exit code: {}. {}]",
                command.id,
                command.command,
                command.exit_code.map_or_else(
                    || "unknown (signal or server shutdown)".into(),
                    |code| code.to_string()
                ),
                if recovered {
                    "Output unavailable after server restart.".into()
                } else {
                    format!(
                        "Use shell_output with id={} to read the remaining output.",
                        command.id
                    )
                },
            );
            self.new_background_notice(
                command.id,
                text,
                wake,
                wake.then(|| Instant::now() + Duration::from_secs(2)),
            )
            .await?;
        }
        self.journal
            .append(vec![EventPayload::BackgroundCommandUpdated { command }])
            .await?;
        Ok(())
    }
    pub(crate) async fn recover_background(&mut self) -> Result<(), CoreError> {
        for mut command in self.journal.snapshot.background_commands.clone() {
            if command.running {
                command.running = false;
                command.completed_at = Some(crate::journal::now());
                command.exit_code = None;
                self.background_update(command, true, false).await?;
            }
        }
        Ok(())
    }
}

impl Shared {
    pub(crate) async fn sync_background(&self) -> Result<(), CoreError> {
        self.sync_background_inner(true).await
    }
    async fn sync_background_inner(&self, collect_output: bool) -> Result<(), CoreError> {
        let mut state = self.state.lock().await;
        let can_wake = !state.journal.snapshot.settings.non_interactive
            && state.active.is_none()
            && state.queue.is_empty()
            && state.requests.is_empty()
            && !state.background_events.draining
            && state.available().is_ok()
            && state.background_events.count < self.options.auto_wake_limit
            && self.input_waiters.load(std::sync::atomic::Ordering::SeqCst) == 0;
        for (id, count, allowed) in self.tools.background().matching_commands() {
            let pending = state
                .background_events
                .output_wakes
                .entry(id)
                .or_insert((count, allowed));
            if pending.0 != count {
                *pending = (count, allowed);
            }
            pending.1 &= allowed;
        }
        if can_wake
            && state
                .background_events
                .output_wakes
                .values()
                .any(|(_, wake)| *wake)
        {
            state
                .background_events
                .output_due
                .get_or_insert(Instant::now() + Duration::from_secs(2));
        } else {
            state.background_events.output_due = None;
        }
        if collect_output
            || state
                .background_events
                .output_due
                .is_some_and(|due| due <= Instant::now())
        {
            for (id, text, allowed) in self.tools.background().take_matches() {
                let wake = allowed
                    && state
                        .background_events
                        .output_wakes
                        .remove(&id)
                        .is_some_and(|(_, wake)| wake);
                state
                    .new_background_notice(id, text, true, wake.then(Instant::now))
                    .await?;
            }
            state.background_events.output_due = None;
            state.background_events.output_wakes.clear();
        }
        for command in self.tools.background().list() {
            if !state
                .journal
                .snapshot
                .background_commands
                .iter()
                .any(|old| old == &command)
            {
                let wake = self.tools.background().wakes_on_exit(command.id);
                state.background_update(command, false, wake).await?;
            }
        }
        Ok(())
    }
}

pub(crate) fn watch(shared: &Arc<Shared>) {
    let mut changes = shared.tools.background().subscribe();
    let lifetime = shared.lifetime.clone();
    let shared: Weak<Shared> = Arc::downgrade(shared);
    tokio::spawn(async move {
        loop {
            let (deadline, ready) = {
                let Some(shared) = shared.upgrade() else {
                    break;
                };
                let state = shared.state.lock().await;
                let deadline = [
                    state.goal_deadline,
                    state.background_events.deadline,
                    state.background_events.output_due,
                ]
                .into_iter()
                .flatten()
                .min();
                (deadline, state.background_events.changed.clone())
            };
            tokio::select! {
                biased;
                _ = lifetime.cancelled() => break,
                _ = ready.notified() => {},
                _ = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {},
                changed = changes.changed() => if changed.is_err() { break; },
            }
            let Some(shared) = shared.upgrade() else {
                break;
            };
            if shared.state.lock().await.closing {
                break;
            }
            if shared.sync_background_inner(false).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
            let mut state = shared.state.lock().await;
            match shared.next_goal(&mut state).await {
                Ok(Some(done)) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        crate::runner::run(shared, done).await;
                    });
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if state
                .background_events
                .deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                if shared
                    .input_waiters
                    .load(std::sync::atomic::Ordering::SeqCst)
                    > 0
                {
                    state.background_events.deadline =
                        Some(Instant::now() + Duration::from_millis(10));
                    continue;
                }
                state.background_events.deadline = None;
                if state.active.is_none()
                    && !state.journal.snapshot.settings.non_interactive
                    && state.queue.is_empty()
                    && state.requests.is_empty()
                    && !state.background_events.draining
                    && state.journal.snapshot.status == SessionStatus::Idle
                    && !state.background_events.cards.is_empty()
                    && state.available().is_ok()
                {
                    if state.background_events.count >= shared.options.auto_wake_limit {
                        if state
                            .report_wake_limit(shared.options.auto_wake_limit)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    let params = SessionInputParams {
                        input: Vec::new(),
                        review: None,
                        mode: InputMode::Queue,
                        client_name: "background".into(),
                    };
                    match shared.start(&mut state, params, None, Vec::new()).await {
                        Ok((_, done)) => {
                            state.active.as_mut().unwrap().auto_wake = true;
                            state.background_events.count += 1;
                            let turn_id = state.active.as_ref().unwrap().turn.id.clone();
                            let setup = async {
                                state.background_cards(&turn_id).await?;
                                if state.background_events.count == shared.options.auto_wake_limit {
                                    state
                                        .report_wake_limit(shared.options.auto_wake_limit)
                                        .await?;
                                }
                                Ok::<_, CoreError>(())
                            }
                            .await;
                            if let Err(error) = setup {
                                state.active = None;
                                let _ = done.send(Some(Err(error)));
                                break;
                            }
                            let shared = shared.clone();
                            tokio::spawn(async move {
                                crate::runner::run(shared, done).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });
}

impl Session {
    /// Stop initiating turns during the server's drain grace period.
    pub async fn drain(&self) {
        let mut state = self.shared.state.lock().await;
        state.background_events.draining = true;
        state.background_events.interrupt();
    }
    pub async fn background_commands(&self) -> Result<BackgroundCommandListResponse, CoreError> {
        self.shared.sync_background_inner(false).await?;
        Ok(BackgroundCommandListResponse {
            data: self
                .shared
                .state
                .lock()
                .await
                .journal
                .snapshot
                .background_commands
                .clone(),
        })
    }
    pub async fn background_output(&self, id: u64) -> Result<BackgroundCommandOutput, CoreError> {
        match self.shared.tools.background().output(id, false) {
            Ok(output) => Ok(output),
            Err(_) => {
                let state = self.shared.state.lock().await;
                let command = state
                    .journal
                    .snapshot
                    .background_commands
                    .iter()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| {
                        CoreError::new(ErrorCode::InvalidRequest, "unknown background command")
                    })?
                    .clone();
                Ok(BackgroundCommandOutput {
                    command,
                    output: "Output unavailable after server restart.".into(),
                    skipped_bytes: 0,
                })
            }
        }
    }
    pub async fn stop_background(
        &self,
        id: Option<u64>,
    ) -> Result<BackgroundCommandListResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            if let Some(id) = id {
                session
                    .shared
                    .tools
                    .background()
                    .stop(id)
                    .await
                    .map_err(|message| CoreError::new(ErrorCode::InvalidRequest, &message))?;
            } else {
                session.shared.tools.background().stop_all().await;
            }
            session.background_commands().await
        })
        .await
    }
    pub async fn shell_background(
        &self,
        params: ShellBackgroundParams,
    ) -> Result<ShellBackgroundResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            let command_id = session
                .shared
                .tools
                .background()
                .promote(&params.item_id)
                .await
                .map_err(|message| CoreError::new(ErrorCode::Conflict, &message))?;
            session.shared.sync_background().await?;
            Ok(ShellBackgroundResponse {
                command_id,
                event_seq: session
                    .shared
                    .state
                    .lock()
                    .await
                    .journal
                    .snapshot
                    .last_event_seq,
            })
        })
        .await
    }
}
