use super::*;
use crate::chatgpt_auth::{ChatgptAuth, test_support::Fake};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}

struct CatalogServer {
    endpoint: reqwest::Url,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<()>,
    task: tokio::task::JoinHandle<()>,
}

impl CatalogServer {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/models", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let route_replies = replies.clone();
        let (sender, requests) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(move || {
                let replies = route_replies.clone();
                let sender = sender.clone();
                async move {
                    sender.send(()).unwrap();
                    let reply = replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected refresh");
                    tokio::time::sleep(reply.delay).await;
                    (
                        axum::http::StatusCode::from_u16(reply.status).unwrap(),
                        axum::Json(reply.body.parse::<Value>().unwrap()),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            replies,
            requests,
            task,
        }
    }

    async fn next(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .expect("catalog request did not arrive")
            .unwrap();
    }

    fn push(&self, status: u16, body: Value) {
        self.replies.lock().unwrap().push_back(Reply {
            status,
            body: body.to_string(),
            delay: Duration::ZERO,
        });
    }
}

impl Drop for CatalogServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn ok(body: Value) -> Reply {
    Reply {
        status: 200,
        body: body.to_string(),
        delay: Duration::ZERO,
    }
}

fn delayed(body: Value, delay: Duration) -> Reply {
    Reply {
        status: 200,
        body: body.to_string(),
        delay,
    }
}

async fn login(home: &Path, fake: &Fake) -> (ChatgptAuth, String) {
    let now = fake.state.lock().unwrap().now.clone();
    let auth = ChatgptAuth::for_test(
        home,
        fake.origin.clone(),
        Arc::new(move || now.load(std::sync::atomic::Ordering::SeqCst)),
    );
    let attempt = auth.start_login(None).await.unwrap();
    assert!(fake.callback(&attempt).await.status().is_success());
    let account = attempt.wait().await.unwrap();
    auth.complete_login(&account.id, true).await.unwrap();
    (auth, account.id)
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not become true");
}

fn full(slug: &str, name: &str) -> Value {
    json!({
        "slug":slug,"display_name":name,"visibility":"list",
        "context_window":200000,
        "supported_reasoning_levels":[{"effort":"medium"},{"effort":"future"},{"effort":"high"}],
        "default_reasoning_level":"medium","input_modalities":["text","image"],
        "unknown_field":{"is":"ignored"}
    })
}

fn bare(slug: &str) -> Value {
    json!({"slug":slug,"visibility":"list"})
}

#[tokio::test]
async fn parsing_precedence_defaults_cache_and_restart() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, account) = login(&home, &fake).await;
    let mut server = CatalogServer::new(vec![ok(json!({"models":[
        full("first", "First"),
        json!({"slug":"hidden","display_name":"Hidden","visibility":"hidden"}),
        bare("second"), bare("second"), bare("third"),
        json!({"slug":"named","display_name":null,"visibility":"list",
            "supported_reasoning_levels":[{"effort":"future"}],
            "default_reasoning_level":"future","input_modalities":"unknown"})
    ]}))])
    .await;
    let catalog = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| catalog.state.read().unwrap().models.len() == 4).await;

    let mut options = CoreOptions::default();
    options.model_limits.insert(
        "chatgpt/first".into(),
        ModelLimits {
            context_window: 300000,
            auto_compact_tokens: Some(250000),
        },
    );
    options.model_reasoning.insert(
        "chatgpt/first".into(),
        ModelReasoning {
            supported: vec![ReasoningEffort::High],
            default: ReasoningEffort::High,
        },
    );
    options.model_limits.insert(
        "chatgpt/manual".into(),
        ModelLimits {
            context_window: 180000,
            auto_compact_tokens: None,
        },
    );
    let (models, options) = catalog.apply(
        Some("chatgpt"),
        vec!["chatgpt/manual".into(), "chatgpt/first".into()],
        options,
    );
    assert_eq!(
        models,
        [
            "chatgpt/first",
            "chatgpt/second",
            "chatgpt/third",
            "chatgpt/named",
            "chatgpt/manual"
        ]
    );
    assert_eq!(options.limits("chatgpt/first").context_window, 300000);
    assert_eq!(
        options.reasoning("chatgpt/first").default,
        ReasoningEffort::High
    );
    assert!(!options.image_models.contains(&"chatgpt/first".into()));
    for slug in ["second", "third", "named"] {
        let id = format!("chatgpt/{slug}");
        assert_eq!(options.limits(&id).context_window, 128000);
        assert_eq!(options.reasoning(&id).supported, [ReasoningEffort::Medium]);
        assert_eq!(options.reasoning(&id).default, ReasoningEffort::Medium);
        assert!(!options.image_models.contains(&id));
    }
    assert_eq!(catalog.details("chatgpt/second").unwrap().0, "second");
    assert_eq!(
        catalog.details("chatgpt/second").unwrap().1,
        ["contextWindow", "reasoningEffort", "imageInput"]
    );
    assert_eq!(
        catalog.details("chatgpt/first").unwrap().1,
        Vec::<String>::new()
    );
    let (_, directory_options) = catalog.apply(Some("chatgpt"), Vec::new(), CoreOptions::default());
    assert_eq!(
        directory_options.limits("chatgpt/first").context_window,
        200000
    );
    assert_eq!(
        directory_options.reasoning("chatgpt/first").supported,
        [ReasoningEffort::Medium, ReasoningEffort::High]
    );
    assert!(
        directory_options
            .image_models
            .contains(&"chatgpt/first".into())
    );

    let file = cache_dir(&home).join(format!("{account}.json"));
    let contents = fs::read_to_string(&file).unwrap();
    assert!(!contents.contains("fake-access-1"));
    assert!(!contents.contains("fake-refresh-1"));
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(catalog);
    let restarted = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    assert_eq!(
        restarted.activate(&auth).await.map(|(account, _)| account),
        Some(account)
    );
    let (models, options) = restarted.apply(Some("chatgpt"), Vec::new(), CoreOptions::default());
    assert_eq!(models.len(), 4);
    assert_eq!(options.limits("chatgpt/second").context_window, 128000);
    fake.stop().await;
}

