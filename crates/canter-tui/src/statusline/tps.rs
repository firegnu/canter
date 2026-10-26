//! TPS for the current model request, or the last completed response.

use canter_protocol::{
    Event, EventPayload, Item, ItemDelta, SessionSnapshot, SessionStatus, TurnStatus,
};

#[derive(Default)]
pub(super) struct Tps {
    current: Option<Output>,
    last: Option<f64>,
    /// Snapshots have text but no first-delta time. Wait for a request boundary
    /// instead of dividing the whole request's tokens by only its observed tail.
    incomplete: bool,
}

struct Output {
    first: i64,
    bytes: u64,
    end: Option<i64>,
    tokens: Option<u64>,
}

impl Output {
    fn rate(&self, now: i64) -> Option<f64> {
        let elapsed = self.end.unwrap_or(now).saturating_sub(self.first);
        // Same UTF-8 byte heuristic as canter-core's context::estimate.
        let tokens = self.tokens.unwrap_or_else(|| self.bytes.div_ceil(3));
        (elapsed > 0).then(|| tokens as f64 * 1000.0 / elapsed as f64)
    }
}

impl Tps {
    pub fn new(snapshot: &SessionSnapshot) -> Self {
        Self {
            incomplete: snapshot.turns.last().is_some_and(|turn| {
                turn.status == TurnStatus::InProgress
                    && matches!(
                        turn.items.last(),
                        Some(Item::AgentMessage { .. } | Item::Reasoning { .. })
                    )
            }),
            ..Self::default()
        }
    }

    pub fn rate(&self, now: i64) -> Option<f64> {
        self.current
            .as_ref()
            .map_or(self.last, |output| output.rate(now))
    }

    pub fn event(&mut self, event: &Event) {
        if self.incomplete {
            let boundary = match &event.payload {
                EventPayload::TurnStarted { .. }
                | EventPayload::TurnCompleted { .. }
                | EventPayload::SessionUsage { .. } => true,
                EventPayload::ItemStarted { item, .. }
                | EventPayload::ItemCompleted { item, .. } => {
                    !matches!(item, Item::AgentMessage { .. } | Item::Reasoning { .. })
                }
                EventPayload::SessionStatus { status, .. } => {
                    !matches!(status, SessionStatus::Working | SessionStatus::Starting)
                }
                _ => false,
            };
            self.incomplete = !boundary;
            return;
        }
        let at = event.timestamp;
        match &event.payload {
            EventPayload::ItemDelta { delta, .. } => {
                let text = match delta {
                    ItemDelta::AgentMessage { delta }
                    | ItemDelta::ReasoningContent { delta, .. }
                    | ItemDelta::ReasoningSummary { delta, .. } => delta,
                    _ => return,
                };
                if text.is_empty() {
                    return;
                }
                if self
                    .current
                    .as_ref()
                    .is_some_and(|output| output.end.is_some())
                {
                    self.current = None;
                }
                let output = self.current.get_or_insert(Output {
                    first: at,
                    bytes: 0,
                    end: None,
                    tokens: None,
                });
                output.bytes = output.bytes.saturating_add(text.len() as u64);
            }
            EventPayload::ItemCompleted {
                item: Item::AgentMessage { .. } | Item::Reasoning { .. },
                ..
            }
            | EventPayload::TurnCompleted { .. } => self.finish(at),
            EventPayload::SessionStatus { status, .. }
                if !matches!(status, SessionStatus::Working | SessionStatus::Starting) =>
            {
                self.finish(at)
            }
            EventPayload::SessionUsage { token_usage } => {
                if let Some(output) = &mut self.current {
                    // outputTokens already includes reasoning; do not add it twice.
                    output.tokens = Some(token_usage.last.output_tokens);
                    output.end = Some(at);
                    self.last = output.rate(at);
                }
            }
            EventPayload::ItemStarted { item, .. } => {
                if !matches!(item, Item::AgentMessage { .. } | Item::Reasoning { .. }) {
                    self.finish(at);
                    self.current = None;
                }
            }
            EventPayload::TurnStarted { .. } => self.current = None,
            _ => {}
        }
    }

    fn finish(&mut self, at: i64) {
        if let Some(output) = &mut self.current {
            output.end.get_or_insert(at);
            self.last = output.rate(at);
        }
    }
}
