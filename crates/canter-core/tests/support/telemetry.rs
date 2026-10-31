use super::*;

#[tokio::test]
async fn request_timing_measures_first_fragment_and_stream_completion() {
    struct TimedModel;
    impl Model for TimedModel {
        fn stream(&self, _: Request, _: CancellationToken) -> ModelStream<'_> {
            Box::pin(async_stream::stream! {
                yield Ok(ModelEvent::ItemCompleted {
                    item_id: "empty".into(),
                    content: canter_model::AssistantContent::Text { text: String::new() },
                });
                tokio::time::sleep(Duration::from_millis(25)).await;
                yield Ok(ModelEvent::ReasoningDelta { item_id: "think".into(), delta: "thinking".into() });
                tokio::time::sleep(Duration::from_millis(35)).await;
                yield Ok(ModelEvent::Finished(FinishReason::Stop));
            })
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        "timed".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        Arc::new(TimedModel),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("think")).await.unwrap();
    finished(&mut subscription).await;
    let record = store
        .events_after("timed", 0)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::TelemetryModelRequest { record } => Some(record),
            _ => None,
        })
        .unwrap();
    assert!(record.first_response_ms.unwrap() >= 25);
    assert!(record.duration_ms >= record.first_response_ms.unwrap() + 35);
}

#[tokio::test]
async fn read_truncation_records_source_bytes_without_inventing_original_tokens() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("large.txt"), "x".repeat(200_000)).unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call("read-large", "read", r#"{"path":"large.txt"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "read-size".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("read")).await.unwrap();
    finished(&mut subscription).await;
    let records: Vec<_> = store
        .events_after("read-size", 0)
        .unwrap()
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    let size = records
        .iter()
        .find(|e| e["type"] == "telemetry.toolResult")
        .unwrap();
    assert_eq!(size["truncated"], true);
    assert_eq!(size["sourceFileBytes"], 200_000);
    assert!(size.get("originalTokens").is_none());
    assert!(
        records
            .iter()
            .any(|e| e["type"] == "item.completed" && e["item"]["id"] == size["itemId"])
    );
}

#[tokio::test]
async fn disabled_telemetry_keeps_tags_usage_compaction_and_identical_model_requests() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut captured = Vec::new();
    for enabled in [false, true] {
        let name = if enabled { "on" } else { "off" };
        let usage = || {
            ModelEvent::Usage(canter_model::Usage {
                input_tokens: 10,
                output_tokens: 2,
                total_tokens: 12,
                ..Default::default()
            })
        };
        let reply = || {
            vec![
                ModelEvent::TextDelta {
                    item_id: "a".into(),
                    delta: "summary".into(),
                },
                usage(),
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        let model = ScriptModel::new(vec![
            vec![
                ModelEvent::Retrying {
                    error: canter_model::ModelError {
                        kind: canter_model::ErrorKind::Network,
                        status: None,
                        retry_after: None,
                        retries: 0,
                    },
                    attempt: 1,
                    max_retries: 10,
                    delay: Duration::from_millis(1),
                },
                call("tool", "shell", r#"{"command":"printf test > fixture"}"#),
                usage(),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            reply(),
            reply(),
        ]);
        let mut config = settings(temp.path());
        config.tags.insert("task".into(), "not-for-model".into());
        config.approval_policy = PermissionMode::AlwaysAsk;
        let options = CoreOptions {
            telemetry: enabled,
            ..Default::default()
        };
        let (session, _) = Session::create(
            store.clone(),
            name.into(),
            config,
            RuntimeContext::default(),
            model.clone(),
            options.clone(),
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe(0).await.unwrap();
        session.input(input("run")).await.unwrap();
        let opened = until(&mut subscription, |e| {
            matches!(e, EventPayload::RequestOpened { .. })
        })
        .await;
        let EventPayload::RequestOpened { request } = opened.payload else {
            unreachable!()
        };
        session
            .resolve(
                &request.id,
                RequestResolveParams {
                    decision: Some(ApprovalDecision::Accept),
                    answer: None,
                    feedback: None,
                    plan: None,
                    execpolicy_amendment: None,
                    client_name: "test".into(),
                },
            )
            .await
            .unwrap();
        finished(&mut subscription).await;
        session.compact().await.unwrap();
        finished(&mut subscription).await;
        let events = store.events_after(name, 0).unwrap();
        let json = serde_json::to_string(&events).unwrap();
        if !enabled {
            assert!(!json.contains("telemetry."));
            assert!(!json.contains("\"requestId\":\"request-"));
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.payload, EventPayload::SessionUsage { .. }))
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.payload,
                    EventPayload::ItemCompleted {
                        item: Item::ModelRetry { .. },
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::ContextCompaction { summary, .. }, .. } if summary == "summary")));
        session.close().await.unwrap();
        drop(session);
        let (reopened, response) = Session::open(
            store.clone(),
            name.into(),
            RuntimeContext::default(),
            ScriptModel::new(vec![]),
            options,
        )
        .await
        .unwrap();
        assert_eq!(response.session.settings.tags["task"], "not-for-model");
        reopened.close().await.unwrap();
        let requests = serde_json::to_value(model.requests.lock().unwrap().clone()).unwrap();
        assert!(!requests.to_string().contains("not-for-model"));
        captured.push(requests);
    }
    let left = captured[0].to_string();
    let right = captured[1].to_string();
    let difference = left
        .chars()
        .zip(right.chars())
        .position(|(a, b)| a != b)
        .unwrap_or(left.len().min(right.len()));
    let excerpt = |text: &str| {
        text.chars()
            .skip(difference.saturating_sub(60))
            .take(160)
            .collect::<String>()
    };
    assert!(
        left == right,
        "telemetry must not change model inputs or cache prefixes; first difference at {difference}: {:?} / {:?}",
        excerpt(&left),
        excerpt(&right)
    );
}