#[tokio::test]
async fn failed_refresh_uses_cache_and_empty_success_removes_listings() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake).await;
    let mut server = CatalogServer::new(vec![ok(json!({"models":[bare("old")]}))]).await;
    let catalog = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| catalog.state.read().unwrap().models.len() == 1).await;
    server.push(500, json!({"error":"not returned"}));
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| catalog.state.read().unwrap().refresh_failed).await;
    assert_eq!(catalog.details("chatgpt/old").unwrap().0, "old");
    let status = catalog.status("chatgpt").unwrap();
    assert!(status.stale && status.refresh_failed && status.fetched_at.is_some());

    server.push(200, json!({"models":[]}));
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| !catalog.state.read().unwrap().refresh_failed).await;
    assert!(catalog.state.read().unwrap().models.is_empty());
    let (models, options) = catalog.apply(Some("chatgpt"), Vec::new(), CoreOptions::default());
    assert!(models.is_empty());
    assert_eq!(options.limits("chatgpt/old").context_window, 128000);
    fake.stop().await;
}

#[tokio::test]
async fn switching_accounts_ignores_an_old_inflight_result_and_logout_deletes_cache() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, first) = login(&home, &fake).await;
    let mut server = CatalogServer::new(vec![
        delayed(
            json!({"models":[bare("first-model")]}),
            Duration::from_millis(200),
        ),
        ok(json!({"models":[bare("second-model")]})),
    ])
    .await;
    let catalog = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    fake.state.lock().unwrap().client_id = "second-test-client".into();
    let attempt = auth.start_login(None).await.unwrap();
    assert!(fake.callback(&attempt).await.status().is_success());
    let second = attempt.wait().await.unwrap();
    auth.complete_login(&second.id, true).await.unwrap();
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| {
        catalog
            .state
            .read()
            .unwrap()
            .models
            .iter()
            .any(|model| model.slug == "second-model")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    {
        let state = catalog.state.read().unwrap();
        assert_eq!(state.models.len(), 1);
        assert_eq!(state.models[0].slug, "second-model");
        assert_eq!(state.account.as_deref(), Some(second.id.as_str()));
    }
    assert!(!cache_dir(&home).join(format!("{first}.json")).exists());

    auth.logout(&second.id).await.unwrap();
    catalog.logout(&auth, &second.id).await;
    assert!(
        !cache_dir(&home)
            .join(format!("{}.json", second.id))
            .exists()
    );
    let (models, _) = catalog.apply(Some("chatgpt"), Vec::new(), CoreOptions::default());
    assert!(models.is_empty());
    fake.stop().await;
}

#[tokio::test]
async fn no_login_or_plan_does_not_fetch_and_failure_without_cache_lists_nothing() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let now = fake.state.lock().unwrap().now.clone();
    let auth = ChatgptAuth::for_test(
        &home,
        fake.origin.clone(),
        Arc::new(move || now.load(std::sync::atomic::Ordering::SeqCst)),
    );
    let mut server = CatalogServer::new(Vec::new()).await;
    let catalog = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    catalog.account_changed(auth.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), server.requests.recv())
            .await
            .is_err()
    );
    let (models, _) = catalog.apply(
        Some("chatgpt"),
        vec!["chatgpt/manual".into()],
        CoreOptions::default(),
    );
    assert_eq!(models, ["chatgpt/manual"]);
    assert!(catalog.status("chatgpt").is_none());

    fake.state.lock().unwrap().scopes =
        "openid profile email offline_access resource.invoke".into();
    let attempt = auth.start_login(None).await.unwrap();
    assert!(fake.callback(&attempt).await.status().is_success());
    let account = attempt.wait().await.unwrap();
    auth.complete_login(&account.id, true).await.unwrap();
    catalog.account_changed(auth.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), server.requests.recv())
            .await
            .is_err()
    );
    assert!(catalog.status("chatgpt").is_none());
    fake.stop().await;
}

