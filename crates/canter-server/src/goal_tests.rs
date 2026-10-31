use super::*;
use canter_model::{ApiKey, ChatCompletions};
use serde_json::Value;
use std::sync::Mutex as SyncMutex;

struct Fixture {
    root: tempfile::TempDir,
    app: Arc<App>,
    client: reqwest::Client,
    requests: Arc<SyncMutex<Vec<Value>>>,
    replies: Arc<SyncMutex<VecDeque<Value>>>,
    ready: Arc<Notify>,
    stop: CancellationToken,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

#[tokio::test]
async fn non_interactive_input_is_atomic_and_permissions_expire_with_the_turn() {
    let fixture = Fixture::new(json!({"sandbox":false})).await;
    let url = fixture.create().await;
    fixture.client.post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"Unattended task"}],"nonInteractive":true,"approvalPolicy":"askForExecution","clientName":"exec"}))
        .send().await.unwrap().error_for_status().unwrap();
    fixture.request_count(1).await;
    // Another client's settings request cannot change the accepted turn's scope.
    fixture
        .client
        .patch(&url)
        .json(&json!({"nonInteractive":false,"approvalPolicy":"allowAll"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let question = call(
        "ask_user",
        json!({"question":"Which way?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}),
    );
    fixture.respond(vec![
        question.clone(),
        call(
            "shell",
            json!({"command":"printf unattended > unattended.txt"}),
        ),
        reply("Unattended done."),
    ]);
    let snapshot = fixture.until(&url, |s| s["status"] == "idle").await;
    assert!(snapshot["settings"].get("nonInteractive").is_none());
    assert_eq!(snapshot["settings"]["approvalPolicy"], "allowAll");
    assert!(!fixture.root.path().join("project/unattended.txt").exists());
    {
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests[1]["messages"]
                .to_string()
                .contains("No one is available.")
        );
        assert!(
            requests[2]["messages"]
                .to_string()
                .contains("No one is available to approve")
        );
    }
    fixture.respond(vec![
        question,
        call(
            "shell",
            json!({"command":"printf interactive > interactive.txt"}),
        ),
        reply("Interactive done."),
    ]);
    fixture.input(&url, "Ordinary next input").await;
    let snapshot = fixture
        .until(&url, |s| {
            s["pendingRequests"]
                .as_array()
                .is_some_and(|r| !r.is_empty())
        })
        .await;
    fixture
        .resolve(
            &url,
            &snapshot["pendingRequests"][0],
            json!({"answer":{"type":"text","text":"Choose A"},"clientName":"terminal"}),
        )
        .await;
    fixture.until(&url, |s| s["status"] == "idle").await;
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("project/interactive.txt")).unwrap(),
        "interactive"
    );
    fixture.close().await;
}

#[tokio::test]
async fn non_interactive_goal_runs_one_turn_and_keeps_budget_then_interactive_continues() {
    let fixture = Fixture::new(
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
    )
    .await;
    let url = fixture.create().await;
    fixture
        .client
        .post(format!("{url}/input"))
        .json(&json!({"nonInteractive":true,"input":[{"type":"text","text":"First step"}],"clientName":"exec"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .set(
            &url,
            json!({"objective":"Finish both steps","budgetUsd":10}),
        )
        .await;
    let queued = fixture.client.post(format!("{url}/input")).json(&json!({"input":[{"type":"text","text":"A competing turn"}],"clientName":"other-client"})).send().await.unwrap();
    assert_eq!(
        queued.status(),
        StatusCode::CONFLICT,
        "non-interactive input must not queue another turn"
    );
    fixture.respond(vec![charged(reply("First step."), 1)]);
    fixture
        .until(&url, |s| s["turns"][0]["status"] == "completed")
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let snapshot = fixture.snapshot(&url).await;
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["goal"]["status"], "active");
    assert_eq!(snapshot["goal"]["cost"]["usd"], 1.0);
    fixture.respond(vec![
        reply("Second step."),
        call("update_goal", json!({"status":"complete"})),
        reply("Done."),
    ]);
    fixture.input(&url, "Continue interactively.").await;
    let snapshot = fixture
        .until(&url, |s| {
            s["goal"]["status"] == "complete" && s["status"] == "idle"
        })
        .await;
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 3);
    fixture.close().await;
}

fn reply(text: &str) -> Value {
    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
}

fn call(name: &str, arguments: Value) -> Value {
    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"goal-call","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]})
}

fn charged(mut response: Value, output: u64) -> Value {
    response["usage"] =
        json!({"prompt_tokens":10,"completion_tokens":output,"total_tokens":10+output});
    response
}

impl Fixture {
    async fn new(extra: Value) -> Self {
        Self::with_question_timeout(extra, Duration::from_secs(300)).await
    }

    async fn with_question_timeout(extra: Value, timeout: Duration) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-goal-")
            .tempdir_in("/tmp")
            .unwrap();
        for name in ["home", "state", "project"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let mut config = json!({"base_url":"http://127.0.0.1:1/v1","model":"fake","api_key":"test-key","context_window":1048576});
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        std::fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
        let mut options = crate::config::Config::load(&root.path().join("state"))
            .unwrap()
            .options();
        options.goal_question_timeout = timeout;
        let requests = Arc::new(SyncMutex::new(Vec::new()));
        let replies = Arc::new(SyncMutex::new(VecDeque::<Value>::new()));
        let ready = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let model_router = {
            let (requests, replies, ready, stop) = (
                requests.clone(),
                replies.clone(),
                ready.clone(),
                stop.clone(),
            );
            Router::new().route("/v1/chat/completions", post(move |Json(request): Json<Value>| {
                let (requests, replies, ready, stop) = (requests.clone(), replies.clone(), ready.clone(), stop.clone());
                async move {
                    requests.lock().unwrap().push(request);
                    let body = Body::from_stream(futures_util::stream::once(async move {
                        let response = loop {
                            let notified = ready.notified();
                            if let Some(response) = replies.lock().unwrap().pop_front() { break response; }
                            tokio::select! { _ = notified => {}, _ = stop.cancelled() => break reply("stopped") }
                        };
                        Ok::<_, Infallible>(format!("data: {response}\n\ndata: [DONE]\n\n"))
                    }));
                    ([("content-type", "text/event-stream")], body)
                }
            }))
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model = ChatCompletions::new(
            &format!("http://{}/v1", listener.local_addr().unwrap()),
            ApiKey::new("test-key"),
        )
        .unwrap();
        let shutdown = stop.clone();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, model_router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let app = Arc::new(App {
            home: root.path().join("state"),
            store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
            model: Arc::new(model),
            default_model: "fake".into(),
            models: vec!["fake".into()],
            options,
            mcp: Default::default(),
            lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
            sessions: Default::default(),
            registry_changed: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let (server_app, shutdown) = (app.clone(), stop.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router(server_app))
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
            app,
            client,
            requests,
            replies,
            ready,
            stop,
            servers: vec![server, model_server],
        }
    }

    fn respond(&self, responses: Vec<Value>) {
        self.replies.lock().unwrap().extend(responses);
        self.ready.notify_one();
    }

    fn env(&self) -> Value {
        json!({"HOME":self.root.path().join("home"),"CANTER_HOME":self.root.path().join("state"),"PATH":"/usr/bin:/bin"})
    }

    async fn create(&self) -> String {
        let value: Value = self.client.post("http://localhost/v1/sessions").json(&json!({"cwd":self.root.path().join("project"),"env":self.env(),"model":"fake","clientName":"test","approvalPolicy":"allowAll"})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        format!(
            "http://localhost/v1/sessions/{}",
            value["session"]["id"].as_str().unwrap()
        )
    }

    async fn snapshot(&self, url: &str) -> Value {
        self.client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["session"]
            .clone()
    }

    async fn input(&self, url: &str, text: &str) {
        self.client
            .post(format!("{url}/input"))
            .json(&json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    async fn action(&self, url: &str, action: &str) {
        self.client
            .post(format!("{url}/goal/{action}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    async fn request_count(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.requests.lock().unwrap().len() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn set(&self, url: &str, params: Value) -> Value {
        self.client
            .post(format!("{url}/goal"))
            .json(&params)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn resolve(&self, url: &str, request: &Value, answer: Value) {
        self.client
            .post(format!(
                "{url}/requests/{}",
                request["id"].as_str().unwrap()
            ))
            .json(&answer)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    async fn reopen(&self, url: &str) {
        let id = self.snapshot(url).await["id"].as_str().unwrap().to_owned();
        let session = self.app.sessions.lock().await.remove(&id).unwrap();
        session.close_for_shutdown().await.unwrap();
        drop(session);
        self.client
            .post(format!("{url}/resume"))
            .json(&json!({"env":self.env()}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    async fn until(&self, url: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = self.snapshot(url).await;
                if predicate(&value) {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("goal did not reach expected state")
    }

    async fn close(self) {
        for session in self.app.sessions.lock().await.values() {
            session.close_for_shutdown().await.unwrap();
        }
        self.stop.cancel();
        for server in self.servers {
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn goal_starts_immediately_continues_and_model_completes_it() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture.respond(vec![
        reply("first step finished"),
        call("update_goal", json!({"status":"complete"})),
        reply("all done"),
    ]);
    let response = fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Finish both steps"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "setting a goal must start work"
    );
    let snapshot = fixture
        .until(&url, |s| {
            s["goal"]["status"] == "complete" && s["status"] == "idle"
        })
        .await;
    assert_eq!(snapshot["goal"]["budgetUsd"], 5.0);
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 2);
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let messages = requests[1]["messages"].to_string();
    assert!(messages.contains("Finish both steps") && messages.contains("5.00"));
    assert!(requests[1]["tools"].to_string().contains("get_goal"));
    assert!(!requests[1]["tools"].to_string().contains("create_goal"));
    fixture.close().await;
}

#[tokio::test]
async fn goal_budget_at_ninety_percent_summarizes_once_without_running_tools() {
    let fixture = Fixture::new(
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
    )
    .await;
    let url = fixture.create().await;
    fixture.respond(vec![
        charged(
            call(
                "write",
                json!({"path":"must-not-start","content":"new work"}),
            ),
            9,
        ),
        charged(
            call(
                "write",
                json!({"path":"summary-must-not-start","content":"new work"}),
            ),
            1,
        ),
    ]);
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Implement feature","budgetUsd":10}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let snapshot = fixture
        .until(&url, |s| {
            s["goal"]["status"] == "budgetLimited" && s["status"] == "idle"
        })
        .await;
    assert_eq!(snapshot["goal"]["cost"]["usd"], 10.0);
    assert!(!fixture.root.path().join("project/must-not-start").exists());
    assert!(
        !fixture
            .root
            .path()
            .join("project/summary-must-not-start")
            .exists()
    );
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["tools"], requests[0]["tools"]);
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("[Goal budget limit]")
    );
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn goal_pause_requires_explicit_resume_and_clear_allows_replacement() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Keep working"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.request_count(1).await;
    fixture.action(&url, "pause").await;
    let paused = fixture.until(&url, |s| s["status"] == "idle").await;
    assert_eq!(paused["goal"]["status"], "paused");
    let replacement = fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"replacement"}))
        .send()
        .await
        .unwrap();
    assert_eq!(replacement.status(), StatusCode::CONFLICT);
    fixture.respond(vec![reply("ordinary conversation")]);
    fixture.input(&url, "a side question").await;
    fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["turns"].as_array().unwrap().len() == 2
        })
        .await;
    assert_eq!(fixture.snapshot(&url).await["goal"]["status"], "paused");
    fixture.respond(vec![
        call("update_goal", json!({"status":"blocked"})),
        reply("blocked after three turns"),
    ]);
    fixture.action(&url, "resume").await;
    fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["goal"]["status"] == "blocked"
        })
        .await;
    fixture
        .client
        .delete(format!("{url}/goal"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(fixture.snapshot(&url).await.get("goal").is_none());
    fixture.respond(vec![
        call("update_goal", json!({"status":"complete"})),
        reply("done"),
    ]);
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"replacement"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["goal"]["status"] == "complete"
        })
        .await;
    fixture.close().await;
}

#[tokio::test]
async fn goal_question_times_out_without_faking_a_user_answer() {
    let fixture = Fixture::with_question_timeout(json!({}), Duration::from_millis(60)).await;
    let url = fixture.create().await;
    fixture.respond(vec![call("ask_user", json!({"question":"Which approach?","options":[{"label":"A","description":"Use A"},{"label":"B","description":"Use B"}]})), call("update_goal", json!({"status":"complete"})), reply("Chose A while you were away.")]);
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Choose and finish"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let snapshot = fixture
        .until(&url, |s| {
            s["goal"]["status"] == "complete" && s["status"] == "idle"
        })
        .await;
    assert!(snapshot["pendingRequests"].as_array().unwrap().is_empty());
    let requests = fixture.requests.lock().unwrap().clone();
    let tool = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("user is unavailable")
    );
    assert!(
        tool["content"]
            .as_str()
            .unwrap()
            .contains("state which decisions")
    );
    assert!(
        snapshot["turns"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "userInput" && item["answer"].is_null())
    );
    fixture.close().await;
}

#[tokio::test]
async fn goal_notice_survives_compaction_and_conversation_rewind() {
    for operation in ["compact", "rewind"] {
        let fixture = Fixture::new(
            json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
        )
        .await;
        let url = fixture.create().await;
        fixture.respond(vec![charged(reply("seed conversation"), 5)]);
        fixture.input(&url, "seed").await;
        let seed = fixture.until(&url, |s| s["status"] == "idle").await;
        fixture.respond(vec![charged(call("get_goal", json!({})), 2)]);
        fixture
            .client
            .post(format!("{url}/goal"))
            .json(&json!({"objective":"Persistent objective"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fixture.request_count(3).await;
        fixture.action(&url, "pause").await;
        if operation == "compact" {
            fixture.respond(vec![charged(reply("summary with no objective"), 7)]);
            fixture
                .client
                .post(format!("{url}/compact"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            fixture.until(&url, |s| s["status"] == "idle").await;
        } else {
            let mut args = json!({"beforeTurnId":seed["turns"][0]["id"],"rewindConversation":true,"rewindFiles":false});
            let preview: Value = fixture
                .client
                .post(format!("{url}/rewind/preview"))
                .json(&args)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            args["lastEventSeq"] = preview["lastEventSeq"].clone();
            fixture
                .client
                .post(format!("{url}/rewind"))
                .json(&args)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        fixture.respond(vec![reply("ordinary answer")]);
        fixture.input(&url, "check context").await;
        fixture.until(&url, |s| s["status"] == "idle").await;
        let requests = fixture.requests.lock().unwrap().clone();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        assert!(
            messages.iter().any(|m| m["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("[Goal]")
                    && text.contains("Persistent objective")
                    && text.contains("Paused"))),
            "{operation}: {messages:?}"
        );
        assert_eq!(fixture.snapshot(&url).await["goal"]["status"], "paused");
        assert_eq!(
            fixture.snapshot(&url).await["goal"]["cost"]["usd"],
            2.0,
            "{operation} must not undo spending or include paused requests"
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn goal_attached_to_user_turn_survives_retraction() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture.input(&url, "retract this message").await;
    fixture.request_count(1).await;
    fixture
        .set(&url, json!({"objective":"Retained after retract"}))
        .await;
    let started = fixture.snapshot(&url).await;
    let interrupted: Value = fixture
        .client
        .post(format!("{url}/interrupt"))
        .json(&json!({"turnId":started["turns"][0]["id"],"retract":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(interrupted["retracted"], true);
    assert_eq!(fixture.snapshot(&url).await["goal"]["status"], "paused");
    fixture.respond(vec![reply("ordinary answer")]);
    fixture.input(&url, "new message").await;
    fixture.until(&url, |s| s["status"] == "idle").await;
    let requests = fixture.requests.lock().unwrap().clone();
    let messages = requests.last().unwrap()["messages"].to_string();
    assert!(messages.contains("Retained after retract") && messages.contains("Paused"));
    assert!(!messages.contains("retract this message"));
    fixture.close().await;
}

#[tokio::test]
async fn goal_unknown_charge_stops_with_default_budget_but_zero_runs_past_ten_turns() {
    for budget in [7.0, 0.0] {
        let fixture = Fixture::new(json!({"goal_budget_usd":budget,"auto_wake_limit":0})).await;
        let url = fixture.create().await;
        let mut responses = vec![charged(reply("made progress"), 1)];
        if budget == 0.0 {
            responses.extend((0..11).map(|_| reply("another step")));
            responses.push(call("get_goal", json!({})));
            responses.push(call("update_goal", json!({"status":"blocked"})));
        }
        responses.push(reply("Summary and remaining blocker"));
        fixture.respond(responses);
        fixture.set(&url, json!({"objective":"Long goal"})).await;
        let expected = if budget == 0.0 {
            "blocked"
        } else {
            "budgetLimited"
        };
        let snapshot = fixture
            .until(&url, |s| {
                s["goal"]["status"] == expected && s["status"] == "idle"
            })
            .await;
        assert_eq!(snapshot["goal"]["budgetUsd"], budget);
        assert_eq!(snapshot["goal"]["cost"]["unknown"], true);
        assert_eq!(snapshot["goal"]["cost"]["usd"], 0.0);
        let count = fixture.requests.lock().unwrap().len();
        assert_eq!(count, if budget == 0.0 { 15 } else { 2 });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fixture.requests.lock().unwrap().len(), count);
        fixture.close().await;
    }
}

#[tokio::test]
async fn goal_escape_immediately_pauses_without_retracting_automatic_turn() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture
        .set(&url, json!({"objective":"Immediately interrupt"}))
        .await;
    let started = fixture.snapshot(&url).await;
    let interrupted: Value = fixture
        .client
        .post(format!("{url}/interrupt"))
        .json(&json!({"turnId":started["turns"][0]["id"],"retract":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(interrupted["retracted"], false);
    let snapshot = fixture.snapshot(&url).await;
    assert_eq!(snapshot["goal"]["status"], "paused");
    assert_eq!(snapshot["turns"][0]["status"], "interrupted");
    fixture.close().await;
}

#[tokio::test]
async fn goal_user_message_has_priority_and_its_question_does_not_timeout() {
    let fixture = Fixture::with_question_timeout(json!({}), Duration::from_millis(60)).await;
    let url = fixture.create().await;
    fixture
        .set(&url, json!({"objective":"Continue after user"}))
        .await;
    fixture.request_count(1).await;
    fixture.input(&url, "user priority").await;
    fixture.respond(vec![reply("first goal turn"), call("ask_user", json!({"question":"Choose?","options":[{"label":"A","description":"Use A"},{"label":"B","description":"Use B"}]}))]);
    let waiting = fixture
        .until(&url, |s| {
            !s["pendingRequests"].as_array().unwrap().is_empty()
        })
        .await;
    assert_eq!(waiting["turns"][1]["input"][0]["text"], "user priority");
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(
        fixture.snapshot(&url).await["pendingRequests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.respond(vec![
        reply("user answered"),
        call("update_goal", json!({"status":"complete"})),
        reply("finished"),
    ]);
    fixture
        .resolve(
            &url,
            &waiting["pendingRequests"][0],
            json!({"answer":{"type":"text","text":"A"},"clientName":"test"}),
        )
        .await;
    let done = fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["goal"]["status"] == "complete"
        })
        .await;
    assert_eq!(done["turns"].as_array().unwrap().len(), 3);
    fixture.close().await;
}

#[tokio::test]
async fn goal_restart_preserves_state_and_waits_for_user_before_continuing() {
    for resume in [false, true] {
        let fixture = Fixture::new(json!({})).await;
        let url = fixture.create().await;
        fixture
            .set(&url, json!({"objective":"Remember this goal"}))
            .await;
        fixture.request_count(1).await;
        let before = fixture.snapshot(&url).await["goal"].clone();
        fixture.reopen(&url).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.snapshot(&url).await["goal"], before);
        fixture.respond(vec![
            reply("one more step"),
            call("update_goal", json!({"status":"complete"})),
            reply("done"),
        ]);
        if resume {
            fixture.action(&url, "resume").await;
        } else {
            fixture.input(&url, "continue").await;
        }
        fixture
            .until(&url, |s| {
                s["goal"]["status"] == "complete" && s["status"] == "idle"
            })
            .await;
        assert!(
            fixture.requests.lock().unwrap()[1]["messages"]
                .to_string()
                .contains("Remember this goal")
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn goal_approval_wait_does_not_timeout_or_spend_and_paused_chat_is_not_charged() {
    let fixture = Fixture::with_question_timeout(
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
        Duration::from_millis(60),
    )
    .await;
    let url = fixture.create().await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"approvalPolicy":"alwaysAsk"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.respond(vec![charged(
        call("shell", json!({"command":"printf approved > approved.txt"})),
        1,
    )]);
    fixture
        .set(
            &url,
            json!({"objective":"Run approved command","budgetUsd":10}),
        )
        .await;
    let waiting = fixture
        .until(&url, |s| {
            !s["pendingRequests"].as_array().unwrap().is_empty()
        })
        .await;
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.snapshot(&url).await["goal"]["cost"]["usd"], 1.0);
    fixture.respond(vec![charged(reply("command finished"), 1)]);
    fixture
        .resolve(
            &url,
            &waiting["pendingRequests"][0],
            json!({"decision":"accept","clientName":"test"}),
        )
        .await;
    fixture.request_count(3).await;
    fixture.action(&url, "pause").await;
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("project/approved.txt")).unwrap(),
        "approved"
    );
    assert_eq!(fixture.snapshot(&url).await["goal"]["cost"]["usd"], 2.0);
    fixture.respond(vec![charged(reply("ordinary chat"), 4)]);
    fixture.input(&url, "while paused").await;
    fixture.until(&url, |s| s["status"] == "idle").await;
    assert_eq!(fixture.snapshot(&url).await["goal"]["cost"]["usd"], 2.0);
    fixture.close().await;
}

#[tokio::test]
async fn goal_automatic_compaction_cost_triggers_the_single_final_summary() {
    for interrupted in [false, true] {
        let fixture = Fixture::new(json!({"auto_compact_tokens":12000,"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}})).await;
        let url = fixture.create().await;
        fixture.respond(vec![reply("seed reply")]);
        fixture
            .input(&url, "Seed context for the compaction transcript")
            .await;
        fixture.until(&url, |s| s["status"] == "idle").await;
        let mut compressed = charged(reply("compressed context"), 9);
        if interrupted {
            compressed["choices"][0]["finish_reason"] = Value::Null;
        }
        fixture.respond(vec![
            compressed,
            charged(
                call(
                    "write",
                    json!({"path":"summary-must-not-start","content":"new work"}),
                ),
                1,
            ),
        ]);
        fixture.set(&url, json!({"objective":format!("Full long objective: {}", "details ".repeat(6000)),"budgetUsd":10})).await;
        let done = fixture
            .until(&url, |s| {
                s["status"] == "idle" && s["goal"]["status"] == "budgetLimited"
            })
            .await;
        assert_eq!(done["goal"]["cost"]["usd"], 10.0);
        let requests = fixture.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        // The intervening request is compaction; retain the conversation's tools.
        assert_eq!(requests[2]["tools"], requests[0]["tools"]);
        assert!(
            !fixture
                .root
                .path()
                .join("project/summary-must-not-start")
                .exists()
        );
        assert!(
            requests[1]["messages"]
                .to_string()
                .contains("Seed context for the compaction transcript")
        );
        assert!(
            requests[2]["messages"]
                .to_string()
                .contains("[Goal budget limit]")
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn goal_steering_during_summary_becomes_a_normal_turn_outside_goal_budget() {
    let fixture = Fixture::new(
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
    )
    .await;
    let url = fixture.create().await;
    fixture.respond(vec![charged(reply("progress"), 9)]);
    fixture
        .set(&url, json!({"objective":"Finish the goal","budgetUsd":10}))
        .await;
    fixture.request_count(2).await;
    fixture.client.post(format!("{url}/input")).json(&json!({"input":[{"type":"text","text":"new user request"}],"mode":"steer","clientName":"test"})).send().await.unwrap().error_for_status().unwrap();
    fixture.respond(vec![
        charged(reply("summary"), 1),
        charged(reply("answer to user"), 4),
    ]);
    let snapshot = fixture
        .until(&url, |s| {
            s["status"] == "idle" && fixture.requests.lock().unwrap().len() == 3
        })
        .await;
    assert_eq!(snapshot["goal"]["cost"]["usd"], 10.0);
    assert_eq!(snapshot["turns"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["turns"][1]["input"][0]["text"], "new user request");
    fixture.close().await;
}

#[tokio::test]
async fn goal_failure_does_not_retry_failed_turns_forever() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    // A malformed provider response is a non-retryable model error.
    fixture.respond(vec![json!({"choices":"invalid provider response"})]);
    fixture
        .set(&url, json!({"objective":"Task with broken model"}))
        .await;
    fixture
        .until(&url, |s| s["turns"][0]["status"] == "failed")
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.snapshot(&url).await["goal"]["status"], "active");
    fixture.respond(vec![
        call("update_goal", json!({"status":"complete"})),
        reply("finished"),
    ]);
    fixture.action(&url, "resume").await;
    fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["goal"]["status"] == "complete"
        })
        .await;
    fixture.close().await;
}

#[tokio::test]
async fn goal_clear_is_reasserted_after_rewinding_the_clear_notice() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture
        .set(&url, json!({"objective":"Old objective"}))
        .await;
    fixture.request_count(1).await;
    fixture.action(&url, "pause").await;
    fixture.respond(vec![reply("ordinary answer")]);
    fixture.input(&url, "rewind before this").await;
    let before = fixture.until(&url, |s| s["status"] == "idle").await;
    fixture
        .client
        .delete(format!("{url}/goal"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let mut args = json!({"beforeTurnId":before["turns"][1]["id"],"rewindConversation":true,"rewindFiles":false});
    let preview: Value = fixture
        .client
        .post(format!("{url}/rewind/preview"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    args["lastEventSeq"] = preview["lastEventSeq"].clone();
    fixture
        .client
        .post(format!("{url}/rewind"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.respond(vec![reply("no goal")]);
    fixture.input(&url, "what now").await;
    fixture.until(&url, |s| s["status"] == "idle").await;
    let requests = fixture.requests.lock().unwrap().clone();
    let last_notice = requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .rfind(|text| text.starts_with("[Goal"))
        .unwrap();
    assert!(last_notice.starts_with("[Goal cleared]"), "{last_notice}");
    assert!(fixture.snapshot(&url).await.get("goal").is_none());
    fixture.close().await;
}

#[tokio::test]
async fn goal_budget_reached_by_side_question_does_not_execute_waiting_tool() {
    for interrupted in [false, true] {
        let fixture = Fixture::new(
            json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
        )
        .await;
        let url = fixture.create().await;
        fixture
            .client
            .patch(&url)
            .json(&json!({"approvalPolicy":"alwaysAsk"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fixture.respond(vec![charged(
            call("shell", json!({"command":"printf approved > approved.txt"})),
            1,
        )]);
        fixture
            .set(&url, json!({"objective":"Run command","budgetUsd":10}))
            .await;
        let waiting = fixture
            .until(&url, |s| {
                !s["pendingRequests"].as_array().unwrap().is_empty()
            })
            .await;
        let mut side = charged(reply("side answer"), 8);
        if interrupted {
            side["choices"][0]["finish_reason"] = Value::Null;
        }
        fixture.respond(vec![side]);
        fixture
            .client
            .post(format!("{url}/btw"))
            .json(&json!({"question":"Quick question"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        fixture
            .until(&url, |s| s["goal"]["status"] == "budgetLimited")
            .await;
        fixture.respond(vec![charged(reply("summary"), 1)]);
        fixture
            .resolve(
                &url,
                &waiting["pendingRequests"][0],
                json!({"decision":"accept","clientName":"test"}),
            )
            .await;
        let done = fixture.until(&url, |s| s["status"] == "idle").await;
        assert!(!fixture.root.path().join("project/approved.txt").exists());
        assert_eq!(done["goal"]["cost"]["usd"], 10.0);
        assert_eq!(fixture.requests.lock().unwrap().len(), 3);
        fixture.close().await;
    }
}

#[tokio::test]
async fn goal_summary_failure_is_not_retried() {
    let fixture = Fixture::new(
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
    )
    .await;
    let url = fixture.create().await;
    let mut summary = reply("interrupted summary");
    summary["choices"][0]["finish_reason"] = Value::Null;
    fixture.respond(vec![charged(reply("progress"), 9), summary]);
    fixture
        .set(&url, json!({"objective":"Task","budgetUsd":10}))
        .await;
    let done = fixture
        .until(&url, |s| {
            s["status"] == "idle" && s["goal"]["status"] == "budgetLimited"
        })
        .await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(done["turns"][0]["status"], "failed");
    fixture.close().await;
}

#[tokio::test]
async fn goal_cannot_start_or_resume_during_server_shutdown() {
    let fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    *fixture.app.draining.write().await = true;
    let setting = fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Task"}))
        .send()
        .await
        .unwrap();
    assert_eq!(setting.status(), StatusCode::CONFLICT);
    assert!(fixture.snapshot(&url).await.get("goal").is_none());
    *fixture.app.draining.write().await = false;
    fixture.set(&url, json!({"objective":"Task"})).await;
    fixture.action(&url, "pause").await;
    *fixture.app.draining.write().await = true;
    let resuming = fixture
        .client
        .post(format!("{url}/goal/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resuming.status(), StatusCode::CONFLICT);
    assert_eq!(fixture.snapshot(&url).await["goal"]["status"], "paused");
    fixture.close().await;
}
