//! Event-stream lifetimes, independent of one-shot session requests.
use crate::{Session, hooks, session::Shared};
use canter_protocol::SessionStatus;
use std::sync::Arc;
use tokio::time::Duration;

const LEAVE_GRACE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(crate) struct Clients {
    count: usize,
    generation: u64,
}

impl Clients {
    pub(crate) fn is_attached(&self) -> bool {
        self.count != 0
    }
}

/// Keep this guard for the lifetime of an external client's event stream.
pub struct ClientConnection(Session);

impl Session {
    pub async fn attach_client(&self) -> Option<ClientConnection> {
        let mut state = self.shared.state.lock().await;
        if state.closing || state.journal.snapshot.status == SessionStatus::Closed {
            return None;
        }
        let mut clients = self.shared.clients.lock().unwrap();
        clients.count += 1;
        clients.generation += 1;
        if state.hook_session_ended {
            state.hook_session_ended = false;
            hooks::notify(
                &self.shared,
                &state,
                hooks::Event::SessionStart,
                serde_json::json!({"source":"resume"}),
                self.shared.lifetime.child_token(),
            );
        }
        Some(ClientConnection(self.clone()))
    }
}

impl Drop for ClientConnection {
    fn drop(&mut self) {
        let shared = &self.0.shared;
        let generation = {
            let mut clients = shared.clients.lock().unwrap();
            clients.count -= 1;
            if clients.count != 0 {
                return;
            }
            clients.generation += 1;
            clients.generation
        };
        let lifetime = shared.lifetime.clone();
        let shared = Arc::downgrade(shared);
        tokio::spawn(async move {
            tokio::select! {
                _ = lifetime.cancelled() => return,
                _ = tokio::time::sleep(LEAVE_GRACE) => {},
            }
            loop {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                if finish_if_stopped(&shared, generation).await {
                    return;
                }
                // Only poll while an absent client's session is still working;
                // goal/wake scheduling also changes state without journal events.
                tokio::select! {
                    _ = lifetime.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        });
    }
}

// This only ends the client's hook lifecycle, without closing the session.
// Waiting for user input counts as stopped here; close_for_trash is stricter.
async fn finish_if_stopped(shared: &Shared, generation: u64) -> bool {
    let mut state = shared.state.lock().await;
    let clients = shared.clients.lock().unwrap();
    if clients.count != 0
        || clients.generation != generation
        || state.closing
        || state.hook_session_ended
        || state.journal.failed
        || state.journal.snapshot.status == SessionStatus::Closed
    {
        return true;
    }
    if state.btw.is_some()
        || (state.journal.snapshot.status != SessionStatus::WaitingApproval
            && (state.active.is_some()
                || !state.queue.is_empty()
                || state.goal_deadline.is_some()
                || (!state.automatic_turns_paused
                    && state
                        .background_events
                        .has_pending_wake(shared.next_options().auto_wake_limit))))
    {
        return false;
    }
    state.hook_session_ended = true;
    hooks::notify(
        shared,
        &state,
        hooks::Event::SessionEnd,
        serde_json::json!({"reason":"client_exit"}),
        shared.lifetime.child_token(),
    );
    true
}