#[tokio::test]
async fn settings_are_captured_again_after_a_session_update() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "settings".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("first")).await.unwrap();
    finished(&mut subscription).await;
    session.update(serde_json::from_value(serde_json::json!({"model":"other", "reasoningEffort":"low", "approvalPolicy":"alwaysAsk", "planMode":true})).unwrap()).await.unwrap();
    session.input(input("second")).await.unwrap();
    finished(&mut subscription).await;
    let records: Vec<_> = store
        .events_after("settings", 0)
        .unwrap()
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .filter(|e| e["type"] == "telemetry.modelRequest")
        .collect();
    assert_eq!(records[0]["settings"]["model"], "fake");
    assert_eq!(
        records[1]["settings"],
        serde_json::json!({"model":"other", "reasoningEffort":"low", "approvalPolicy":"alwaysAsk", "planMode":true})
    );
}

#[tokio::test]
async fn private_requests_are_recorded_without_saving_their_content() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let reply = || {
        vec![
            ModelEvent::TextDelta {
                item_id: "a".into(),
                delta: "private-result".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ]
    };
    let model = ScriptModel::new(vec![
        vec![ModelEvent::Finished(FinishReason::Stop)],
        reply(),
        reply(),
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "private".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("context")).await.unwrap();
    finished(&mut subscription).await;
    let mut stream = session.btw("private-question".into()).await.unwrap();
    while let Some(event) = stream.recv().await {
        if matches!(event, BtwEvent::Completed) {
            break;
        }
    }
    let mut stream = session.handoff(None).await.unwrap();
    while let Some(event) = stream.recv().await {
        if matches!(event, BtwEvent::Completed) {
            break;
        }
    }
    let events = serde_json::to_value(store.events_after("private", 0).unwrap()).unwrap();
    for purpose in ["btw", "handoff"] {
        assert!(
            events
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "telemetry.modelRequest"
                    && e["purpose"] == purpose
                    && e.get("turnId").is_none())
        );
    }
    assert!(!events.to_string().contains("private-question"));
    assert!(!events.to_string().contains("private-result"));
}

#[tokio::test]
async fn manual_and_automatic_compaction_record_before_and_after() {
    for automatic in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let reply = |text: String| {
            vec![
                ModelEvent::TextDelta {
                    item_id: "a".into(),
                    delta: text,
                },
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        let model = ScriptModel::new(vec![
            reply("history ".repeat(15_000)),
            reply("summary".into()),
            reply("done".into()),
        ]);
        let (session, _) = Session::create(
            store.clone(),
            "compact".into(),
            settings(temp.path()),
            RuntimeContext::default(),
            model,
            CoreOptions {
                auto_compact_tokens: automatic.then_some(30_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe(0).await.unwrap();
        session.input(input("first")).await.unwrap();
        finished(&mut subscription).await;
        if automatic {
            session.input(input("next")).await.unwrap();
        } else {
            session.compact().await.unwrap();
        }
        finished(&mut subscription).await;
        let events: Vec<_> = store
            .events_after("compact", 0)
            .unwrap()
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        let record = events
            .iter()
            .find(|e| e["type"] == "telemetry.compaction")
            .expect("compaction telemetry");
        assert_eq!(record["estimated"], true);
        assert_eq!(record["succeeded"], true);
        assert!(record["beforeTokens"].as_u64().unwrap() > record["afterTokens"].as_u64().unwrap());
        assert!(record["removedMessages"].as_u64().unwrap() > 0);
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "telemetry.modelRequest" && e["purpose"] == "compaction")
        );
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "item.completed" && e["item"]["id"] == record["itemId"])
        );
    }
}

