#[path = "../../canter-model/tests/support/mod.rs"]
#[allow(dead_code)]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription, stats, usage};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn complete(text: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105}
        })
    ))
}

fn input(text: &str) -> SessionInputParams {
    serde_json::from_value(
        json!({"input":[{"type":"text","text":text}],"clientName":"test","mode":"queue"}),
    )
    .unwrap()
}

async fn finished(events: &mut Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(event.payload, EventPayload::TurnCompleted { .. }) {
                return;
            }
        }
    })
    .await
    .unwrap();
}

async fn idle(session: &Session) -> SessionSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = session.snapshot().await.session;
            if snapshot.status == SessionStatus::Idle {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    options: CoreOptions,
    fake: Server,
}

impl Fixture {
    async fn new(replies: Vec<Reply>, model: &str, prompt_suggestions: bool) -> Self {
        Self::with_options(replies, model, prompt_suggestions, CoreOptions::default()).await
    }

    async fn with_options(
        replies: Vec<Reply>,
        model: &str,
        prompt_suggestions: bool,
        mut options: CoreOptions,
    ) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let fake = Server::start(replies).await;
        let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
        let client = Arc::new(ChatCompletions::new(&fake.base_url, ApiKey::new(KEY)).unwrap());
        options.prompt_suggestions = prompt_suggestions;
        options.chatgpt_plan_providers.insert("subscription".into());
        let settings = serde_json::from_value(
            json!({"cwd":root.path(),"model":model,"approvalPolicy":"allowAll"}),
        )
        .unwrap();
        let (session, _) = Session::create(
            store.clone(),
            "plan".into(),
            settings,
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
            client,
            options.clone(),
        )
        .await
        .unwrap();
        Self {
            _root: root,
            store,
            session,
            options,
            fake,
        }
    }

    fn usages(&self, id: &str, expected: usize) -> Vec<TokenUsage> {
        let usages: Vec<_> = self
            .store
            .events_after(id, 0)
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.payload {
                EventPayload::SessionUsage { token_usage }
                | EventPayload::SessionBtwUsage { token_usage } => Some(token_usage),
                _ => None,
            })
            .collect();
        assert_eq!(usages.len(), expected);
        for usage in &usages {
            let request = usage.request.as_ref().unwrap();
            assert_eq!(request.billing_kind, Some(BillingKind::ChatgptPlan));
            assert_eq!(request.cost_usd, None);
            let cost = usage.cost.as_ref().unwrap();
            assert_eq!(cost.usd, 0.0);
            assert!(!cost.unknown);
            assert!(cost.chatgpt_plan);
            assert!(usage.last.total_tokens > 0);
        }
        usages
    }

    async fn close(self, children: &[Session]) {
        for child in children {
            child.close().await.unwrap();
        }
        self.session.close().await.unwrap();
        self.fake.stop().await;
    }
}

#[tokio::test]
async fn ordinary_compaction_btw_handoff_and_subagent_requests_use_plan_billing() {
    let fixture = Fixture::new(
        vec![
            complete("Parent answer"),
            complete("Summary"),
            complete("Side answer"),
            complete("Handoff note"),
            complete("Child answer"),
        ],
        "subscription/fake",
        false,
    )
    .await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture.session.input(input("Parent task")).await.unwrap();
    finished(&mut events).await;
    fixture.session.compact().await.unwrap();
    finished(&mut events).await;
    let mut btw = fixture.session.btw("Side question".into()).await.unwrap();
    while btw.recv().await.is_some() {}
    let mut handoff = fixture.session.handoff(None).await.unwrap();
    while handoff.recv().await.is_some() {}
    let (child, _) = fixture
        .session
        .spawn_agent("child".into(), "Child task".into())
        .await
        .unwrap();
    idle(&child).await;

    fixture.usages("plan", 4);
    fixture.usages("child", 1);
    fixture.close(&[child]).await;
}

#[tokio::test]
async fn next_prompt_suggestion_uses_plan_billing_without_changing_turn_usage() {
    let fixture = Fixture::new(
        vec![complete("Parent answer"), complete("Next suggestion")],
        "subscription/fake",
        true,
    )
    .await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture.session.input(input("Parent task")).await.unwrap();
    finished(&mut events).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(event.payload, EventPayload::SessionPromptSuggestion { .. }) {
                return;
            }
        }
    })
    .await
    .unwrap();

    fixture.usages("plan", 2);
    let snapshot = fixture.session.snapshot().await.session;
    assert_eq!(
        snapshot.turns[0].usage.as_ref().unwrap().total.total_tokens,
        105
    );
    assert_eq!(snapshot.token_usage.unwrap().total.total_tokens, 210);
    fixture.close(&[]).await;
}

#[tokio::test]
async fn plan_subagent_and_metered_parent_keep_separate_costs_in_stats() {
    let options = CoreOptions {
        prices: serde_json::from_value(
            json!({"metered/fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":24000}}}),
        )
        .unwrap(),
        ..Default::default()
    };
    let fixture = Fixture::with_options(
        vec![complete("Metered parent"), complete("Plan child")],
        "metered/fake",
        false,
        options,
    )
    .await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture.session.input(input("Parent task")).await.unwrap();
    finished(&mut events).await;
    fixture
        .session
        .update(serde_json::from_value(json!({"model":"subscription/fake"})).unwrap())
        .await
        .unwrap();
    let (child, _) = fixture
        .session
        .spawn_agent("child".into(), "Child task".into())
        .await
        .unwrap();
    idle(&child).await;

    let report = stats::report(&fixture.store, "plan", &fixture.options.prices).unwrap();
    let parent = report.overview.usage.as_ref().unwrap();
    assert!((parent.total.cost.usd - 0.12).abs() < 1e-12);
    assert!(!parent.total.cost.unknown);
    assert!(!parent.total.cost.chatgpt_plan);
    let child_usage = report.sub_agents[0].usage.as_ref().unwrap();
    assert_eq!(child_usage.cost.usd, 0.0);
    assert!(!child_usage.cost.unknown);
    assert!(child_usage.cost.chatgpt_plan);
    let combined = report.overview.usage_with_sub_agents.unwrap();
    assert!((combined.cost.usd - 0.12).abs() < 1e-12);
    assert!(!combined.cost.unknown);
    assert!(combined.cost.chatgpt_plan);
    assert_eq!(combined.output_tokens, 10);

    let events = fixture.store.events_after("plan", 0).unwrap();
    let ledger = usage::Ledger::replay(&events, &fixture.options.prices);
    assert!(!ledger.report.total.cost.chatgpt_plan);
    fixture.close(&[child]).await;
}
