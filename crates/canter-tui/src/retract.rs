use canter_protocol::{
    Event, EventPayload, InputRemovalReason, SessionSnapshot, TurnStatus, UserInput,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone)]
struct Source {
    seq: u64,
    input: Vec<UserInput>,
}

/// An interrupt this terminal sent with Esc.
struct Pending {
    turn: String,
    /// The turn's own input; comes back only when it was retracted.
    sources: Vec<Source>,
    /// Queued inputs the interrupt discarded; they always come back.
    queued: Vec<Source>,
    /// `turn.retracted` arrived.
    removed: bool,
    /// `turn.completed` arrived.
    completed: bool,
    /// The reply says this interrupt stopped the turn, or the reply was lost.
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
    /// Recover a completed planning turn's withdrawn queue using the same
    /// ordering and attachment fallback as an interrupt. In-flight send replies
    /// are not carried to the new session; unknown image paths stay placeholders.
    pub fn plan_handoff(&self, turn: &str) -> Self {
        let mut drafts = Self {
            sources: self.sources.clone(),
            originals: self.originals.clone(),
            last_seq: self.last_seq,
            ..Self::default()
        };
        drafts.interrupt(turn);
        drafts.response(turn, true);
        drafts
    }

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
            queued: Vec::new(),
            removed: false,
            completed: false,
            accepted: false,
        });
    }

    /// `stopped`: this terminal's interrupt stopped the turn (retracted or
    /// not), or its reply was lost; otherwise nothing comes back.
    pub fn response(&mut self, turn: &str, stopped: bool) {
        if let Some(pending) = &mut self.pending
            && pending.turn == turn
        {
            if stopped {
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
                        && !pending.completed
                    {
                        pending.queued.extend(sources);
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
                if let Some(pending) = &mut self.pending
                    && pending.turn == turn.id
                {
                    pending.completed = true;
                }
            }
            _ => {}
        }
    }

    pub fn take_restored(&mut self) -> Option<(Vec<Vec<UserInput>>, bool)> {
        let restored = if self.sending == 0
            && self
                .pending
                .as_ref()
                .is_some_and(|p| p.accepted && (p.removed || p.completed))
        {
            let pending = self.pending.take().expect("ready interrupt");
            let own = if pending.removed {
                pending.sources
            } else {
                Vec::new()
            };
            let mut missing_images = false;
            let inputs: Vec<Vec<UserInput>> = own
                .into_iter()
                .chain(pending.queued)
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
            (!inputs.is_empty()).then_some((inputs, missing_images))
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
                || self.pending.as_ref().is_some_and(|p| {
                    p.sources
                        .iter()
                        .chain(&p.queued)
                        .any(|source| source.seq == *seq)
                })
        });
        restored
    }
}
