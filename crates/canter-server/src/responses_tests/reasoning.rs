use super::*;
use canter_core::{Configuration, ConfigurationSource, CoreOptions, ForkSnapshot};

struct ChangedConfiguration {
    model: Arc<dyn canter_model::Model>,
    options: CoreOptions,
}

impl ConfigurationSource for ChangedConfiguration {
    fn snapshot(&self) -> Configuration {
        Configuration {
            revision: Some(1),
            options: self.options.clone(),
            model: self.model.clone(),
            mcp_tools: Vec::new(),
            lsp: None,
            lease: Arc::new(()),
        }
    }

    fn acquire(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<Configuration, canter_core::CoreError>> {
        Box::pin(async { Ok(self.snapshot()) })
    }

    fn options(&self) -> CoreOptions {
        self.options.clone()
    }
}

async fn check_recovery(boundary: &str) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake, true).await;
    let mut inference = Inference::new(vec![completed(json!([]))]).await;
    let config = config(&home);
    let model = config
        .model_client(Some(ModelAuth::for_test(
            home.clone(),
            auth,
            inference.endpoint.clone(),
        )))
        .unwrap();
    let mut options = config.options();
    let reasoning = options
        .model_reasoning
        .get_mut("chatgpt/test-model")
        .unwrap();
    reasoning.supported = vec![
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
    ];
    reasoning.default = ReasoningEffort::Medium;
    let store = Arc::new(canter_store::Store::new(home.join("sessions")).unwrap());
    let runtime = RuntimeContext {
        environment: BTreeMap::from([
            (
                "HOME".into(),
                root.path().join("home").to_string_lossy().into_owned(),
            ),
            ("CANTER_HOME".into(), home.to_string_lossy().into_owned()),
        ]),
        ..Default::default()
    };
    let mut settings: SessionSettings = serde_json::from_value(json!({
        "cwd":root.path(),"model":"chatgpt/test-model","reasoningEffort":"none","approvalPolicy":"allowAll"
    })).unwrap();
    let (session, cursor, previous) = if boundary == "reload" {
        settings.reasoning_effort = ReasoningEffort::Xhigh;
        let (session, response) = Session::create(
            store.clone(),
            "source".into(),
            settings,
            runtime,
            model.clone(),
            options.clone(),
        )
        .await
        .unwrap();
        options
            .model_reasoning
            .get_mut("chatgpt/test-model")
            .unwrap()
            .supported = vec![ReasoningEffort::Medium, ReasoningEffort::High];
        session
            .set_configuration_source(Arc::new(ChangedConfiguration { model, options }), Some(0));
        (session, response.event_seq, "xhigh")
    } else {
        drop(store.create("source", settings, 0).unwrap());
        if boundary == "fork" {
            let (session, response) = ForkSnapshot::read(&store, "source")
                .unwrap()
                .create(store.clone(), "fork".into(), runtime, model, options)
                .await
                .unwrap();
            assert_eq!(
                response.session.settings.reasoning_effort,
                ReasoningEffort::Medium
            );
            assert_eq!(
                store
                    .open("source")
                    .unwrap()
                    .snapshot()
                    .settings
                    .reasoning_effort,
                ReasoningEffort::None
            );
            (session, response.event_seq, "none")
        } else {
            let (session, response) =
                Session::open(store.clone(), "source".into(), runtime, model, options)
                    .await
                    .unwrap();
            assert_eq!(
                response.session.settings.reasoning_effort,
                ReasoningEffort::Medium
            );
            (session, response.event_seq, "none")
        }
    };
    let mut events = session.subscribe(cursor).await.unwrap();
    session.input(input()).await.unwrap();
    let (body, _) = inference.next().await;
    assert_eq!(
        body["reasoning"],
        json!({"summary":"auto","effort":"medium"})
    );
    let seen = finished(&mut events).await;
    let expected =
        format!("Thinking effort changed from {previous} to medium for chatgpt/test-model.");
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message == &expected)), "{seen:?}");
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.settings.reasoning_effort, ReasoningEffort::Medium);
    assert!(store.events_after(&snapshot.id, 0).unwrap().iter().any(|event| matches!(&event.payload, EventPayload::SessionUpdated { settings } if settings.reasoning_effort == ReasoningEffort::Medium)));
    session.close().await.unwrap();
}

#[tokio::test]
async fn opening_saved_none_uses_model_default_and_delivers_warning() {
    check_recovery("open").await;
}

#[tokio::test]
async fn forking_saved_none_uses_model_default_without_changing_source() {
    check_recovery("fork").await;
}

#[tokio::test]
async fn reloading_supported_efforts_corrects_the_next_responses_request() {
    check_recovery("reload").await;
}
