#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn complete(text: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    ))
}
fn shell(command: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"shell-call","type":"function","function":{"name":"shell","arguments":json!({"command":command}).to_string()}}]},"finish_reason":"tool_calls"}]})
    ))
}
fn input(text: &str) -> SessionInputParams {
    serde_json::from_value(
        json!({"input":[{"type":"text","text":text}],"clientName":"test","mode":"queue"}),
    )
    .unwrap()
}
async fn idle(session: &Session) -> SessionSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = session.snapshot().await.session;
            if snapshot.status == SessionStatus::Idle || snapshot.status == SessionStatus::Error {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn spawn_inherits_current_settings_runs_fresh_and_can_continue_and_reopen() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    for name in ["home", "work", "extra", "state"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let fake = Server::start(vec![
        complete("Parent answer"),
        shell("printf %s \"$CHILD_TEST_ENV\" > inherited-env"),
        complete("Child result"),
        shell("touch should-not-exist"),
        complete("Followup result"),
    ])
    .await;
    let model = Arc::new(ChatCompletions::new(&fake.base_url, ApiKey::new(KEY)).unwrap());
    let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
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
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("CHILD_TEST_ENV".into(), "in-memory-value".into()),
        ]
        .into(),
        ..Default::default()
    };
    let settings = serde_json::from_value(json!({"cwd":root.path().join("work"),"model":"fake-model","name":null,"approvalPolicy":"alwaysAsk"})).unwrap();
    let options = CoreOptions {
        sandbox: cfg!(target_os = "macos"),
        command_rules_path: Some(root.path().join("state/rules.json")),
        ..Default::default()
    };
    let (parent, _) = Session::create(
        store.clone(),
        "parent".into(),
        settings,
        runtime.clone(),
        model.clone(),
        options.clone(),
    )
    .await
    .unwrap();
    parent
        .input(input("Parent private conversation"))
        .await
        .unwrap();
    let before = idle(&parent).await;
    parent.update(serde_json::from_value(json!({"model":"changed-model","reasoningEffort":"low","approvalPolicy":"allowAll","addDirectories":[root.path().join("extra")]})).unwrap()).await.unwrap();
    let (child, created) = parent
        .spawn_agent("child".into(), "Inspect parser".into())
        .await
        .unwrap();
    assert_eq!(
        created.session.parent.as_ref().unwrap(),
        &SessionParent {
            session_id: "parent".into(),
            turn_id: before.turns[0].id.clone(),
            task: "Inspect parser".into()
        }
    );
    assert_eq!(created.session.settings.model, "changed-model");
    assert_eq!(
        created.session.settings.reasoning_effort,
        ReasoningEffort::Low
    );
    assert_eq!(
        created.session.settings.approval_policy,
        PermissionMode::AllowAll
    );
    assert_eq!(created.session.settings.cwd, before.settings.cwd);
    assert_eq!(
        created.session.settings.sandbox_enabled,
        Some(options.sandbox)
    );
    assert_eq!(created.session.settings.additional_directories.len(), 1);
    let done = idle(&child).await;
    assert_eq!(
        done.agent.as_ref().unwrap().result.as_deref(),
        Some("Child result")
    );
    assert_eq!(done.turns.len(), 1);
    assert!(done.turns[0].input == input("Inspect parser").input);
    let requests = fake.requests.lock().unwrap().clone();
    assert_eq!(fake.arrivals.lock().unwrap().len(), 3);
    assert!(
        !requests[1]
            .to_string()
            .contains("Parent private conversation")
    );
    assert_eq!(requests[1]["model"], "changed-model");
    assert_eq!(
        std::fs::read_to_string(root.path().join("work/inherited-env")).unwrap(),
        "in-memory-value"
    );
    std::fs::write(
        root.path().join("state/rules.json"),
        r#"{"rules":[{"pattern":["touch"],"decision":"forbidden"}]}"#,
    )
    .unwrap();
    child.input(input("Check again")).await.unwrap();
    let done = idle(&child).await;
    assert_eq!(
        done.agent.as_ref().unwrap().result.as_deref(),
        Some("Followup result")
    );
    assert!(!root.path().join("work/should-not-exist").exists());
    assert!(
        fake.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("forbidden")
    );
    child.close_for_shutdown().await.unwrap();
    let (reopened, saved) = Session::open(store.clone(), "child".into(), runtime, model, options)
        .await
        .unwrap();
    assert_eq!(saved.session.agent, done.agent);
    assert_eq!(saved.session.parent, done.parent);
    let events =
        std::fs::read_to_string(root.path().join("state/sessions/child/events.jsonl")).unwrap();
    assert!(!events.contains("in-memory-value"));
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        canter_core::stats::list(&store, true).unwrap().data.len(),
        1
    );
    reopened.close().await.unwrap();
    parent.close().await.unwrap();
    fake.stop().await;
}

#[test]
fn restart_marks_unfinished_children_interrupted_without_loading_a_model() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::new(root.path()).unwrap();
    let settings = SessionSnapshot::empty("parent").settings;
    let mut ordinary = store.create("parent", settings.clone(), 1).unwrap();
    let turn: Turn = serde_json::from_value(json!({"id":"t1","status":"inProgress","input":[],"clientName":"test","items":[],"error":null,"usage":null})).unwrap();
    ordinary
        .append(
            EventPayload::TurnStarted {
                turn: turn.clone(),
                input_id: None,
            },
            2,
        )
        .unwrap();
    drop(ordinary);
    let mut child = store
        .create_child(
            "child",
            settings,
            SessionParent {
                session_id: "parent".into(),
                turn_id: "t1".into(),
                task: "Unfinished task".into(),
            },
            3,
        )
        .unwrap();
    child
        .append(
            EventPayload::TurnStarted {
                turn,
                input_id: None,
            },
            4,
        )
        .unwrap();
    drop(child);
    canter_core::recover_agent_sessions(&store).unwrap();
    let child = store.open("child").unwrap();
    assert_eq!(
        child.snapshot().agent.as_ref().unwrap().status,
        AgentStatus::Interrupted
    );
    assert_eq!(child.snapshot().turns[0].status, TurnStatus::Interrupted);
    let snapshot = child.snapshot().clone();
    drop(child);
    canter_core::recover_agent_sessions(&store).unwrap();
    assert_eq!(store.open("child").unwrap().snapshot(), &snapshot);
    assert_eq!(
        store.open("parent").unwrap().snapshot().turns[0].status,
        TurnStatus::InProgress
    );
}
