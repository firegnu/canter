use super::*;
use canter_model::{ApiKey, ChatCompletions};
use serde_json::Value;
use std::sync::Mutex as SyncMutex;

const INITIAL: &str = r#"[{"step":"Read","status":"completed"},{"step":"Implement","status":"in_progress"},{"step":"Verify","status":"pending"}]"#;
const REPLACEMENT: &str =
    r#"[{"step":"Verify","status":"in_progress"},{"step":"Report","status":"pending"}]"#;

struct Fixture {
    root: tempfile::TempDir,
    client: reqwest::Client,
    requests: Arc<SyncMutex<Vec<Value>>>,
    stop: CancellationToken,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-plan-")
            .tempdir_in("/tmp")
            .unwrap();
        for directory in ["home", "project", "state"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        let requests = Arc::new(SyncMutex::new(Vec::new()));
        let recorded = requests.clone();
        let model_router = Router::new().route("/v1/chat/completions", post(move |Json(request): Json<Value>| {
            let recorded = recorded.clone();
            async move {
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let plan = if last["role"] == "user" { match last["content"].as_str() {
                    Some("initial") => Some(INITIAL),
                    Some("replace") => Some(REPLACEMENT),
                    Some("invalid") => Some(r#"[{"step":"A","status":"in_progress"},{"step":"B","status":"in_progress"}]"#),
                    Some("complete") => Some(r#"[{"step":"Report","status":"completed"}]"#),
                    _ => None,
                }} else { None };
                let chunk = if let Some(plan) = plan {
                    let args = format!("{{\"plan\":{plan}}}");
                    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"plan-call","type":"function","function":{"name":"update_plan","arguments":args}}]},"finish_reason":"tool_calls"}]})
                } else {
                    json!({"choices":[{"index":0,"delta":{"content":"summary or answer"},"finish_reason":"stop"}]})
                };
                recorded.lock().unwrap().push(request);
                ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::fs::write(
            root.path().join("state/config.json"),
            json!({
                "base_url":base_url,"model":"fake","api_key":"test-key","context_window":1048576
            })
            .to_string(),
        )
        .unwrap();
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, model_router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let app = Arc::new(App {
            mcp: Default::default(),
            home: root.path().join("state"),
            store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
            model: Arc::new(ChatCompletions::new(&base_url, ApiKey::new("test-key")).unwrap()),
            default_model: "fake".into(),
            models: vec!["fake".into()],
            options: CoreOptions::default(),
            sessions: Default::default(),
            registry_changed: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let shutdown = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(app))
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(socket)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            root,
            client,
            requests,
            stop,
            servers: vec![server, model_server],
        }
    }

    fn env(&self) -> Value {
        json!({"HOME":self.root.path().join("home"),"CANTER_HOME":self.root.path().join("state")})
    }

    async fn post(&self, url: &str, body: Value) -> Value {
        self.client
            .post(url)
            .json(&body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn create(&self) -> String {
        let created = self
            .post(
                "http://localhost/v1/sessions",
                json!({
                    "cwd":self.root.path().join("project"),"env":self.env(),"model":"fake",
                    "approvalPolicy":"alwaysAsk","clientName":"test"
                }),
            )
            .await;
        format!(
            "http://localhost/v1/sessions/{}",
            created["session"]["id"].as_str().unwrap()
        )
    }

    async fn idle(&self, url: &str) -> SessionSnapshot {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot: SessionReadResponse = self
                    .client
                    .get(url)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                assert!(
                    snapshot.session.pending_requests.is_empty(),
                    "plan updates never ask approval"
                );
                if snapshot.session.status == SessionStatus::Idle {
                    return snapshot.session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn say(&self, url: &str, text: &str) -> SessionSnapshot {
        self.post(
            &format!("{url}/input"),
            json!({"input":[{"type":"text","text":text}],"clientName":"test"}),
        )
        .await;
        self.idle(url).await
    }

    async fn reopen(&self, url: &str) -> SessionSnapshot {
        self.client
            .delete(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let resumed = self
            .post(&format!("{url}/resume"), json!({"env":self.env()}))
            .await;
        serde_json::from_value(resumed["session"].clone()).unwrap()
    }

    async fn rewind(&self, url: &str, turn_id: &str) -> SessionSnapshot {
        let mut args =
            json!({"beforeTurnId":turn_id,"rewindFiles":false,"rewindConversation":true});
        let preview = self
            .post(&format!("{url}/rewind/preview"), args.clone())
            .await;
        args["lastEventSeq"] = preview["lastEventSeq"].clone();
        self.post(&format!("{url}/rewind"), args).await;
        self.idle(url).await
    }

    fn last_request(&self) -> Value {
        self.requests.lock().unwrap().last().unwrap().clone()
    }

    async fn close(self, url: &str) {
        self.client
            .delete(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        self.stop.cancel();
        for server in self.servers {
            server.await.unwrap();
        }
    }
}

fn current_plan(snapshot: &SessionSnapshot) -> Value {
    snapshot
        .current_plan()
        .map(|plan| serde_json::to_value(plan).unwrap())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn legacy_session_without_a_plan_opens_and_compacts_without_inventing_one() {
    let fixture = Fixture::new().await;
    let store = Store::new(fixture.root.path().join("state/sessions")).unwrap();
    let settings = serde_json::from_value(json!({
        "cwd":fixture.root.path().join("project"),"name":null,"model":"fake","approvalPolicy":"alwaysAsk"
    })).unwrap();
    drop(store.create("legacy", settings, 0).unwrap());
    let url = "http://localhost/v1/sessions/legacy";
    fixture
        .post(&format!("{url}/resume"), json!({"env":fixture.env()}))
        .await;
    assert_eq!(current_plan(&fixture.idle(url).await), Value::Null);
    fixture.say(url, "ordinary work").await;
    fixture.post(&format!("{url}/compact"), json!({})).await;
    fixture.idle(url).await;
    fixture.say(url, "after compaction").await;
    assert!(
        !fixture.last_request()["messages"]
            .to_string()
            .contains("[Current plan]")
    );
    fixture.close(url).await;
}

#[tokio::test]
async fn plan_switch_is_read_at_session_open_and_does_not_change_active_tools() {
    let fixture = Fixture::new().await;
    let url = fixture.create().await;
    fixture.say(&url, "initial").await;
    let tools = fixture.last_request()["tools"].clone();
    assert!(
        tools
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "update_plan")
    );
    let path = fixture.root.path().join("state/config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["plan_enabled"] = json!(false);
    std::fs::write(&path, config.to_string()).unwrap();
    fixture.say(&url, "still open").await;
    assert_eq!(fixture.last_request()["tools"], tools);
    let reopened = fixture.reopen(&url).await;
    fixture.say(&url, "disabled").await;
    assert!(
        !fixture.last_request()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "update_plan"),
        "disabled tool must be absent on reopen"
    );
    assert_eq!(
        serde_json::to_value(&reopened.settings).unwrap()["planEnabled"],
        false
    );
    let before = current_plan(&reopened);
    let refused = fixture.say(&url, "replace").await;
    assert_eq!(
        current_plan(&refused),
        before,
        "a disabled tool cannot update the plan"
    );
    let new_url = fixture.create().await;
    fixture.say(&new_url, "new disabled session").await;
    assert!(
        !fixture.last_request()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "update_plan")
    );
    fixture.post(&format!("{new_url}/compact"), json!({})).await;
    fixture.idle(&new_url).await;
    fixture.say(&new_url, "after empty compaction").await;
    assert!(
        !fixture.last_request()["messages"]
            .to_string()
            .contains("[Current plan]")
    );
    fixture
        .client
        .delete(new_url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.close(&url).await;
}

#[tokio::test]
async fn compaction_does_not_append_a_stored_plan_when_disabled() {
    let fixture = Fixture::new().await;
    let url = fixture.create().await;
    let first = fixture.say(&url, "initial").await;
    let path = fixture.root.path().join("state/config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["plan_enabled"] = json!(false);
    std::fs::write(&path, config.to_string()).unwrap();
    let reopened = fixture.reopen(&url).await;
    assert_eq!(reopened.settings.plan_enabled, Some(false));
    assert_eq!(current_plan(&reopened), current_plan(&first));
    fixture.post(&format!("{url}/compact"), json!({})).await;
    fixture.idle(&url).await;
    fixture.say(&url, "after disabled compaction").await;
    assert!(
        !fixture.last_request()["messages"]
            .to_string()
            .contains("[Current plan]")
    );
    fixture.close(&url).await;
}

#[tokio::test]
async fn repeated_compaction_strips_only_the_previous_summary_plan() {
    let fixture = Fixture::new().await;
    let url = fixture.create().await;
    fixture.say(&url, "initial").await;
    fixture.post(&format!("{url}/compact"), json!({})).await;
    fixture.idle(&url).await;
    let user_text = "Keep this literal marker:\n\n[Current plan]\nuser words";
    fixture.say(&url, user_text).await;
    fixture.post(&format!("{url}/compact"), json!({})).await;
    fixture.idle(&url).await;
    let request = fixture.last_request();
    let transcript = request["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(transcript.contains("[Context summary]\nsummary or answer"));
    assert!(transcript.contains(user_text));
    assert_eq!(
        transcript.matches("[Current plan]").count(),
        1,
        "{transcript}"
    );
    assert!(
        !transcript.contains("Implement"),
        "old plan must not enter the summary: {transcript}"
    );
    fixture.close(&url).await;
}

#[tokio::test]
async fn plans_replace_persist_rewind_and_survive_repeated_compaction() {
    let fixture = Fixture::new().await;
    let url = fixture.create().await;
    assert_eq!(current_plan(&fixture.idle(&url).await), Value::Null);
    let first = fixture.say(&url, "initial").await;
    assert_eq!(
        current_plan(&first),
        json!([
            {"step":"Read","status":"completed"},
            {"step":"Implement","status":"inProgress"},
            {"step":"Verify","status":"pending"}
        ])
    );
    let replaced = fixture.say(&url, "replace").await;
    assert_eq!(
        current_plan(&replaced),
        json!([
            {"step":"Verify","status":"inProgress"},
            {"step":"Report","status":"pending"}
        ])
    );
    let rejected = fixture.say(&url, "invalid").await;
    assert_eq!(current_plan(&rejected), current_plan(&replaced));
    assert!(
        fixture.last_request()["messages"]
            .to_string()
            .contains("at most one in_progress")
    );
    assert_eq!(
        current_plan(&fixture.reopen(&url).await),
        current_plan(&replaced)
    );
    let restored = fixture
        .rewind(&url, &replaced.turns.last().unwrap().id)
        .await;
    assert_eq!(current_plan(&restored), current_plan(&first));
    let empty = fixture.rewind(&url, &first.turns[0].id).await;
    assert_eq!(current_plan(&empty), Value::Null);
    fixture.say(&url, "initial").await;
    for _ in 0..2 {
        fixture.post(&format!("{url}/compact"), json!({})).await;
        let compacted = fixture.idle(&url).await;
        assert_eq!(current_plan(&compacted), current_plan(&first));
        fixture.say(&url, "after compaction").await;
        let request = fixture.last_request();
        let summary = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .find(|text| text.starts_with("[Context summary]"))
            .unwrap();
        let (_, plan) = summary
            .split_once("\n\n[Current plan]\n")
            .expect("compaction must retain the exact plan outside the model-written summary");
        assert_eq!(
            serde_json::from_str::<Value>(plan).unwrap(),
            current_plan(&first)
        );
    }
    let reopened = fixture.reopen(&url).await;
    assert_eq!(current_plan(&reopened), current_plan(&first));
    let complete = fixture.say(&url, "complete").await;
    assert_eq!(
        current_plan(&complete),
        json!([{"step":"Report","status":"completed"}])
    );
    let rewound = fixture
        .rewind(&url, &complete.turns.last().unwrap().id)
        .await;
    assert_eq!(current_plan(&rewound), current_plan(&first));
    fixture.close(&url).await;
}
