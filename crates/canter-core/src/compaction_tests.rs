#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use crate::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::{EventPayload, TurnStatus};
use canter_store::Store;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn reply(delta: Value, reason: &str, input_tokens: u64) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]}),
        json!({"choices":[],"usage":{"prompt_tokens":input_tokens,"completion_tokens":1,"total_tokens":input_tokens+1}}),
    ))
}

async fn completed(events: &mut Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventPayload::TurnCompleted { turn } =
                events.recv().await.unwrap().unwrap().payload
            {
                assert_eq!(turn.status, TurnStatus::Completed);
                return;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn manual_compaction_neither_counts_nor_resets_prior_auto_compactions() {
    for count in [1_u8, 2] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("note.txt"), "File facts.\n").unwrap();
        let mut replies = Vec::new();
        for index in 0..count {
            replies.push(reply(
                json!({"tool_calls":[{"index":0,"id":format!("read-{index}"),"function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}}]}),
                "tool_calls",
                41_000,
            ));
            replies.push(reply(json!({"content":"Automatic summary."}), "stop", 100));
        }
        replies.push(reply(json!({"content":"Done."}), "stop", 100));
        replies.push(reply(json!({"content":"Manual summary."}), "stop", 100));
        let server = Server::start(replies).await;
        let (session, _) = Session::create(
            Arc::new(Store::new(root.path().join("sessions")).unwrap()),
            "spin".into(),
            serde_json::from_value(json!({
                "cwd":root.path(), "model":"fake", "approvalPolicy":"allowAll"
            }))
            .unwrap(),
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
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
            CoreOptions {
                auto_compact_tokens: Some(40_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session
            .input(
                serde_json::from_value(json!({
                    "input":[{"type":"text","text":"Inspect."}],
                    "mode":"queue", "clientName":"test"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        completed(&mut events).await;
        assert_eq!(
            session
                .shared
                .state
                .lock()
                .await
                .auto_compactions_without_progress,
            count
        );
        assert_eq!(
            server.requests.lock().unwrap().len(),
            usize::from(count) * 2 + 1
        );

        // No user input or goal instruction may reset the count before this assertion.
        session.compact().await.unwrap();
        completed(&mut events).await;
        assert_eq!(
            session
                .shared
                .state
                .lock()
                .await
                .auto_compactions_without_progress,
            count,
            "manual compaction must preserve {count} prior automatic compactions"
        );
        assert_eq!(
            server.arrivals.lock().unwrap().len(),
            usize::from(count) * 2 + 2
        );
        session.close().await.unwrap();
        server.stop().await;
    }
}
