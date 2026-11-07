//! TPS for the current model request, or the last completed response.

use std::cell::Cell;

use canter_protocol::{
    Event, EventPayload, Item, ItemDelta, SessionSnapshot, SessionStatus, TurnStatus,
};

/// How long a rate shown stays before a new one shows (P2-245), so the
/// number does not change all the time.
const HOLD_MS: i64 = 1000;

#[derive(Default)]
pub(crate) struct Tps {
    current: Option<Output>,
    last: Option<f64>,
    /// Snapshots have text but no first-delta time. Wait for a request boundary
    /// instead of dividing the whole request's tokens by only its observed tail.
    incomplete: bool,
    /// The rate last given out, and when.
    shown: Cell<Option<(i64, f64)>>,
}

struct Output {
    first: i64,
    /// When the latest text came.
    latest: i64,
    bytes: u64,
    end: Option<i64>,
    tokens: Option<u64>,
}

impl Output {
    /// While the model writes, over the time from its first text to its
    /// latest, so the rate holds while it waits (P2-245); once the request
    /// ended, to its end.
    fn rate(&self) -> Option<f64> {
        let elapsed = self.end.unwrap_or(self.latest).saturating_sub(self.first);
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

    /// The current request's rate, or the last one's until it has one; the
    /// same for [`HOLD_MS`] after it changes.
    pub fn rate(&self, now: i64) -> Option<f64> {
        let rate = self.current.as_ref().and_then(Output::rate).or(self.last);
        match (self.shown.get(), rate) {
            (Some((at, shown)), Some(_)) if now.saturating_sub(at) < HOLD_MS => Some(shown),
            _ => {
                self.shown.set(rate.map(|rate| (now, rate)));
                rate
            }
        }
    }

    pub fn event(&mut self, event: &Event) {
        if let EventPayload::SessionUsage { token_usage } = &event.payload
            && token_usage.records_no_request()
        {
            return;
        }
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
                    latest: at,
                    bytes: 0,
                    end: None,
                    tokens: None,
                });
                output.bytes = output.bytes.saturating_add(text.len() as u64);
                output.latest = at;
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
                    self.last = output.rate();
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
            self.last = output.rate();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn event(tps: &mut Tps, at: i64, payload: Value) {
        let mut value = json!({"seq": 1, "sessionId": "s1", "timestamp": at});
        value
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        tps.event(&serde_json::from_value(value).unwrap());
    }

    /// Text of `bytes` bytes from the model at `at`.
    fn text(tps: &mut Tps, at: i64, bytes: usize) {
        event(
            tps,
            at,
            json!({"type":"item.delta","turnId":"t1","itemId":"m1",
                "delta":{"type":"agentMessage","delta":"a".repeat(bytes)}}),
        );
    }

    #[test]
    fn while_the_model_waits_the_speed_stays() {
        let mut tps = Tps::default();
        text(&mut tps, 1000, 30);
        text(&mut tps, 2000, 30);
        // 20 tokens from the first text to the last, one second.
        assert_eq!(tps.rate(2000), Some(20.0));
        // Nothing comes for five seconds: the time to now does not count.
        assert_eq!(tps.rate(7000), Some(20.0));
        text(&mut tps, 7000, 30);
        assert_eq!(tps.rate(8000), Some(5.0));
    }

    #[test]
    fn the_speed_shown_changes_at_most_once_a_second() {
        let mut tps = Tps::default();
        text(&mut tps, 1000, 30);
        text(&mut tps, 2000, 30);
        assert_eq!(tps.rate(2000), Some(20.0));
        // 40 tokens over a second and a half: not shown yet.
        text(&mut tps, 2500, 60);
        assert_eq!(tps.rate(2500), Some(20.0), "held");
        assert_eq!(tps.rate(2999), Some(20.0), "held");
        // 50 tokens over two seconds, a second after the last change.
        text(&mut tps, 3000, 30);
        assert_eq!(tps.rate(3000), Some(25.0));
        // 100 tokens over two and a half seconds.
        text(&mut tps, 3500, 150);
        assert_eq!(tps.rate(3500), Some(25.0), "held from 3000");
        assert_eq!(tps.rate(3999), Some(25.0), "held from 3000");
        assert_eq!(tps.rate(4000), Some(40.0));
    }
}