#[tokio::test]
async fn tool_results_report_actual_delivered_size_and_original_truncation_size() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call(
                "large",
                "shell",
                r#"{"command":"awk 'BEGIN {for(i=0;i<60000;i++) printf \"x\"}'"}"#,
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "tools".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("run")).await.unwrap();
    finished(&mut subscription).await;
    let events: Vec<_> = store
        .events_after("tools", 0)
        .unwrap()
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    let record = events
        .iter()
        .find(|e| e["type"] == "telemetry.toolResult")
        .expect("tool telemetry");
    assert_eq!(record["toolCallId"], "large");
    assert_eq!(record["truncated"], true);
    assert_eq!(record["originalTokens"], 20_000);
    let requests = model.requests.lock().unwrap();
    let text = requests[1]
        .messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult { text, .. } => Some(text),
            _ => None,
        })
        .unwrap();
    assert_eq!(record["resultTokens"], (text.len() as u64).div_ceil(3));
    assert!(text.contains("[truncated"));
    let last = events
        .iter()
        .rfind(|e| e["type"] == "telemetry.modelRequest")
        .unwrap();
    assert!(
        last["context"]["toolResultTokens"].as_u64().unwrap()
            > record["resultTokens"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn approval_wait_records_accept_decline_and_cancel_once() {
    for decision in [
        Some(ApprovalDecision::Accept),
        Some(ApprovalDecision::Decline),
        None,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let model = ScriptModel::new(vec![
            vec![
                call(
                    "tool1",
                    "shell",
                    r#"{"command":"printf hello > output.txt"}"#,
                ),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![ModelEvent::Finished(FinishReason::Stop)],
        ]);
        let mut config = settings(temp.path());
        config.approval_policy = PermissionMode::AlwaysAsk;
        let (session, _) = Session::create(
            store.clone(),
            "wait".into(),
            config,
            RuntimeContext::default(),
            model,
            CoreOptions::default(),
        )
        .await
        .unwrap();
        let mut subscription = session.subscribe(0).await.unwrap();
        session.input(input("run")).await.unwrap();
        let opened = until(&mut subscription, |e| {
            matches!(e, EventPayload::RequestOpened { .. })
        })
        .await;
        let EventPayload::RequestOpened { request } = opened.payload else {
            unreachable!()
        };
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(decision) = decision {
            session
                .resolve(
                    &request.id,
                    RequestResolveParams {
                        decision: Some(decision),
                        answer: None,
                        feedback: None,
                        plan: None,
                        execpolicy_amendment: None,
                        client_name: "test".into(),
                    },
                )
                .await
                .unwrap();
        } else {
            session
                .interrupt(SessionInterruptParams {
                    turn_id: request.turn_id.clone(),
                    retract: None,
                })
                .await
                .unwrap();
        }
        finished(&mut subscription).await;
        let records: Vec<_> = store
            .events_after("wait", 0)
            .unwrap()
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .filter(|e| e["type"] == "telemetry.approval")
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["requestId"], request.id);
        assert!(records[0]["durationMs"].as_u64().unwrap() >= 25);
        assert_eq!(
            records[0]["outcome"],
            match decision {
                Some(ApprovalDecision::Accept) => "approved",
                Some(_) => "declined",
                None => "cancelled",
            }
        );
    }
}

#[tokio::test]
async fn request_records_context_settings_timing_and_usage_link() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![
        ModelEvent::TextDelta {
            item_id: "a".into(),
            delta: "answer".into(),
        },
        ModelEvent::Usage(canter_model::Usage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        }),
        ModelEvent::Finished(FinishReason::Stop),
    ]]);
    let (session, _) = Session::create(
        store.clone(),
        "telemetry".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut subscription = session.subscribe(0).await.unwrap();
    session.input(input("question")).await.unwrap();
    finished(&mut subscription).await;
    let events = store.events_after("telemetry", 0).unwrap();
    let events: Vec<_> = events
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    let record = events
        .iter()
        .find(|e| e["type"] == "telemetry.modelRequest")
        .expect("request telemetry");
    assert_eq!(record["settings"]["model"], "fake");
    assert_eq!(record["settings"]["approvalPolicy"], "allowAll");
    assert_eq!(record["settings"]["planMode"], false);
    assert!(record["context"]["systemTokens"].as_u64().unwrap() > 0);
    assert!(record["context"]["toolDefinitionTokens"].as_u64().unwrap() > 0);
    assert!(record["context"]["conversationTokens"].as_u64().unwrap() > 0);
    assert_eq!(record["context"]["toolResultTokens"], 0);
    assert_eq!(record["context"]["estimated"], true);
    assert!(record["firstResponseMs"].as_u64().unwrap() <= record["durationMs"].as_u64().unwrap());
    let usage = events
        .iter()
        .find(|e| e["type"] == "session.usage")
        .unwrap();
    assert_eq!(
        usage["tokenUsage"]["request"]["requestId"],
        record["requestId"]
    );
}
