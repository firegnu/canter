use super::*;
use crate::model_test_server as chat;

async fn idle(session: &Session) -> SessionSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = session.snapshot().await.session;
            if matches!(snapshot.status, SessionStatus::Idle | SessionStatus::Error) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn custom_role_routes_to_responses_and_missing_login_fails_without_fallback() {
    for signed_in in [false, true] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let home = root.path().join("state");
        let oauth = Fake::new().await;
        let (auth, record) = login(&home, &oauth, true).await;
        let reply = (
            200,
            format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":16,"output_tokens":2,"total_tokens":18,"input_tokens_details":{"cached_tokens":4}}}})
            ),
        );
        let mut inference = Inference::with_catalog(vec![reply], json!({"models":[{"slug":"worker","visibility":"list","apply_patch_tool_type":"freeform"}]})).await;
        let chat = chat::Server::start(vec![chat::Reply::sse("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Parent ready\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")]).await;
        std::fs::write(home.join("config.json"), json!({
            "model":"lookup/parent","prompt_suggestions":false,"sandbox":false,"model_max_retries":0,
            "providers":{
                "lookup":{"api":"chat_completions","base_url":chat.base_url,"api_key":chat::KEY,"models":{"parent":{"context_window":200000}}},
                "solver":{"api":"responses","auth":"chatgpt","models":{"worker":{"context_window":300000,"auto_compact_tokens":120000,"image":true,"supported_reasoning_efforts":["medium","high"],"default_reasoning_effort":"medium"}}}
            }
        }).to_string()).unwrap();
        std::fs::create_dir(home.join("agents")).unwrap();
        std::fs::write(home.join("agents/reviewer.md"), "---\nname: reviewer\ndescription: Review changes independently.\nmodel: solver/worker\n---\nUse the role-specific model.").unwrap();
        let config = Config::load(&home).unwrap();
        let source = ModelAuth::for_test(home.clone(), auth.clone(), inference.endpoint.clone());
        source.catalog().account_changed(auth.clone()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, options) = source.catalog().apply(
                    config.catalog_provider(),
                    config.models.clone(),
                    config.options(),
                );
                if options.apply_patch_models.contains("solver/worker") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if !signed_in {
            auth.logout(&record).await.unwrap();
        }
        let model = config.model_client(Some(source.clone())).unwrap();
        let live = crate::reload::LiveConfig::connect(
            home.clone(),
            config,
            model.clone(),
            canter_tools::lsp::LspService::new(Default::default()).unwrap(),
            CancellationToken::new(),
            source,
        )
        .await;
        let configuration = live.snapshot();
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                ("CANTER_HOME".into(), home.display().to_string()),
            ]
            .into(),
            ..Default::default()
        };
        let (parent, _) = Session::create(Arc::new(canter_store::Store::new(home.join("sessions")).unwrap()), "parent".into(),
            serde_json::from_value(json!({"cwd":root.path(),"model":"lookup/parent","reasoningEffort":"high","approvalPolicy":"alwaysAsk"})).unwrap(),
            runtime, model, configuration.options,).await.unwrap();
        parent.set_configuration_source(live.clone(), configuration.revision);
        let mut params = input();
        params.non_interactive = true;
        parent.input(params).await.unwrap();
        assert_eq!(idle(&parent).await.status, SessionStatus::Idle);
        let spawned = parent
            .spawn_agent_of_type(
                "child".into(),
                "Review the recent change".into(),
                serde_json::from_value(json!("reviewer")).unwrap(),
                None,
                None,
            )
            .await;
        if !signed_in {
            assert!(
                spawned.is_err(),
                "a logged-out role must fail at spawn time"
            );
            assert!(spawned.err().unwrap().message.contains("ChatGPT"));
            assert!(inference.requests.try_recv().is_err());
        } else {
            let (child, _) = spawned.unwrap();
            let (body, _) = inference.next().await;
            let snapshot = idle(&child).await;
            assert_eq!(snapshot.status, SessionStatus::Idle);
            assert_eq!(snapshot.settings.model, "solver/worker");
            assert_eq!(snapshot.settings.reasoning_effort, ReasoningEffort::Medium);
            assert_eq!(body["model"], "worker");
            assert_eq!(body["reasoning"]["effort"], "medium");
            let tools = body["tools"].to_string();
            assert!(tools.contains("\"name\":\"apply_patch\""));
            assert!(!tools.contains("\"name\":\"write\"") && !tools.contains("\"name\":\"edit\""));
            assert_eq!(child.context().await.model_context_window, 300000);
            let usage = serde_json::to_value(snapshot.token_usage).unwrap();
            assert_eq!(usage["cost"]["chatgptPlan"], true, "{usage}");
            child.close().await.unwrap();
        }
        assert_eq!(
            chat.requests.lock().unwrap().len(),
            1,
            "no fallback to parent's provider"
        );
        parent.close().await.unwrap();
        live.shutdown().await;
        oauth.stop().await;
        chat.stop().await;
    }
}
