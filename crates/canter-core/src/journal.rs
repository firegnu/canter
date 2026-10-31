use crate::{CoreError, context::HistoryEntry};
use canter_protocol::*;
use canter_store::{SessionWriter, Store};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, watch};

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) struct Journal {
    pub compactions: Vec<crate::telemetry::Compaction>,
    approval_times: std::collections::HashMap<String, (String, std::time::Instant)>,
    pub telemetry: Option<crate::telemetry::Pending>,
    writer: Arc<Mutex<Option<SessionWriter>>>,
    pub snapshot: SessionSnapshot,
    pub store: Arc<Store>,
    pub events: Option<broadcast::Sender<Event>>,
    pub committed: watch::Sender<u64>,
    pub failed: bool,
}
impl Journal {
    pub fn new(store: Arc<Store>, writer: SessionWriter, telemetry: bool) -> Self {
        let snapshot = writer.snapshot().clone();
        let committed = watch::channel(snapshot.last_event_seq).0;
        Self {
            compactions: Vec::new(),
            approval_times: Default::default(),
            telemetry: telemetry.then(Default::default),
            writer: Arc::new(Mutex::new(Some(writer))),
            snapshot,
            store,
            events: Some(broadcast::channel(256).0),
            committed,
            failed: false,
        }
    }
    pub async fn append(
        &mut self,
        mut payloads: Vec<EventPayload>,
    ) -> Result<Vec<Event>, CoreError> {
        if self.failed {
            return Err(CoreError::internal());
        }
        if let Some(pending) = &self.telemetry {
            let mut approvals = Vec::new();
            for payload in &payloads {
                match payload {
                    EventPayload::RequestOpened { request }
                        if matches!(
                            request.kind,
                            RequestKind::Approval(_)
                                | RequestKind::SandboxApproval(_)
                                | RequestKind::PlanApproval(_)
                        ) =>
                    {
                        self.approval_times.insert(
                            request.id.clone(),
                            (request.turn_id.clone(), std::time::Instant::now()),
                        );
                    }
                    EventPayload::RequestResolved {
                        request_id,
                        resolution,
                        ..
                    } => {
                        if let Some((turn_id, start)) = self.approval_times.remove(request_id) {
                            let outcome = match resolution {
                                ApprovalResolution::Answered {
                                    decision:
                                        ApprovalDecision::Accept
                                        | ApprovalDecision::AcceptForSession
                                        | ApprovalDecision::AcceptWithExecpolicyAmendment,
                                    ..
                                }
                                | ApprovalResolution::Plan {
                                    answer:
                                        PlanAnswer {
                                            decision:
                                                PlanDecision::Implement | PlanDecision::ClearContext,
                                            ..
                                        },
                                } => "approved",
                                ApprovalResolution::Answered {
                                    decision:
                                        ApprovalDecision::Decline
                                        | ApprovalDecision::DeclineWithFeedback,
                                    ..
                                }
                                | ApprovalResolution::Plan {
                                    answer:
                                        PlanAnswer {
                                            decision: PlanDecision::KeepPlanning,
                                            ..
                                        },
                                } => "declined",
                                _ => "cancelled",
                            };
                            approvals.push(EventPayload::TelemetryApproval {
                                turn_id,
                                request_id: request_id.clone(),
                                duration_ms: crate::telemetry::millis(start),
                                outcome: outcome.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            payloads.extend(approvals);
            payloads.extend(std::mem::take(
                &mut *pending.lock().expect("telemetry queue"),
            ));
        }
        let compactions = std::mem::take(&mut self.compactions);
        if payloads.is_empty() && compactions.is_empty() {
            return Ok(Vec::new());
        }
        let writer = self.writer.clone();
        let result = tokio::task::spawn_blocking(move || {
            payloads.extend(
                compactions
                    .into_iter()
                    .map(crate::telemetry::Compaction::event),
            );
            let mut guard = writer.lock().map_err(|_| CoreError::internal())?;
            let writer = guard.as_mut().ok_or_else(CoreError::internal)?;
            writer
                .append_batch(payloads.into_iter().map(|payload| (payload, now())))
                .map_err(CoreError::from)
        })
        .await
        .unwrap_or_else(|_| Err(CoreError::internal()));
        match result {
            Ok(events) => {
                for event in &events {
                    apply_event_mut(&mut self.snapshot, event)
                        .map_err(|_| CoreError::internal())?;
                }
                self.committed.send_replace(self.snapshot.last_event_seq);
                if let Some(sender) = &self.events {
                    for event in &events {
                        let _ = sender.send(event.clone());
                    }
                }
                Ok(events)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
    pub async fn record(&mut self, entry: HistoryEntry) -> Result<(), CoreError> {
        if self.failed {
            return Err(CoreError::internal());
        }
        let writer = self.writer.clone();
        let result = tokio::task::spawn_blocking(move || {
            writer
                .lock()
                .map_err(|_| CoreError::internal())?
                .as_mut()
                .ok_or_else(CoreError::internal)?
                .append_model_history(&entry)
                .map_err(CoreError::from)
        })
        .await
        .unwrap_or_else(|_| Err(CoreError::internal()));
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub async fn release(&mut self) -> Result<(), CoreError> {
        self.events.take();
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            writer.lock().map_err(|_| CoreError::internal())?.take();
            Ok(())
        })
        .await
        .map_err(|_| CoreError::internal())?
    }
}

/// Durable catch-up followed by live events. A slow reader catches up from disk.
pub struct Subscription {
    pub(crate) store: Arc<Store>,
    pub(crate) id: String,
    pub(crate) receiver: broadcast::Receiver<Event>,
    pub(crate) committed: watch::Receiver<u64>,
    pub(crate) pending: VecDeque<Event>,
    pub(crate) cursor: u64,
    pub(crate) ended: bool,
}
impl Subscription {
    pub(crate) async fn catch_up(&mut self) -> Result<(), CoreError> {
        let store = self.store.clone();
        let id = self.id.clone();
        let cursor = self.cursor;
        let through = *self.committed.borrow();
        self.pending = tokio::task::spawn_blocking(move || store.events_after(&id, cursor))
            .await
            .map_err(|_| CoreError::internal())??
            .into_iter()
            .take_while(|event| event.seq <= through)
            .collect();
        Ok(())
    }
    pub async fn recv(&mut self) -> Result<Option<Event>, CoreError> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let event = if let Some(event) = self.pending.pop_front() {
                event
            } else {
                match self.receiver.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        self.catch_up().await?;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.catch_up().await?;
                        if self.pending.is_empty() {
                            self.ended = true;
                            return Ok(None);
                        }
                        continue;
                    }
                }
            };
            if event.seq <= self.cursor {
                continue;
            }
            if event.seq != self.cursor + 1 {
                self.catch_up().await?;
                continue;
            }
            self.cursor = event.seq;
            return Ok(Some(event));
        }
    }
}
