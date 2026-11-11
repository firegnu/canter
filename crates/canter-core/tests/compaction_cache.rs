#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn reply(delta: Value, reason: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    ))
}

fn answer(text: &str) -> Reply {
    reply(json!({"content":text}), "stop")
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
}

impl Fixture {
    async fn new(replies: Vec<Reply>, options: CoreOptions) -> Self {
        Self::with_history(replies, options, &[]).await
    }

    async fn with_history(replies: Vec<Reply>, options: CoreOptions, history: &[Value]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
        let server = Server::start(replies).await;
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                (
                    "CANTER_HOME".into(),
                    root.path().join("state").display().to_string(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        {
            let settings = serde_json::from_value(json!({"cwd":root.path(),"name":null,"model":"fake","reasoningEffort":"high","approvalPolicy":"allowAll"})).unwrap();
            let mut writer = store.create("cache", settings, 1).unwrap();
            for entry in history {
                writer.append_model_history(entry).unwrap();
            }
        }
        let (session, _) = Session::open(
            store.clone(),
            "cache".into(),
            runtime,
            Arc::new(
                ChatCompletions::new(&server.base_url, ApiKey::new(KEY))
                    .unwrap()
                    .with_headers(
                        serde_json::from_value(json!({"x-test-session":"{session_id}"})).unwrap(),
                    ),
            ),
            options,
        )
        .await
        .unwrap();
        Self {
            root,
            store,
            session,
            server,
        }
    }

    async fn events(&self) -> Subscription {
        self.session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap()
    }

    async fn send(&self, text: &str) {
        let mut events = self.events().await;
        self.session
            .input(SessionInputParams {
                from_suggestion: false,
                input: vec![UserInput::Text { text: text.into() }],
                mode: InputMode::Queue,
                client_name: "test".into(),
                non_interactive: false,
                approval_policy: None,
                review: None,
            })
            .await
            .unwrap();
        finished(&mut events).await;
    }

    async fn compact(&self) {
        let mut events = self.events().await;
        self.session.compact().await.unwrap();
        finished(&mut events).await;
    }

    async fn close(self) {
        self.session.close().await.unwrap();
        self.server.stop().await;
    }
}

async fn finished(events: &mut Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if let EventPayload::TurnCompleted { turn } = event.payload {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn manual_compaction_items_report_context_estimates_without_telemetry() {
    let fixture = Fixture::new(
        vec![answer("Earlier answer."), answer("Summary.")],
        CoreOptions {
            telemetry: false,
            ..Default::default()
        },
    )
    .await;
    fixture.send("Keep these words.").await;
    let before = fixture.session.context().await.estimated_tokens;
    fixture.compact().await;
    let after = fixture.session.context().await.estimated_tokens;
    let events: Vec<_> = fixture
        .store
        .events_after("cache", 0)
        .unwrap()
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    let started = events
        .iter()
        .find(|event| {
            event["type"] == "item.started" && event["item"]["type"] == "contextCompaction"
        })
        .unwrap();
    assert_eq!(started["item"]["beforeTokens"], before);
    assert!(started["item"].get("afterTokens").is_none());
    assert!(started["item"].get("part").is_none());
    let completed = events
        .iter()
        .find(|event| {
            event["type"] == "item.completed" && event["item"]["type"] == "contextCompaction"
        })
        .unwrap();
    assert_eq!(completed["item"]["beforeTokens"], before);
    assert_eq!(completed["item"]["afterTokens"], after);
    assert!(completed["item"].get("part").is_none());
    fixture.close().await;
}

#[tokio::test]
async fn automatic_compaction_items_match_context_while_model_responses_are_held() {
    // Hold each HTTP response so /context observes exactly the start and end
    // of compaction, before the ordinary reply adds anything to the history.
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let send = send.clone();
            async move {
                let (release, response) = tokio::sync::oneshot::channel::<String>();
                send.send(release).unwrap();
                (
                    [("content-type", "text/event-stream")],
                    response.await.unwrap(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
    {
        let settings = serde_json::from_value(
            json!({"cwd":root.path(),"name":null,"model":"fake","approvalPolicy":"allowAll"}),
        )
        .unwrap();
        let mut writer = store.create("auto", settings, 1).unwrap();
        writer.append_model_history(&json!({"type":"message","message":{"type":"user","text":"Keep these words."},"user_input":true})).unwrap();
        writer.append_model_history(&json!({"type":"message","message":{"type":"assistant","content":[{"type":"text","text":"old output ".repeat(16_000)}]}})).unwrap();
    }
    let (session, _) = Session::open(
        store.clone(),
        "auto".into(),
        RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                (
                    "CANTER_HOME".into(),
                    root.path().join("state").display().to_string(),
                ),
            ]
            .into(),
            ..Default::default()
        },
        Arc::new(ChatCompletions::new(&base_url, ApiKey::new(KEY)).unwrap()),
        CoreOptions {
            auto_compact_tokens: Some(30_000),
            telemetry: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(serde_json::from_value(json!({"input":[{"type":"text","text":"Continue."}],"mode":"queue","clientName":"test"})).unwrap()).await.unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(10), receive.recv())
        .await
        .unwrap()
        .unwrap();
    let before = session.context().await.estimated_tokens;
    let snapshot = session.snapshot().await.session;
    let item = snapshot
        .turns
        .last()
        .unwrap()
        .items
        .iter()
        .find(|item| matches!(item, Item::ContextCompaction { .. }))
        .unwrap();
    let item = serde_json::to_value(item).unwrap();
    assert_eq!(item["beforeTokens"], before);
    assert!(item.get("afterTokens").is_none());
    summary.send(answer("Summary.").body).unwrap();
    let ordinary = tokio::time::timeout(Duration::from_secs(10), receive.recv())
        .await
        .unwrap()
        .unwrap();
    let after = session.context().await.estimated_tokens;
    let completed = store
        .events_after("auto", 0)
        .unwrap()
        .into_iter()
        .find_map(|event| match event.payload {
            EventPayload::ItemCompleted {
                item: item @ Item::ContextCompaction { .. },
                ..
            } => Some(serde_json::to_value(item).unwrap()),
            _ => None,
        })
        .unwrap();
    assert_eq!(completed["beforeTokens"], before);
    assert_eq!(completed["afterTokens"], after);
    assert!(after < before);
    assert!(completed.get("part").is_none());
    ordinary.send(answer("Continuing.").body).unwrap();
    finished(&mut events).await;
    session.close().await.unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn failed_and_interrupted_compactions_do_not_report_after_tokens() {
    for interrupted in [false, true] {
        let replies = if interrupted {
            let mut held = Reply::sse("");
            held.hang = true;
            vec![held]
        } else {
            vec![
                context_too_long(),
                reply(json!({"content":"Incomplete summary"}), "length"),
            ]
        };
        let fixture = Fixture::with_history(replies, CoreOptions::default(), &[
            json!({"type":"message","message":{"type":"user","text":"Keep these words."},"user_input":true}),
        ]).await;
        let before = fixture.session.context().await.estimated_tokens;
        let mut events = fixture.events().await;
        let response = fixture.session.compact().await.unwrap();
        let seen = tokio::time::timeout(Duration::from_secs(10), async {
            let mut seen = Vec::new();
            loop {
                let event = events.recv().await.unwrap().unwrap();
                if interrupted
                    && matches!(
                        event.payload,
                        EventPayload::ItemStarted {
                            item: Item::ContextCompaction { .. },
                            ..
                        }
                    )
                {
                    fixture
                        .session
                        .interrupt(
                            serde_json::from_value(json!({"turnId":response.turn_id})).unwrap(),
                        )
                        .await
                        .unwrap();
                }
                let done = matches!(event.payload, EventPayload::TurnCompleted { .. });
                seen.push(serde_json::to_value(event).unwrap());
                if done {
                    break seen;
                }
            }
        })
        .await
        .unwrap();
        let completed = seen
            .iter()
            .find(|event| {
                event["type"] == "item.completed" && event["item"]["type"] == "contextCompaction"
            })
            .unwrap();
        assert_eq!(completed["item"]["beforeTokens"], before);
        assert!(completed["item"].get("afterTokens").is_none());
        assert_eq!(
            completed["item"]["summary"],
            "Compaction did not finish; the original context is kept."
        );
        // Ending an interrupted turn appends its existing interruption notice.
        // The original history stays intact and no summary replaces it.
        let history = fixture.store.model_history::<Value>("cache").unwrap();
        assert_eq!(history[0]["message"]["text"], "Keep these words.");
        assert!(history.iter().all(|entry| entry["type"] != "compaction"));
        fixture.close().await;
    }
}

#[tokio::test]
async fn compaction_replaces_old_images_but_preserves_new_images_and_legacy_replay() {
    fn image_count(request: &Value) -> usize {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter(|part| part["type"] == "image_url")
            .count()
    }

    for fallback in [false, true] {
        let mut users = vec![
            json!({"type":"user","text":"Keep the oldest instruction."}),
            json!({"type":"user_parts","content":[
                {"type":"text","text":"User text before."},
                {"type":"image","path":"image-1568-old.png","name":"pasted.png"},
                {"type":"text","text":"User text after."}
            ]}),
        ];
        users.extend((0..24).map(|index| {
            json!({"type":"tool_images","content":[
                {"type":"text","text":format!("Screenshot {index}.")},
                {"type":"image","path":"image-1568-old.png","name":format!("screen-{index}.png")}
            ]})
        }));
        let mut messages = users.clone();
        messages.push(json!({"type":"user","text":"[Context summary]\nEarlier summary."}));
        let legacy = json!({
            "type":"compaction","summary":"Earlier summary.",
            "messages":messages,"user_messages":["Keep the oldest instruction."],
            "user_inputs":users
        });
        let mut replies = vec![answer("Before compaction.")];
        if fallback {
            replies.push(context_too_long());
        }
        replies.extend([
            answer("Summary of the pictures."),
            reply(
                json!({"tool_calls":[{"index":0,"id":"fresh","type":"function",
                    "function":{"name":"read","arguments":"{\"path\":\"fresh.png\"}"}}]}),
                "tool_calls",
            ),
            answer("Fresh image."),
            answer("After reopening."),
        ]);
        let options = CoreOptions {
            image_models: vec!["fake".into()],
            sandbox: false,
            ..Default::default()
        };
        let mut fixture =
            Fixture::with_history(replies, options.clone(), std::slice::from_ref(&legacy)).await;
        image::RgbImage::new(2, 2)
            .save(
                fixture
                    .store
                    .image_path("cache", "image-1568-old.png")
                    .unwrap(),
            )
            .unwrap();
        image::RgbImage::new(3, 3)
            .save(fixture.root.path().join("fresh.png"))
            .unwrap();
        fixture.send("Continue from the old record.").await;
        {
            let requests = fixture.server.requests.lock().unwrap();
            assert_eq!(image_count(&requests[0]), 25, "legacy images must replay");
            let wire = requests[0]["messages"].as_array().unwrap();
            assert_eq!(
                wire[1],
                json!({"role":"user","content":"Keep the oldest instruction."})
            );
            assert_eq!(wire[2]["content"][0]["text"], "User text before.");
            assert_eq!(wire[2]["content"][2]["text"], "User text after.");
            for index in 0..24 {
                assert_eq!(
                    wire[index + 3]["content"][0]["text"],
                    format!("Screenshot {index}.")
                );
                assert_eq!(wire[index + 3]["content"][1], wire[2]["content"][1]);
            }
            assert_eq!(
                wire[27],
                json!({"role":"user","content":"[Context summary]\nEarlier summary."})
            );
        }
        fixture.compact().await;
        fixture.send("Read the fresh screenshot.").await;
        let mut expected_reopened = {
            let requests = fixture.server.requests.lock().unwrap();
            let after = requests.len() - 2;
            for summary in &requests[1..after] {
                assert_eq!(
                    image_count(summary),
                    25,
                    "summarization still sees the images"
                );
            }
            assert_eq!(
                image_count(&requests[after]),
                0,
                "compacted requests must not carry old images"
            );
            assert_eq!(
                image_count(&requests[after + 1]),
                1,
                "new tool images must still be sent"
            );
            requests[after + 1].clone()
        };
        let history = fixture.store.model_history::<Value>("cache").unwrap();
        assert_eq!(
            history[0], legacy,
            "the old compaction record must stay unchanged"
        );
        let compacted = history
            .iter()
            .rfind(|entry| entry["type"] == "compaction")
            .unwrap();
        let mut expected_users = vec![
            json!({"type":"user","text":"Keep the oldest instruction."}),
            json!({"type":"user_parts","content":[
                {"type":"text","text":"User text before."},
                {"type":"text","text":"[image: pasted.png]"},
                {"type":"text","text":"User text after."}
            ]}),
        ];
        expected_users.extend((0..24).map(|index| {
            json!({"type":"tool_images","content":[
                {"type":"text","text":format!("Screenshot {index}.")},
                {"type":"text","text":format!("[image: screen-{index}.png]")}
            ]})
        }));
        expected_users.push(json!({"type":"user","text":"Continue from the old record."}));
        // All 24 screenshot messages plus the older user text fit once images
        // are replaced; charging 1024 tokens per old image would lose them.
        assert_eq!(compacted["user_inputs"], json!(expected_users));
        let mut expected_messages = expected_users;
        expected_messages
            .push(json!({"type":"user","text":"[Context summary]\nSummary of the pictures."}));
        assert_eq!(compacted["messages"], json!(expected_messages));
        {
            let requests = fixture.server.requests.lock().unwrap();
            let wire = requests[requests.len() - 2]["messages"].as_array().unwrap();
            for (stored, sent) in expected_messages.iter().zip(&wire[1..]) {
                let content = stored.get("content").unwrap_or(&stored["text"]);
                assert_eq!(sent, &json!({"role":"user","content":content}));
            }
        }
        expected_reopened["messages"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"role":"assistant","content":"Fresh image."}),
                json!({"role":"user","content":"Reopened turn."}),
            ]);
        fixture.session.close().await.unwrap();
        fixture.store = Arc::new(Store::new(fixture.root.path().join("sessions")).unwrap());
        let (session, _) = Session::open(
            fixture.store.clone(),
            "cache".into(),
            RuntimeContext {
                environment: [
                    (
                        "HOME".into(),
                        fixture.root.path().join("home").display().to_string(),
                    ),
                    (
                        "CANTER_HOME".into(),
                        fixture.root.path().join("state").display().to_string(),
                    ),
                ]
                .into(),
                ..Default::default()
            },
            Arc::new(ChatCompletions::new(&fixture.server.base_url, ApiKey::new(KEY)).unwrap()),
            options,
        )
        .await
        .unwrap();
        fixture.session = session;
        fixture.send("Reopened turn.").await;
        {
            let requests = fixture.server.requests.lock().unwrap();
            assert_eq!(requests.last().unwrap(), &expected_reopened);
            assert_eq!(image_count(requests.last().unwrap()), 1);
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn session_id_survives_compaction_and_reopening_with_a_fresh_store() {
    for fallback in [false, true] {
        let mut replies = vec![answer("Before compaction.")];
        if fallback {
            replies.push(context_too_long());
        }
        replies.extend([
            answer("Summary."),
            answer("After compaction."),
            answer("After reopening."),
        ]);
        let mut fixture = Fixture::new(replies, CoreOptions::default()).await;
        fixture.send("First turn.").await;
        fixture.compact().await;
        fixture.send("Next turn.").await;
        fixture.session.close().await.unwrap();
        fixture.store = Arc::new(Store::new(fixture.root.path().join("sessions")).unwrap());
        let (session, _) = Session::open(
            fixture.store.clone(),
            "cache".into(),
            RuntimeContext {
                environment: [
                    (
                        "HOME".into(),
                        fixture.root.path().join("home").display().to_string(),
                    ),
                    (
                        "CANTER_HOME".into(),
                        fixture.root.path().join("state").display().to_string(),
                    ),
                ]
                .into(),
                ..Default::default()
            },
            Arc::new(
                ChatCompletions::new(&fixture.server.base_url, ApiKey::new(KEY))
                    .unwrap()
                    .with_headers(
                        serde_json::from_value(json!({"x-test-session":"{session_id}"})).unwrap(),
                    ),
            ),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        fixture.session = session;
        fixture.send("Reopened turn.").await;
        {
            let headers = fixture.server.headers.lock().unwrap();
            assert_eq!(headers.len(), if fallback { 5 } else { 4 });
            for header in headers.iter() {
                assert_eq!(
                    header.get("x-test-session").and_then(|v| v.to_str().ok()),
                    Some("cache")
                );
            }
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn review_btw_and_handoff_use_the_owning_session_id() {
    let fixture = Fixture::new(
        (0..4).map(|_| answer("Done.")).collect(),
        CoreOptions::default(),
    )
    .await;
    fixture.send("First turn.").await;
    let mut events = fixture.events().await;
    fixture
        .session
        .input(SessionInputParams {
            from_suggestion: false,
            input: vec![UserInput::Text {
                text: "Review the current work.".into(),
            }],
            mode: InputMode::Queue,
            client_name: "test".into(),
            non_interactive: false,
            approval_policy: None,
            review: Some("Current work".into()),
        })
        .await
        .unwrap();
    finished(&mut events).await;
    for handoff in [false, true] {
        let mut stream = if handoff {
            fixture.session.handoff(None).await.unwrap()
        } else {
            fixture.session.btw("What changed?".into()).await.unwrap()
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match stream.recv().await.expect("private answer must finish") {
                    BtwEvent::Delta { .. } => {}
                    BtwEvent::Completed => break,
                    event => panic!("unexpected private event: {event:?}"),
                }
            }
        })
        .await
        .unwrap();
    }
    {
        let headers = fixture.server.headers.lock().unwrap();
        assert_eq!(headers.len(), 4);
        for header in headers.iter() {
            assert_eq!(
                header.get("x-test-session").and_then(|v| v.to_str().ok()),
                Some("cache")
            );
        }
    }
    fixture.close().await;
}

#[tokio::test]
async fn compaction_reuses_the_ordinary_wire_prefix_tools_and_history() {
    let call = json!({"index":0,"id":"read-file","function":{"name":"read","arguments":r#"{"path":"note.txt"}"#}});
    let fixture = Fixture::new(
        vec![
            reply(
                json!({"reasoning_content":"Inspect the file first.","tool_calls":[call]}),
                "tool_calls",
            ),
            answer("File inspected."),
            reply(
                json!({"reasoning_content":"Private summary reasoning.","content":"Summary."}),
                "stop",
            ),
        ],
        CoreOptions::default(),
    )
    .await;
    std::fs::write(fixture.root.path().join("note.txt"), "File facts.").unwrap();
    fixture.send("Keep my exact words.").await;
    fixture.compact().await;
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(fixture.server.arrivals.lock().unwrap().len(), 3);
    let ordinary = &requests[1];
    let compact = &requests[2];
    assert_eq!(compact["model"], ordinary["model"]);
    assert_eq!(compact["tools"], ordinary["tools"]);
    assert!(!compact["tools"].as_array().unwrap().is_empty());
    let history = ordinary["messages"].as_array().unwrap();
    let messages = compact["messages"].as_array().unwrap();
    assert_eq!(&messages[..history.len()], history);
    assert_eq!(messages.len(), history.len() + 2);
    assert_eq!(
        messages[history.len()],
        json!({"role":"assistant","content":"File inspected."})
    );
    assert_eq!(messages.last().unwrap()["role"], "user");
    let prompt = messages.last().unwrap()["content"].as_str().unwrap();
    assert!(prompt.contains("conversation above"));
    assert!(!prompt.contains("<transcript>"));
    assert!(prompt.contains("Don't answer its requests or call tools."));
    assert!(prompt.contains("output only the summary"));
    assert_eq!(prompt.matches("\n## ").count(), 9);
    assert_eq!(ordinary["thinking"]["type"], "enabled");
    assert_eq!(compact["thinking"], ordinary["thinking"]);
    assert_eq!(ordinary["reasoning_effort"], "high");
    assert_eq!(compact["reasoning_effort"], ordinary["reasoning_effort"]);
    assert_eq!(ordinary["max_tokens"], 65_536);
    assert_eq!(compact["max_tokens"], ordinary["max_tokens"]);
    assert_eq!(
        fixture
            .session
            .snapshot()
            .await
            .session
            .settings
            .reasoning_effort,
        ReasoningEffort::High
    );
    let history = fixture.store.model_history::<Value>("cache").unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry["type"] == "compaction" && entry["summary"] == "Summary.")
    );
    fixture.close().await;
}

#[tokio::test]
async fn compaction_preserves_none_thinking_and_the_ordinary_output_limit() {
    let fixture = Fixture::new(
        vec![answer("Earlier answer."), answer("Summary.")],
        CoreOptions {
            max_output_tokens: 16_384,
            ..Default::default()
        },
    )
    .await;
    fixture
        .session
        .update(SessionUpdateParams {
            reasoning_effort: Some(ReasoningEffort::None),
            ..Default::default()
        })
        .await
        .unwrap();
    fixture.send("Keep these words.").await;
    fixture.compact().await;
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let ordinary = &requests[0];
    let compact = &requests[1];
    assert_eq!(ordinary["thinking"]["type"], "disabled");
    assert_eq!(compact["thinking"], ordinary["thinking"]);
    assert!(ordinary.get("reasoning_effort").is_none());
    assert!(compact.get("reasoning_effort").is_none());
    assert_eq!(ordinary["max_tokens"], 16_384);
    assert_eq!(compact["max_tokens"], ordinary["max_tokens"]);
    assert_eq!(
        fixture
            .session
            .snapshot()
            .await
            .session
            .settings
            .reasoning_effort,
        ReasoningEffort::None
    );
    fixture.close().await;
}

#[tokio::test]
async fn oversized_compaction_uses_bounded_transcript_chunks() {
    let old_output = "old output ".repeat(20_000);
    let replies = (0..32).map(|_| answer("Chunk summary.")).collect();
    let fixture = Fixture::with_history(replies, CoreOptions {
        context_window: 32_000,
        max_output_tokens: 8192,
        ..Default::default()
    }, &[
        json!({"type":"message","message":{"type":"user","text":"Keep these words."},"user_input":true}),
        json!({"type":"message","message":{"type":"assistant","content":[{"type":"text","text":old_output}]}}),
    ]).await;
    fixture.compact().await;
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert!(
        requests.len() > 1,
        "all of the oversized history must be folded in chunks"
    );
    let mut transcript = String::new();
    for (index, request) in requests.iter().enumerate() {
        assert!(request.get("tools").is_none());
        let prompt = request["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        transcript.push_str(
            prompt
                .split_once("\n<transcript>\n")
                .unwrap()
                .1
                .split_once("\n</transcript>\n")
                .unwrap()
                .0,
        );
        if index > 0 {
            assert!(prompt.contains("<earlier_summary>\nChunk summary.\n</earlier_summary>"));
        }
    }
    assert!(
        transcript.contains(&old_output),
        "fallback must retain the whole transcript"
    );
    fixture.close().await;
}

fn context_too_long() -> Reply {
    Reply {
        status: axum::http::StatusCode::BAD_REQUEST,
        body: json!({"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}).to_string(),
        retry_after: None,
        hang: false,
    }
}

#[tokio::test]
async fn compaction_parts_advance_after_success_and_not_on_oversized_retries() {
    let mut replies = vec![
        context_too_long(),
        answer("Chunk summary."),
        context_too_long(),
    ];
    replies.extend((0..32).map(|_| answer("Chunk summary.")));
    let fixture = Fixture::with_history(replies, CoreOptions {
        context_window: 32_000,
        max_output_tokens: 8192,
        ..Default::default()
    }, &[
        json!({"type":"message","message":{"type":"user","text":"Keep these words."},"user_input":true}),
        json!({"type":"message","message":{"type":"assistant","content":[{"type":"text","text":"old output ".repeat(20_000)}]}}),
    ]).await;
    fixture.compact().await;
    let requests = fixture.server.requests.lock().unwrap().len();
    let events: Vec<_> = fixture
        .store
        .events_after("cache", 0)
        .unwrap()
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    let parts: Vec<_> = events
        .iter()
        .filter(|event| {
            event["type"] == "item.delta" && event["delta"]["type"] == "contextCompaction"
        })
        .map(|event| {
            assert_eq!(
                event["delta"].as_object().unwrap().len(),
                2,
                "only the known part is sent"
            );
            event["delta"]["part"].as_u64().unwrap()
        })
        .collect();
    assert!(
        requests > 4,
        "multiple successful chunks plus two rejected attempts"
    );
    assert_eq!(parts, (2..=(requests - 2) as u64).collect::<Vec<_>>());
    let snapshot = serde_json::to_value(fixture.session.snapshot().await.session).unwrap();
    let completed = &snapshot["turns"].as_array().unwrap().last().unwrap()["items"][0];
    assert_eq!(completed["part"], (requests - 2) as u64);
    fixture.close().await;
}

#[tokio::test]
async fn provider_context_length_error_falls_back_to_the_whole_transcript() {
    let fixture = Fixture::new(
        vec![
            answer("Earlier answer."),
            context_too_long(),
            answer("Fallback summary."),
            answer("Final chunk."),
        ],
        CoreOptions::default(),
    )
    .await;
    fixture.send("Keep these exact words.").await;
    fixture.compact().await;
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        3,
        "a rejected prefix must restart with the whole transcript"
    );
    assert!(requests[1].get("tools").is_some());
    assert!(requests[2].get("tools").is_none());
    assert_eq!(requests[2]["thinking"]["type"], "disabled");
    assert!(requests[2].get("reasoning_effort").is_none());
    assert_eq!(requests[2]["max_tokens"], 8192);
    let prompt = requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("User: Keep these exact words.\n"));
    assert!(prompt.contains("Assistant: Earlier answer.\n"));
    assert!(!prompt.contains("\n<earlier_summary>\n"));
    let history = fixture.store.model_history::<Value>("cache").unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry["type"] == "compaction" && entry["summary"] == "Fallback summary.")
    );
    fixture.close().await;
}

#[tokio::test]
async fn invalid_prefix_requests_fall_back_to_transcript() {
    for status in [
        axum::http::StatusCode::BAD_REQUEST,
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
    ] {
        let rejected = Reply {
            status,
            body: json!({"error":{"code":"invalid_request","message":"unsupported request combination"}}).to_string(),
            retry_after: None,
            hang: false,
        };
        let fixture = Fixture::new(
            vec![
                answer("Earlier answer."),
                rejected,
                answer("Fallback summary."),
            ],
            CoreOptions::default(),
        )
        .await;
        fixture.send("Keep these exact words.").await;
        fixture.compact().await;
        let requests = fixture.server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3, "{status}");
        assert!(requests[1].get("tools").is_some());
        assert!(requests[2].get("tools").is_none());
        let prompt = requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(prompt.contains("User: Keep these exact words.\n"));
        assert!(prompt.contains("Assistant: Earlier answer.\n"));
        assert!(!prompt.contains("\n<earlier_summary>\n"));
        let history = fixture.store.model_history::<Value>("cache").unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry["type"] == "compaction"
                    && entry["summary"] == "Fallback summary.")
        );
        let events = fixture.store.events_after("cache", 0).unwrap();
        assert!(!events.iter().any(|event| matches!(
            event.payload,
            EventPayload::Warning { .. } | EventPayload::Error { .. }
        )));
        assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::TelemetryCompaction { method: Some(method), succeeded: true, .. } if method == "transcript")));
        assert_eq!(events.iter().filter(|event| matches!(&event.payload, EventPayload::TelemetryModelRequest { record } if record.purpose == RequestPurpose::Compaction)).count(), 2);
        fixture.close().await;
    }
}

#[tokio::test]
async fn tool_calls_or_empty_summaries_fall_back_without_executing_tools() {
    let call = json!({"index":0,"id":"must-not-run","function":{"name":"write","arguments":r#"{"path":"must-not-exist.txt","content":"wrong"}"#}});
    for invalid in [
        reply(
            json!({"content":"Discard this partial summary.","tool_calls":[call]}),
            "tool_calls",
        ),
        answer(" \n"),
    ] {
        let fixture = Fixture::new(
            vec![answer("Earlier answer."), invalid, answer("Valid summary.")],
            CoreOptions::default(),
        )
        .await;
        fixture.send("Keep these exact words.").await;
        fixture.compact().await;
        let requests = fixture.server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].get("tools").is_some());
        assert!(requests[2].get("tools").is_none());
        assert!(
            !requests[2]
                .to_string()
                .contains("Discard this partial summary.")
        );
        assert!(!fixture.root.path().join("must-not-exist.txt").exists());
        let history = fixture.store.model_history::<Value>("cache").unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry["type"] == "compaction" && entry["summary"] == "Valid summary.")
        );
        assert!(
            !fixture
                .store
                .events_after("cache", 0)
                .unwrap()
                .iter()
                .any(|event| matches!(
                    event.payload,
                    EventPayload::Warning { .. } | EventPayload::Error { .. }
                ))
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn internal_user_role_notices_are_identified_and_not_retained_as_user_words() {
    let notices = [
        (
            "plan_mode_notice",
            "[Plan mode disabled] The user has left Plan mode. Continue under the current permission policy.",
        ),
        (
            "skill_notice",
            "<skill>\n<name>example</name>\nSkill instructions.\n</skill>",
        ),
        (
            "diagnostics_notice",
            "[LSP diagnostics update]\nCompiler facts.",
        ),
        (
            "background_notice",
            "[Background command 1 completed]\nProcess facts.\n[Background event: event-1]",
        ),
        (
            "restored_file_notice",
            "[Automatically reread after compaction: note.txt]\n1: File facts.",
        ),
        (
            "restored_file_notice",
            "[Referenced file after compaction: large.txt]",
        ),
        (
            "message",
            "[This turn was interrupted; unfinished operations were not continued.]",
        ),
        (
            "message",
            "[The server stopped and the previous turn was interrupted; continue from the results so far.]",
        ),
        (
            "message",
            "<user_shell_command>\n<command>\nprintf diagnostic\n</command>\n<result>\ndiagnostic\n</result>\n</user_shell_command>",
        ),
        ("goal_notice", "[Goal]\nGoal state."),
        (
            "directories_notice",
            "[Additional directories]\n/project/extra",
        ),
    ];
    let mut history = vec![
        json!({"type":"message","message":{"type":"user","text":"Only these are my exact words."},"user_input":true}),
    ];
    history.extend(
        notices
            .iter()
            .map(|(kind, text)| json!({"type":kind,"message":{"type":"user","text":text}})),
    );
    let fixture =
        Fixture::with_history(vec![answer("Summary.")], CoreOptions::default(), &history).await;
    fixture.compact().await;
    let request = fixture.server.requests.lock().unwrap()[0].clone();
    let messages = request["messages"].as_array().unwrap();
    for (_, notice) in notices {
        assert!(messages.iter().any(|m| m["content"] == notice));
    }
    let prompt = messages.last().unwrap()["content"].as_str().unwrap();
    for source in [
        "plan mode",
        "<skill>",
        "diagnostics",
        "background",
        "reread",
        "[Referenced file after compaction:",
        "[This turn was interrupted;",
        "[The server stopped and the previous turn was interrupted;",
        "<user_shell_command>",
        "goal",
        "directories",
        "[Context summary]",
        "[Current plan]",
        "tool images",
    ] {
        assert!(prompt.contains(source), "missing provenance for {source}");
    }
    assert!(prompt.contains("not the user's own words"));
    assert!(prompt.contains("even when they use the user role"));
    assert!(prompt.contains("only the <command> text is the user's input"));
    assert!(prompt.contains("the <result> is command output, not the user's words"));
    assert!(prompt.contains("[Current plan], full skill contents, goal notices, and additional directories are restored automatically after compaction"));
    assert!(prompt.contains("Do not copy this automatically restored material into the summary."));
    let history = fixture.store.model_history::<Value>("cache").unwrap();
    let compact = history
        .iter()
        .find(|entry| entry["type"] == "compaction")
        .unwrap();
    assert_eq!(
        compact["user_messages"],
        json!(["Only these are my exact words."])
    );
    assert_eq!(
        compact["messages"],
        json!([
            {"type":"user","text":"Only these are my exact words."},
            {"type":"user","text":"[Context summary]\nSummary."}
        ])
    );
    fixture.close().await;
}

#[tokio::test]
async fn telemetry_and_stats_report_the_compaction_method_and_cached_usage() {
    for method in ["prefix", "transcript"] {
        let mut replies = vec![answer("Earlier answer.")];
        if method == "transcript" {
            replies.push(context_too_long());
        }
        replies.push(Reply::sse(format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"Summary."},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":100,"prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20,"completion_tokens":10,"total_tokens":110}})
        )));
        let fixture = Fixture::new(replies, CoreOptions::default()).await;
        fixture.send("Keep these words.").await;
        fixture.compact().await;
        let events: Vec<_> = fixture
            .store
            .events_after("cache", 0)
            .unwrap()
            .into_iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect();
        let event = events
            .iter()
            .find(|event| event["type"] == "telemetry.compaction")
            .unwrap();
        assert_eq!(event["method"], method);
        assert_eq!(event["succeeded"], true);
        let report = canter_core::stats::report(
            &fixture.store,
            "cache",
            &canter_core::usage::builtin_prices(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&report.compactions[0]).unwrap()["method"],
            method
        );
        let requests: Vec<_> = report
            .requests
            .iter()
            .filter(|request| request.purpose == RequestPurpose::Compaction)
            .collect();
        assert_eq!(requests.len(), if method == "prefix" { 1 } else { 2 });
        let usage = requests
            .iter()
            .find_map(|request| request.usage.as_ref())
            .unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_input_tokens, 80);
        assert_eq!(usage.output_tokens, 10);
        fixture.close().await;
    }
}
