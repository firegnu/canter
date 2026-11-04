use super::*;

async fn receive(receiver: &mut mpsc::UnboundedReceiver<Request>) -> Request {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

fn usage_reply(request: Request, input_tokens: u64) {
    let chunk = json!({"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":input_tokens,"completion_tokens":1,"total_tokens":input_tokens+1}});
    request
        .reply
        .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        .unwrap();
}

async fn second_model() -> (
    String,
    mpsc::UnboundedReceiver<Request>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let (send, receive) = mpsc::unbounded_channel();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                let send = send.clone();
                async move {
                    let (reply, response) = oneshot::channel();
                    let _ = send.send(Request {
                        body,
                        headers,
                        reply,
                    });
                    (
                        [("content-type", "text/event-stream")],
                        response.await.unwrap_or_default(),
                    )
                }
            },
        ),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (address, receive, task)
}

async fn select(fixture: &Fixture, url: &str, model: &str) {
    fixture
        .client
        .patch(url)
        .json(&json!({"model":model}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn configured() -> (
    Fixture,
    mpsc::UnboundedReceiver<Request>,
    tokio::task::JoinHandle<()>,
) {
    let (second, receiver, task) = second_model().await;
    let mut fixture = Fixture::new(json!({})).await;
    fixture.config["providers"] = json!({
        "one":{"base_url":fixture.config["base_url"],"api_key":"test-only-one",
            "headers":{"x-provider":"one {session_id}"},
            "models":{"vendor/model":{"context_window":200000,"image":true}}},
        "two":{"api":"chat_completions","base_url":second,"api_key":"test-only-two",
            "headers":{"x-provider":"two {session_id}"},
            "models":{"small":{"context_window":100000,"auto_compact_tokens":15000}}}
    });
    fixture.applied("providers-ready").await;
    (fixture, receiver, task)
}

#[tokio::test]
async fn sessions_route_concurrently_and_switch_providers_with_their_limits() {
    let (mut fixture, mut second, task) = configured().await;
    let models: Value = fixture
        .client
        .get("http://localhost/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let model = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "one/vendor/model")
        .unwrap();
    assert_eq!(model["provider"], "one");
    assert_eq!(model["contextWindow"], 200000);
    assert_eq!(model["inputModalities"], json!(["text", "image"]));
    let first_url = fixture.create().await;
    let second_url = fixture.create().await;
    select(&fixture, &first_url, "one/vendor/model").await;
    select(&fixture, &second_url, "two/small").await;
    fixture.input(&first_url).await;
    let first = fixture.request().await;
    assert_eq!(first.body["model"], "vendor/model");
    assert_eq!(first.headers["authorization"], "Bearer test-only-one");
    assert_eq!(
        first.headers["x-provider"],
        format!("one {}", first_url.rsplit('/').next().unwrap())
    );
    fixture.input(&second_url).await;
    let other = tokio::time::timeout(Duration::from_secs(5), second.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.body["model"], "small");
    assert_eq!(other.headers["authorization"], "Bearer test-only-two");
    assert_eq!(
        fixture.context(&first_url).await.model_context_window,
        200000
    );
    assert_eq!(
        fixture.context(&second_url).await.auto_compact_tokens,
        Some(15000)
    );
    first.stop();
    other.stop();
    fixture.idle(&first_url).await;
    fixture.idle(&second_url).await;
    select(&fixture, &first_url, "two/small").await;
    assert_eq!(
        fixture.context(&first_url).await.model_context_window,
        100000
    );
    fixture.input(&first_url).await;
    let switched = tokio::time::timeout(Duration::from_secs(5), second.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(switched.body["model"], "small");
    assert_eq!(switched.headers["authorization"], "Bearer test-only-two");
    let messages = switched.body["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "assistant" && message["content"] == "done")
    );
    switched.stop();
    fixture.idle(&first_url).await;
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn image_support_and_search_follow_the_configured_model_catalogue() {
    let (mut fixture, mut second, task) = configured().await;
    fixture.config["providers"]["search"] = json!({"base_url":"https://api.deepseek.com","api_key":"test-only-search-key",
        "models":{"search-model":{"context_window":100000}}});
    fixture.applied("search-ready").await;
    let url = fixture.create().await;
    let image = fixture.root.path().join("tiny.png");
    fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    select(&fixture, &url, "two/small").await;
    let response = fixture
        .client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"localImage","path":image}],"clientName":"test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("does not accept images")
    );
    fixture.input(&url).await;
    let no_image = receive(&mut second).await;
    assert!(no_image.tools().contains(&"web_search"));
    assert_eq!(no_image.headers["authorization"], "Bearer test-only-two");
    no_image.stop();
    fixture.idle(&url).await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture
        .client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"localImage","path":image}],"clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let accepts_image = fixture.request().await;
    assert!(
        accepts_image.body["messages"]
            .to_string()
            .contains("data:image/png;base64,")
    );
    assert!(accepts_image.tools().contains(&"web_search"));
    accepts_image.stop();
    fixture.idle(&url).await;
    fixture.config["providers"]
        .as_object_mut()
        .unwrap()
        .remove("search");
    fixture.applied("search-removed").await;
    fixture.input(&url).await;
    let no_search = fixture.request().await;
    assert!(!no_search.tools().contains(&"web_search"));
    no_search.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn switching_providers_waits_for_active_turns_and_private_answers() {
    let (mut fixture, _, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.input(&url).await;
    let request = fixture.request().await;
    let response = fixture
        .client
        .patch(&url)
        .json(&json!({"model":"two/small"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(
        fixture.snapshot(&url).await.session.settings.model,
        "one/vendor/model"
    );
    request.stop();
    fixture.idle(&url).await;

    let client = fixture.client.clone();
    let btw_url = format!("{url}/btw");
    let answer = tokio::spawn(async move {
        client
            .post(btw_url)
            .json(&json!({"question":"private question"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    let private = fixture.request().await;
    let response = fixture
        .client
        .patch(&url)
        .json(&json!({"model":"two/small"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    private.stop();
    answer.await.unwrap();
    select(&fixture, &url, "two/small").await;
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn smaller_provider_compacts_before_the_next_conversation_request() {
    let (mut fixture, mut second, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.input(&url).await;
    usage_reply(fixture.request().await, 50000);
    fixture.idle(&url).await;
    select(&fixture, &url, "two/small").await;
    fixture.input(&url).await;
    let compact = receive(&mut second).await;
    assert_eq!(compact.body["model"], "small");
    assert_eq!(compact.headers["authorization"], "Bearer test-only-two");
    assert!(
        compact.body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .starts_with("Summarize the conversation")
    );
    usage_reply(compact, 1000);
    let next = receive(&mut second).await;
    assert!(
        next.body["messages"]
            .to_string()
            .contains("[Context summary]")
    );
    usage_reply(next, 2000);
    fixture.idle(&url).await;
    let snapshot = fixture.snapshot(&url).await.session;
    assert_eq!(
        snapshot.token_usage.as_ref().unwrap().model_context_window,
        Some(100000)
    );
    assert_eq!(
        snapshot.token_usage.as_ref().unwrap().auto_compact_tokens,
        Some(15000)
    );
    assert_eq!(
        snapshot
            .token_usage
            .as_ref()
            .unwrap()
            .request
            .as_ref()
            .unwrap()
            .model,
        "two/small"
    );
    assert!(fixture.requests.try_recv().is_err());
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

/// P2-201: an idle switch reports the new model's window and compaction line
/// at once, as a usage update that records no request, so status bars need
/// not wait for the next reply; usage, cost and stats stay as they were.
#[tokio::test]
async fn idle_switch_reports_the_new_limits_without_counting_a_request() {
    let (mut fixture, _second, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.input(&url).await;
    usage_reply(fixture.request().await, 50000);
    fixture.idle(&url).await;
    let get = |path: String| {
        let client = fixture.client.clone();
        async move {
            client
                .get(path)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let usage_before = get(format!("{url}/usage")).await;
    let stats_before = get(format!("{url}/stats")).await;
    let before = fixture.snapshot(&url).await.session.token_usage.unwrap();
    assert_eq!(before.model_context_window, Some(200000));

    select(&fixture, &url, "two/small").await;
    let after = fixture.snapshot(&url).await.session.token_usage.unwrap();
    assert_eq!(after.model_context_window, Some(100000));
    assert_eq!(after.auto_compact_tokens, Some(15000));
    assert_eq!(after.context_tokens, before.context_tokens);
    assert_eq!(after.total, before.total);
    assert_eq!(after.cost, before.cost);
    assert!(after.request.is_none());
    assert_eq!(get(format!("{url}/usage")).await, usage_before);
    assert_eq!(
        get(format!("{url}/stats")).await["requests"],
        stats_before["requests"]
    );
    // Choosing the model in use again adds nothing.
    let seq = fixture.snapshot(&url).await.event_seq;
    select(&fixture, &url, "two/small").await;
    assert_eq!(
        fixture.snapshot(&url).await.session.token_usage.unwrap(),
        after
    );
    assert!(fixture.snapshot(&url).await.event_seq <= seq + 1);
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn budget_notice_uses_the_selected_providers_compaction_line() {
    let (mut fixture, mut second, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.input(&url).await;
    usage_reply(fixture.request().await, 13000);
    fixture.idle(&url).await;
    select(&fixture, &url, "two/small").await;
    fixture.input(&url).await;
    let request = receive(&mut second).await;
    let messages = request.body["messages"].to_string();
    assert!(messages.contains("[Context budget]") && messages.contains("of 15000 tokens"));
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn subagent_inherits_its_parents_provider_and_uses_its_own_header_id() {
    let (fixture, mut second, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "two/small").await;
    fixture.input(&url).await;
    receive(&mut second).await.tool(
        "spawn_agent",
        json!({"task":"Inspect only","type":"explorer","name":"Inspector"}),
    );
    let first = receive(&mut second).await;
    let next = receive(&mut second).await;
    let parent_header = format!("two {}", url.rsplit('/').next().unwrap());
    let (parent, child) = if first.headers["x-provider"] == parent_header {
        (first, next)
    } else {
        (next, first)
    };
    assert_eq!(parent.headers["x-provider"], parent_header);
    assert_ne!(child.headers["x-provider"], parent_header);
    assert_eq!(child.headers["authorization"], "Bearer test-only-two");
    assert_eq!(child.body["model"], "small");
    assert!(!child.tools().contains(&"spawn_agent"));
    let child_id = child.headers["x-provider"]
        .to_str()
        .unwrap()
        .strip_prefix("two ")
        .unwrap()
        .to_owned();
    let child_url = format!("http://localhost/v1/sessions/{child_id}");
    assert_eq!(
        fixture.context(&child_url).await.model_context_window,
        100000
    );
    child.stop();
    fixture.idle(&child_url).await;
    parent.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn provider_reload_is_atomic_and_frozen_until_the_next_turn() {
    let (mut fixture, mut second, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.input(&url).await;
    let running = fixture.request().await;
    fixture.config["providers"]["one"]["base_url"] =
        fixture.config["providers"]["two"]["base_url"].clone();
    fixture.config["providers"]["one"]["api_key"] = json!("test-only-new-key");
    fixture.config["providers"]["one"]["models"]["vendor/model"]["context_window"] = json!(120000);
    fixture.applied("provider-edited").await;
    running.tool(
        "update_plan",
        json!({"plan":[{"step":"Checked","status":"completed"}]}),
    );
    let still_old = fixture.request().await;
    assert_eq!(still_old.headers["authorization"], "Bearer test-only-one");
    assert_eq!(fixture.context(&url).await.model_context_window, 200000);
    still_old.stop();
    fixture.idle(&url).await;
    fixture.input(&url).await;
    let fresh = receive(&mut second).await;
    assert_eq!(fresh.headers["authorization"], "Bearer test-only-new-key");
    assert_eq!(fresh.body["model"], "vendor/model");
    assert_eq!(fixture.context(&url).await.model_context_window, 120000);
    fresh.stop();
    fixture.idle(&url).await;

    let mut warnings = fixture.warnings(&url).await;
    fixture.config["providers"]["two"]["api"] = json!("unsupported_api");
    fixture.config["providers"]["one"]["api_key"] = json!("test-only-rejected-key");
    fixture.config["providers"]["one"]["headers"] = json!({"x-secret":"test-only-rejected-header"});
    fixture.save();
    let warning = warnings.next().await;
    assert!(warning.contains("provider two") && warning.contains("unsupported api"));
    for secret in ["test-only-rejected-key", "test-only-rejected-header"] {
        assert!(!warning.contains(secret));
    }
    fixture.input(&url).await;
    let retained = receive(&mut second).await;
    assert_eq!(
        retained.headers["authorization"],
        "Bearer test-only-new-key"
    );
    assert!(!retained.headers.contains_key("x-secret"));
    retained.stop();
    fixture.idle(&url).await;
    fixture.config["providers"]["two"]
        .as_object_mut()
        .unwrap()
        .remove("api_key");
    fixture.config["providers"]["two"]["api"] = json!("chat_completions");
    fixture.save();
    assert!(
        warnings
            .next()
            .await
            .contains("provider two: invalid or missing api_key")
    );
    warnings.task.abort();
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn removing_a_provider_does_not_send_its_sessions_to_the_legacy_provider() {
    let (mut fixture, _, task) = configured().await;
    let url = fixture.create().await;
    select(&fixture, &url, "one/vendor/model").await;
    fixture.config["providers"]
        .as_object_mut()
        .unwrap()
        .remove("one");
    fixture.applied("providers-removed").await;
    fixture.input(&url).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), fixture.requests.recv())
            .await
            .is_err(),
        "a removed provider must not fall back to the legacy endpoint"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.snapshot(&url).await.session.status != SessionStatus::Idle {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .turns
            .last()
            .unwrap()
            .status,
        canter_protocol::TurnStatus::Failed
    );
    fixture.shutdown().await;
    task.abort();
    let _ = task.await;
}
