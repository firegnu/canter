use canter_protocol::*;

fn snapshot() -> SessionSnapshot {
    let mut snapshot = SessionSnapshot::empty("s1");
    let mut output = String::with_capacity(1_000_000);
    output.push_str("existing output");
    snapshot.turns.push(Turn {
        review: None,
        id: "t1".into(),
        items: vec![Item::AgentMessage {
            status: None,
            id: "a1".into(),
            text: output,
        }],
        status: TurnStatus::InProgress,
        error: None,
        input: vec![UserInput::Text {
            text: "hello".into(),
        }],
        client_name: "terminal".into(),
        usage: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    });
    snapshot
}

fn event(payload: EventPayload) -> Event {
    Event {
        seq: 1,
        session_id: "s1".into(),
        timestamp: 10,
        payload,
    }
}

#[test]
fn mutable_replay_keeps_large_buffers_and_matches_pure_replay() {
    let mut state = snapshot();
    let Item::AgentMessage { text, .. } = &state.turns[0].items[0] else {
        unreachable!()
    };
    let pointer = text.as_ptr();
    let event = event(EventPayload::ItemDelta {
        turn_id: "t1".into(),
        item_id: "a1".into(),
        delta: ItemDelta::AgentMessage {
            delta: " plus delta".into(),
        },
    });
    let expected = apply_event(&state, &event).unwrap();
    apply_event_mut(&mut state, &event).unwrap();
    assert_eq!(state, expected);
    let Item::AgentMessage { text, .. } = &state.turns[0].items[0] else {
        unreachable!()
    };
    assert_eq!(
        text.as_ptr(),
        pointer,
        "an in-place delta with spare capacity must not copy the accumulated output"
    );
}

#[test]
fn rejected_events_leave_the_mutable_snapshot_unchanged() {
    let mut state = snapshot();
    state.turns[0].items.push(Item::Reasoning {
        status: None,
        id: "reasoning".into(),
        summary: vec!["first part".into()],
        content: Vec::new(),
        duration_ms: None,
    });
    let original = state.clone();
    let invalid = [
        event(EventPayload::ItemDelta {
            turn_id: "t1".into(),
            item_id: "reasoning".into(),
            delta: ItemDelta::ReasoningSummary {
                summary_index: 2,
                delta: "invalid gap".into(),
            },
        }),
        event(EventPayload::TurnStarted {
            turn: state.turns[0].clone(),
            input_id: None,
        }),
        event(EventPayload::ItemStarted {
            turn_id: "missing".into(),
            item: Item::AgentMessage {
                status: None,
                id: "new".into(),
                text: "text".into(),
            },
        }),
        event(EventPayload::ItemDelta {
            turn_id: "t1".into(),
            item_id: "a1".into(),
            delta: ItemDelta::CommandExecution {
                delta: "wrong kind".into(),
            },
        }),
        Event {
            session_id: "other".into(),
            ..event(EventPayload::Warning {
                message: "warning".into(),
            })
        },
        Event {
            seq: 2,
            ..event(EventPayload::Warning {
                message: "warning".into(),
            })
        },
    ];
    for event in invalid {
        assert!(apply_event_mut(&mut state, &event).is_err());
        assert_eq!(state, original);
    }
}

#[test]
fn completing_a_turn_preserves_item_events_and_original_input() {
    let mut state = snapshot();
    let original_items = state.turns[0].items.clone();
    let original_input = state.turns[0].input.clone();
    let completed = Turn {
        review: None,
        id: "t1".into(),
        items: Vec::new(),
        status: TurnStatus::Failed,
        error: Some(ErrorInfo {
            message: "failed".into(),
            additional_details: None,
        }),
        input: Vec::new(),
        client_name: String::new(),
        usage: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    apply_event_mut(
        &mut state,
        &event(EventPayload::TurnCompleted { turn: completed }),
    )
    .unwrap();
    assert_eq!(state.turns[0].items, original_items);
    assert_eq!(state.turns[0].input, original_input);
    assert_eq!(state.turns[0].client_name, "terminal");
    assert_eq!(state.turns[0].status, TurnStatus::Failed);
    assert_eq!(state.turns[0].error.as_ref().unwrap().message, "failed");
}

#[test]
fn removed_inputs_leave_the_queue_for_every_removal_reason() {
    for reason in ["steered", "merged", "interrupted", "closed", "futureReason"] {
        let mut state = snapshot();
        state.queued_inputs = ["q1", "q2", "q3"]
            .map(|id| QueuedInput {
                review: None,
                id: id.into(),
                input: vec![UserInput::Text { text: id.into() }],
                client_name: "terminal".into(),
            })
            .into();
        let removed: Event = serde_json::from_value(serde_json::json!({
            "seq":1, "sessionId":"s1", "timestamp":10, "type":"input.removed",
            "inputIds":["q1", "q2"], "reason":reason
        }))
        .unwrap();
        apply_event_mut(&mut state, &removed).unwrap();
        assert_eq!(
            state
                .queued_inputs
                .iter()
                .map(|input| input.id.as_str())
                .collect::<Vec<_>>(),
            ["q3"]
        );
        apply_event_mut(&mut state, &removed).unwrap();
        assert_eq!(state.queued_inputs.len(), 1);
    }
}
