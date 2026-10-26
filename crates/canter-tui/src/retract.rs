use canter_protocol::{
    Event, EventPayload, InputRemovalReason, SessionSnapshot, TurnStatus, UserInput,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone)]
struct Source {
    seq: u64,
    input: Vec<UserInput>,
}

struct Pending {
    turn: String,
    sources: Vec<Source>,
    removed: bool,
    accepted: bool,
}

/// Retain original local attachment paths until the server settles the input.
/// HTTP replies and SSE events can arrive in either order.
#[derive(Default)]
pub(crate) struct Drafts {
    sources: HashMap<String, Vec<Source>>,
    originals: BTreeMap<u64, Vec<UserInput>>,
    sending: usize,
    pending: Option<Pending>,
    last_seq: u64,
}

impl Drafts {
    pub fn new(snapshot: &SessionSnapshot) -> Self {
        let mut drafts = Self {
            last_seq: snapshot.last_event_seq,
            ..Self::default()
        };
        for turn in &snapshot.turns {
            if turn.status == TurnStatus::InProgress && turn.review.is_none() {
                drafts.sources.insert(
                    turn.id.clone(),
                    vec![Source {
                        seq: 0,
                        input: turn.input.clone(),
                    }],
                );
            }
        }
        for input in snapshot
            .queued_inputs
            .iter()
            .filter(|input| input.review.is_none())
        {
            drafts.sources.insert(
                input.id.clone(),
                vec![Source {
                    seq: 0,
                    input: input.input.clone(),
                }],
            );
        }
        drafts
    }

    pub fn sending(&mut self) {
        self.sending += 1;
    }

    pub fn sent(&mut self, seq: u64, input: Vec<UserInput>) {
        self.sending = self.sending.saturating_sub(1);
        self.originals.insert(seq, input);
    }

    pub fn send_failed(&mut self) {
        self.sending = self.sending.saturating_sub(1);
    }

    pub fn interrupt(&mut self, turn: &str) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.turn == turn)
        {
            return;
        }
        self.pending = Some(Pending {
            turn: turn.into(),
            sources: self.sources.get(turn).cloned().unwrap_or_default(),
            removed: false,
            accepted: false,
        });
    }

    pub fn response(&mut self, turn: &str, retracted: bool) {
        if let Some(pending) = &mut self.pending
            && pending.turn == turn
        {
            if retracted {
                pending.accepted = true;
            } else {
                self.pending = None;
            }
        }
    }

    pub fn event(&mut self, event: &Event) {
        if event.seq <= self.last_seq {
            return;
        }
        self.last_seq = event.seq;
        match &event.payload {
            EventPayload::TurnStarted { turn, input_id } if turn.review.is_none() => {
                let sources = input_id
                    .as_ref()
                    .and_then(|id| self.sources.remove(id))
                    .unwrap_or_else(|| {
                        vec![Source {
                            seq: event.seq,
                            input: turn.input.clone(),
                        }]
                    });
                self.sources.insert(turn.id.clone(), sources);
            }
            EventPayload::InputQueued { input } if input.review.is_none() => {
                self.sources.insert(
                    input.id.clone(),
                    vec![Source {
                        seq: event.seq,
                        input: input.input.clone(),
                    }],
                );
            }
            EventPayload::InputRemoved {
                input_ids,
                reason,
                turn_id,
            } => {
                for id in input_ids {
                    let Some(sources) = self.sources.remove(id) else {
                        continue;
                    };
                    if matches!(
                        reason,
                        InputRemovalReason::Steered | InputRemovalReason::Merged
                    ) {
                        if let Some(turn) = turn_id {
                            self.sources
                                .entry(turn.clone())
                                .or_default()
                                .extend(sources.clone());
                            if let Some(pending) = &mut self.pending
                                && pending.turn == *turn
                            {
                                pending.sources.extend(sources);
                            }
                        }
                    } else if *reason == InputRemovalReason::Interrupted
                        && let Some(pending) = &mut self.pending
                        && !pending.removed
                    {
                        pending.sources.extend(sources);
                    }
                }
            }
            EventPayload::TurnRetracted { turn_id } => {
                self.sources.remove(turn_id);
                if let Some(pending) = &mut self.pending
                    && pending.turn == *turn_id
                {
                    pending.removed = true;
                }
            }
            EventPayload::TurnCompleted { turn } => {
                self.sources.remove(&turn.id);
                self.response(&turn.id, false);
            }
            _ => {}
        }
    }

    pub fn take_restored(&mut self) -> Option<(Vec<Vec<UserInput>>, bool)> {
        let restored = if self.sending == 0
            && self
                .pending
                .as_ref()
                .is_some_and(|p| p.removed && p.accepted)
        {
            let pending = self.pending.take().expect("ready retraction");
            let mut missing_images = false;
            let inputs = pending
                .sources
                .into_iter()
                .map(|source| {
                    self.originals.remove(&source.seq).unwrap_or_else(|| {
                        source
                            .input
                            .into_iter()
                            .map(|part| match part {
                                UserInput::LocalImage { path, name } => {
                                    missing_images = true;
                                    UserInput::Text {
                                        text: format!("[image: {}]", name.unwrap_or(path)),
                                    }
                                }
                                part => part,
                            })
                            .collect()
                    })
                })
                .collect();
            Some((inputs, missing_images))
        } else {
            None
        };
        self.originals.retain(|seq, _| {
            *seq > self.last_seq
                || self
                    .sources
                    .values()
                    .flatten()
                    .any(|source| source.seq == *seq)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.sources.iter().any(|source| source.seq == *seq))
        });
        restored
    }
}
