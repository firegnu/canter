//! Which queued messages this terminal sent with Tab to steer the turn in
//! progress (P2-97). The server lists them with the queued ones until the
//! model reads them; its reply to the input (`turnId` set) and the
//! `input.queued` event share one `eventSeq` and can come in either order.

use canter_protocol::{Event, EventPayload};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct Steering {
    /// `input.queued` seq → input id, until the reply for it is in.
    queued: HashMap<u64, String>,
    /// Replies accepting a steer whose `input.queued` is not in yet.
    accepted: HashSet<u64>,
    /// Steers not yet in the turn.
    ids: HashSet<String>,
}

impl Steering {
    /// Keeps the steers still waiting, for the same session shown again.
    pub fn carried(&self) -> Self {
        Self {
            ids: self.ids.clone(),
            ..Self::default()
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// The reply to a steer: `accepted` when it names the turn it goes into.
    pub fn reply(&mut self, seq: u64, accepted: bool) {
        match self.queued.remove(&seq) {
            Some(id) if accepted => {
                self.ids.insert(id);
            }
            Some(_) => {}
            None if accepted => {
                self.accepted.insert(seq);
            }
            None => {}
        }
    }

    pub fn event(&mut self, event: &Event) {
        match &event.payload {
            EventPayload::InputQueued { input } => {
                if self.accepted.remove(&event.seq) {
                    self.ids.insert(input.id.clone());
                } else {
                    self.queued.insert(event.seq, input.id.clone());
                }
            }
            EventPayload::InputRemoved { input_ids, .. } => {
                for id in input_ids {
                    self.forget(id);
                }
            }
            EventPayload::TurnStarted {
                input_id: Some(id), ..
            } => self.forget(id),
            _ => {}
        }
        // Events come in order: a seq passed by is not an `input.queued`,
        // e.g. a steer sent just as the turn ended started a new one.
        self.accepted.retain(|seq| *seq > event.seq);
    }

    fn forget(&mut self, id: &str) {
        self.ids.remove(id);
        self.queued.retain(|_, queued| queued != id);
    }
}
