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
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
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
