use canter_core::usage::{ModelPrice, builtin_prices};
use canter_protocol::{BillingKind, Event, TokenUsageBreakdown};

#[test]
fn utc_boundaries_weekends_and_single_price_configuration() {
    let prices = builtin_prices();
    let flat: ModelPrice =
        serde_json::from_str(r#"{"peak":{"cache_hit":1,"cache_miss":2,"output":3}}"#).unwrap();
    let tokens = TokenUsageBreakdown {
        input_tokens: 1_000_000,
        cached_input_tokens: 800_000,
        output_tokens: 100_000,
        reasoning_output_tokens: 50_000,
        total_tokens: 1_100_000,
    };
    // Monday 1970-01-05 UTC, half-open peak intervals; every hour of the weekend.
    for (second, peak) in [
        (3599, false),
        (3600, true),
        (14399, true),
        (14400, false),
        (21599, false),
        (21600, true),
        (35999, true),
        (36000, false),
    ] {
        let at = (345_600 + second) * 1000;
        let expected = if peak { 0.1848 } else { 0.0924 };
        assert!((prices["deepseek-flash"].charge(at, &tokens) - expected).abs() < 1e-10);
        assert!((flat.charge(at, &tokens) - 1.5).abs() < 1e-10);
    }
    for day in [9, 10] {
        // Saturday, Sunday
        for hour in 0..24 {
            let at = (day * 86_400 + hour * 3600) * 1000;
            assert!((prices["deepseek-flash"].charge(at, &tokens) - 0.0924).abs() < 1e-10);
            assert!((flat.charge(at, &tokens) - 1.5).abs() < 1e-10);
        }
    }
}

fn usage_event(
    seq: u64,
    model: &str,
    cost_usd: Option<f64>,
    billing_kind: Option<BillingKind>,
) -> canter_protocol::Event {
    let tokens = TokenUsageBreakdown {
        input_tokens: 100,
        cached_input_tokens: 40,
        output_tokens: 5,
        reasoning_output_tokens: 0,
        total_tokens: 105,
    };
    canter_protocol::Event {
        seq,
        session_id: "s1".into(),
        timestamp: 1_000 + seq as i64,
        payload: canter_protocol::EventPayload::SessionUsage {
            token_usage: canter_protocol::TokenUsage {
                request: Some(canter_protocol::RequestCharge {
                    request_id: None,
                    model: model.into(),
                    started_at: 1_000,
                    cost_usd,
                    billing_kind,
                }),
                cost: None,
                total: tokens.clone(),
                last: tokens,
                context_tokens: 105,
                model_context_window: Some(200_000),
                auto_compact_tokens: None,
            },
        },
    }
}

fn turn(id: &str, status: canter_protocol::TurnStatus) -> canter_protocol::Turn {
    let usage = (status == canter_protocol::TurnStatus::Completed).then(|| {
        serde_json::from_value(serde_json::json!({
            "total":{"inputTokens":0,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":0},
            "last":{"inputTokens":0,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0,"totalTokens":0},
            "contextTokens":0,"modelContextWindow":null
        }))
        .unwrap()
    });
    canter_protocol::Turn {
        usage,
        ..serde_json::from_value(serde_json::json!({
            "id":id,"items":[],"status":status,"error":null,"input":[],"clientName":"test","usage":null
        }))
        .unwrap()
    }
}

#[test]
fn plan_metered_and_unknown_charges_aggregate_independently_within_a_turn() {
    use canter_protocol::{
        BillingKind, Event, EventPayload, SessionSnapshot, TurnStatus, UsageCost, apply_event_mut,
    };

    let plan = usage_event(2, "subscription/gpt", None, Some(BillingKind::ChatgptPlan));
    let metered = usage_event(
        3,
        "metered/fake",
        Some(0.12),
        Some(BillingKind::TokenMetered),
    );
    let unknown = usage_event(4, "metered/unknown", None, Some(BillingKind::TokenMetered));
    let events = vec![
        Event {
            seq: 1,
            session_id: "s1".into(),
            timestamp: 1_001,
            payload: EventPayload::TurnStarted {
                turn: turn("t1", TurnStatus::InProgress),
                input_id: None,
            },
        },
        plan,
        metered,
        unknown,
        Event {
            seq: 5,
            session_id: "s1".into(),
            timestamp: 1_005,
            payload: EventPayload::TurnCompleted {
                turn: turn("t1", TurnStatus::Completed),
            },
        },
    ];
    let prices = Default::default();
    let plan_only = canter_core::usage::Ledger::replay(&events[..2], &prices);
    assert_eq!(
        plan_only.report.total.cost,
        UsageCost {
            usd: 0.0,
            unknown: false,
            chatgpt_plan: true,
        }
    );
    let mixed = canter_core::usage::Ledger::replay(&events[..3], &prices);
    assert_eq!(mixed.report.total.cost.usd, 0.12);
    assert!(!mixed.report.total.cost.unknown);
    assert!(mixed.report.total.cost.chatgpt_plan);

    let ledger = canter_core::usage::Ledger::replay(&events, &prices);
    assert_eq!(ledger.report.total.cost.usd, 0.12);
    assert!(ledger.report.total.cost.unknown);
    assert!(ledger.report.total.cost.chatgpt_plan);
    assert_eq!(ledger.report.total.cached_input_tokens, 120);
    assert_eq!(ledger.report.total.uncached_input_tokens, 180);
    assert_eq!(ledger.report.total.output_tokens, 15);
    assert_eq!(
        ledger.report.last_turn_cost,
        Some(ledger.report.total.cost.clone())
    );
    for (model, usd, unknown, plan) in [
        ("subscription/gpt", 0.0, false, true),
        ("metered/fake", 0.12, false, false),
        ("metered/unknown", 0.0, true, false),
    ] {
        let row = ledger
            .report
            .data
            .iter()
            .find(|row| row.model.as_deref() == Some(model))
            .unwrap();
        assert_eq!(row.cost.usd, usd);
        assert_eq!(row.cost.unknown, unknown);
        assert_eq!(row.cost.chatgpt_plan, plan);
    }

    let mut snapshot = SessionSnapshot::empty("s1");
    for event in &events {
        apply_event_mut(&mut snapshot, event).unwrap();
    }
    ledger.fill_costs(&mut snapshot);
    assert_eq!(
        snapshot.token_usage.unwrap().cost,
        Some(ledger.report.total.cost.clone())
    );
    assert_eq!(
        snapshot.turns[0].usage.as_ref().unwrap().cost,
        Some(ledger.report.total.cost)
    );
}

#[test]
fn old_gpt_records_stay_unknown_and_old_metered_records_keep_their_cost() {
    use canter_protocol::{BillingKind, EventPayload, SessionSnapshot};

    for event in [
        usage_event(1, "chatgpt/gpt", None, None),
        usage_event(1, "chatgpt/gpt", Some(0.12), None),
    ] {
        let ledger =
            canter_core::usage::Ledger::replay(std::slice::from_ref(&event), &Default::default());
        let EventPayload::SessionUsage { token_usage } = &event.payload else {
            unreachable!()
        };
        assert_eq!(
            ledger.report.total.cost.unknown,
            token_usage.request.as_ref().unwrap().cost_usd.is_none()
        );
        assert!(!ledger.report.total.cost.chatgpt_plan);
    }

    let mut old_without_request = usage_event(1, "unused", None, None);
    let EventPayload::SessionUsage { token_usage } = &mut old_without_request.payload else {
        unreachable!()
    };
    token_usage.request = None;
    let mut snapshot = SessionSnapshot::empty("s1");
    snapshot.settings.model = "chatgpt/gpt".into();
    let settings = Event {
        seq: 1,
        session_id: "s1".into(),
        timestamp: 1_000,
        payload: EventPayload::SessionUpdated {
            settings: snapshot.settings,
        },
    };
    old_without_request.seq = 2;
    let ledger =
        canter_core::usage::Ledger::replay(&[settings, old_without_request], &Default::default());
    assert!(ledger.report.total.cost.unknown);
    assert!(!ledger.report.total.cost.chatgpt_plan);

    let metered = canter_core::usage::request_charge(
        &canter_core::usage::builtin_prices(),
        "deepseek-flash",
        349_200_000,
        &TokenUsageBreakdown::default(),
    );
    assert_eq!(metered.billing_kind, Some(BillingKind::TokenMetered));
}