#[tokio::test]
async fn malformed_and_unauthorized_responses_fail_without_replacing_cache() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake).await;
    let mut server = CatalogServer::new(vec![ok(json!({"models":[bare("known")]}))]).await;
    let catalog = ModelCatalog::new(home.clone(), Some(server.endpoint.clone())).unwrap();
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    until(|| catalog.state.read().unwrap().models.len() == 1).await;
    for body in [json!({"wrong":[]}), json!({"models":"wrong"})] {
        server.push(200, body);
        catalog.account_changed(auth.clone()).await;
        server.next().await;
        until(|| catalog.state.read().unwrap().refresh_failed).await;
        assert!(catalog.details("chatgpt/known").is_some());
        server.push(200, json!({"models":[bare("known")]}));
        catalog.account_changed(auth.clone()).await;
        server.next().await;
        until(|| !catalog.state.read().unwrap().refresh_failed).await;
    }
    server.push(401, json!({"error":"rejected"}));
    server.push(200, json!({"models":[bare("recovered")]}));
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    server.next().await;
    until(|| catalog.details("chatgpt/recovered").is_some()).await;
    server.push(401, json!({"error":"rejected"}));
    server.push(401, json!({"error":"still rejected"}));
    catalog.account_changed(auth.clone()).await;
    server.next().await;
    server.next().await;
    until(|| catalog.state.read().unwrap().refresh_failed).await;
    assert!(catalog.details("chatgpt/recovered").is_some());
    fake.stop().await;
}

#[tokio::test]
async fn opening_a_stale_catalog_refreshes_in_the_background() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake).await;
    let mut server = CatalogServer::new(vec![
        ok(json!({"models":[bare("first")]})),
        ok(json!({"models":[bare("refreshed")]})),
    ])
    .await;
    let catalog = ModelCatalog::new(home, Some(server.endpoint.clone())).unwrap();
    catalog.start(auth.clone());
    server.next().await;
    until(|| catalog.details("chatgpt/first").is_some()).await;
    catalog.state.write().unwrap().fetched_at =
        Some(now_ms() - REFRESH_AFTER.as_millis() as i64 - 1);
    catalog.refresh_stale(auth);
    server.next().await;
    until(|| catalog.details("chatgpt/refreshed").is_some()).await;
    assert!(!catalog.state.read().unwrap().refreshing);
    fake.stop().await;
}

#[tokio::test]
async fn activation_cannot_leave_a_new_account_stuck_behind_an_old_refresh() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, first) = login(&home, &fake).await;
    fake.state.lock().unwrap().client_id = "second-test-client".into();
    let attempt = auth.start_login(None).await.unwrap();
    assert!(fake.callback(&attempt).await.status().is_success());
    let second = attempt.wait().await.unwrap();
    auth.complete_login(&second.id, true).await.unwrap();
    auth.select(&first).await.unwrap();
    let server = CatalogServer::new(vec![ok(json!({"models":[bare("new-account")]}))]).await;
    let catalog = ModelCatalog::new(home, Some(server.endpoint.clone())).unwrap();
    catalog
        .pause_after_activate
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    catalog.spawn_refresh(auth.clone(), true);
    tokio::time::timeout(
        Duration::from_secs(5),
        catalog.pause_after_activate.entered.notified(),
    )
    .await
    .unwrap();

    auth.select(&second.id).await.unwrap();
    assert_eq!(
        catalog.activate(&auth).await.map(|(account, _)| account),
        Some(second.id.clone())
    );
    catalog.pause_after_activate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    catalog.account_changed(auth.clone()).await;
    until(|| catalog.details("chatgpt/new-account").is_some()).await;
    assert!(!catalog.state.read().unwrap().refreshing);
    fake.stop().await;
}

#[test]
fn apply_patch_capability_comes_only_from_the_catalog_even_for_configured_models() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = ModelCatalog::new(temp.path().to_owned(), None).unwrap();
    for (slug, capability) in [
        ("freeform", Some("freeform")),
        ("missing", None),
        ("unknown", Some("grammar")),
    ] {
        let mut value = json!({"slug":slug,"visibility":"list"});
        if let Some(capability) = capability {
            value["apply_patch_tool_type"] = capability.into();
        }
        let model = parse_model(&value).unwrap();
        catalog
            .state
            .write()
            .unwrap()
            .known
            .insert(slug.into(), model);
    }
    let mut options = CoreOptions::default();
    options.model_limits.insert(
        "chatgpt/freeform".into(),
        ModelLimits {
            context_window: 200_000,
            auto_compact_tokens: None,
        },
    );
    let (models, options) = catalog.apply(
        Some("chatgpt"),
        vec!["chatgpt/freeform".into(), "other/model".into()],
        options,
    );
    assert!(models.contains(&"chatgpt/freeform".into()));
    assert!(models.contains(&"other/model".into()));
    assert_eq!(
        options.apply_patch_models,
        ["chatgpt/freeform".to_owned()].into()
    );
    assert_eq!(
        options.model_limits["chatgpt/freeform"].context_window,
        200_000
    );
}
