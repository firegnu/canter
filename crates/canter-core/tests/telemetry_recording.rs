#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use support::{KEY, Reply, Server};

fn legacy_settings(root: &Path) -> SessionSettings {
    serde_json::from_value(
        json!({"cwd":root, "name":null, "model":"fake", "approvalPolicy":"allowAll"}),
    )
    .unwrap()
}

fn seed(store: &Store, root: &Path, id: &str, messages: usize) {
    let mut writer = store.create(id, legacy_settings(root), 1).unwrap();
    for index in 0..messages {
        writer.append_model_history(&json!({
            "type":"message", "message":{"type":"user", "text":format!("history {index}: {}", "fixture ".repeat(512))}, "user_input":true
        })).unwrap();
    }
}

fn runtime(root: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            ("HOME".into(), root.join("home").display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("state").display().to_string(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}

async fn turn(session: &Session) {
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session
        .input(SessionInputParams {
            input: vec![UserInput::Text {
                text: "continue".into(),
            }],
            mode: InputMode::Queue,
            client_name: "test".into(),
            review: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.recv().await.unwrap() {
            if let EventPayload::TurnCompleted { turn } = event.payload {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                return;
            }
        }
        panic!("event stream ended before completion");
    })
    .await
    .unwrap();
}

fn reply() -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":350000,"completion_tokens":1,"total_tokens":350001}})
    ))
}

#[tokio::test]
async fn legacy_session_opens_and_new_events_remain_readable_by_legacy_event_reader() {
    #[derive(serde::Deserialize)]
    #[serde(tag = "type")]
    enum OldEvent {
        #[serde(rename = "session.updated")]
        Settings { settings: Value },
        #[serde(other)]
        Unknown,
    }
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
    seed(&store, root.path(), "old", 3);
    let server = Server::start(vec![reply()]).await;
    let model = Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap());
    let (session, response) = Session::open(
        store.clone(),
        "old".into(),
        runtime(root.path()),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert!(response.session.settings.tags.is_empty());
    turn(&session).await;
    session.close().await.unwrap();
    let events = store.events_after("old", 0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::TelemetryModelRequest { .. }))
    );
    for event in &events {
        if let OldEvent::Settings { settings } =
            serde_json::from_value::<OldEvent>(serde_json::to_value(event).unwrap()).unwrap()
        {
            assert_eq!(settings["model"], "fake");
        }
    }
    {
        let requests = server.requests.lock().unwrap();
        assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("history 0:"))
        }));
    }
    server.stop().await;
}

/// Manual comparison, not a wall-clock assertion on a shared development machine.
#[tokio::test]
#[ignore = "run explicitly for the telemetry on/off long-history comparison"]
async fn long_history_on_off_comparison() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
    let server = Server::start((0..22).map(|_| reply()).collect()).await;
    let model = Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap());
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..11 {
        for enabled in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let id = format!("sample-{round}-{enabled}");
            seed(&store, root.path(), &id, 256);
            let (session, _) = Session::open(
                store.clone(),
                id,
                runtime(root.path()),
                model.clone(),
                CoreOptions {
                    telemetry: enabled,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let started = Instant::now();
            turn(&session).await;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            if round > 0 {
                samples[usize::from(enabled)].push(elapsed);
            }
            session.close().await.unwrap();
        }
    }
    let request_bytes = {
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 22);
        assert_eq!(server.arrivals.lock().unwrap().len(), requests.len());
        for request in &requests[1..] {
            assert_eq!(
                &requests[0], request,
                "on/off must send exactly the same request"
            );
        }
        serde_json::to_vec(&requests[0]).unwrap().len()
    };
    for (enabled, samples) in samples.iter_mut().enumerate() {
        samples.sort_by(f64::total_cmp);
        println!(
            "telemetry={} messages=256 request_bytes={} runs={} median_ms={:.3} mean_ms={:.3} min_ms={:.3} max_ms={:.3}",
            enabled == 1,
            request_bytes,
            samples.len(),
            (samples[4] + samples[5]) / 2.0,
            samples.iter().sum::<f64>() / samples.len() as f64,
            samples[0],
            samples[9]
        );
    }
    server.stop().await;
}
