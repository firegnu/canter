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
            agent_type: AgentType::General,
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
    let tools = requests[1]["tools"].as_array().unwrap();
    assert!(
        !tools
            .iter()
            .any(|tool| tool["function"]["name"] == "ask_user")
    );
    for name in ["spawn_agent", "wait_agent", "send_input", "close_agent"] {
        assert!(!tools.iter().any(|tool| tool["function"]["name"] == name));
    }
    let send_input = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "send_input")
        .unwrap();
    assert!(send_input["function"]["description"].as_str().unwrap()
        .contains("In canter exec, first use wait_agent until the running sub-agent stops, then send_input."));
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
    assert!(
        fake.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("You are a sub-agent")
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

#[tokio::test]
async fn plan_mode_and_review_allow_only_explorers() {
    for review in [false, true] {
        let test = Fixture::new(vec![
            Reply {
                hang: true,
                ..Reply::sse("")
            },
            complete("Read-only findings"),
        ])
        .await;
        if review {
            let mut params = input("Review changes");
            params.review = Some("Review the current changes".into());
            test.parent.input(params).await.unwrap();
        } else {
            test.parent
                .update(serde_json::from_value(json!({"planMode":true})).unwrap())
                .await
                .unwrap();
        }
        assert!(
            test.parent
                .spawn_agent("general".into(), "Change code".into())
                .await
                .is_err()
        );
        let (child, _) = test
            .parent
            .spawn_agent_of_type(
                "explorer".into(),
                "Inspect code".into(),
                AgentType::Explorer,
                None,
            )
            .await
            .unwrap();
        test.stop(&[child]).await;
    }
}

#[tokio::test]
async fn general_child_cannot_access_memories_and_has_its_own_scratch_directory() {
    let test = Fixture::new(vec![
        tool("read", json!({"path":"../state/memory/private.md"})),
        tool(
            "write",
            json!({"path":"../state/memory/private.md","content":"replacement"}),
        ),
        complete("Finished without memories"),
    ])
    .await;
    let memory = test.root.path().join("state/memory/private.md");
    std::fs::create_dir_all(memory.parent().unwrap()).unwrap();
    std::fs::write(&memory, "hidden-memory-content").unwrap();
    std::fs::write(
        test.root.path().join("work/AGENTS.md"),
        "Project instruction for the child",
    )
    .unwrap();
    let (child, _) = test
        .parent
        .spawn_agent("child".into(), "Inspect code".into())
        .await
        .unwrap();
    idle(&child).await;
    let requests = test.fake.requests.lock().unwrap().clone();
    let messages = requests.last().unwrap()["messages"].to_string();
    assert!(!messages.contains("hidden-memory-content"));
    assert!(messages.contains("this file is private to canter"));
    assert!(messages.contains("Project instruction for the child"));
    assert_eq!(
        std::fs::read_to_string(memory).unwrap(),
        "hidden-memory-content"
    );
    let scratch = |request: &serde_json::Value| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .flat_map(str::lines)
            .find(|line| line.starts_with("Scratch directory:"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(scratch(&requests[0]), scratch(&requests[1]));
    test.stop(&[child]).await;
}

#[tokio::test]
async fn exec_child_automatically_declines_approval_without_waiting_for_a_client() {
    let test = Fixture::new(vec![
        Reply {
            hang: true,
            ..Reply::sse("")
        },
        shell("touch unapproved"),
        complete("Approval declined; continued"),
    ])
    .await;
    test.parent
        .update(serde_json::from_value(json!({"approvalPolicy":"alwaysAsk"})).unwrap())
        .await
        .unwrap();
    let mut params = input("Run in exec");
    params.non_interactive = true;
    test.parent.input(params).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.fake.arrivals.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (child, _) = test
        .parent
        .spawn_agent("child".into(), "Work with exec policy".into())
        .await
        .unwrap();
    let result = idle(&child).await;
    assert!(result.pending_requests.is_empty());
    assert_eq!(
        result.agent.unwrap().result.as_deref(),
        Some("Approval declined; continued")
    );
    assert!(!test.root.path().join("work/unapproved").exists());
    test.stop(&[child]).await;
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
                agent_type: AgentType::General,
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

fn tool(name: &str, args: serde_json::Value) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call-{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
    ))
}

struct Fixture {
    root: tempfile::TempDir,
    parent: Session,
    fake: Server,
}
impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        Self::with_input(replies, false).await
    }
    async fn with_input(replies: Vec<Reply>, non_interactive: bool) -> Self {
        Self::with_options(replies, non_interactive, CoreOptions::default()).await
    }
    async fn with_options(
        replies: Vec<Reply>,
        non_interactive: bool,
        options: CoreOptions,
    ) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        for dir in ["home", "work", "state"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        let fake = Server::start(
            std::iter::once(complete("Parent ready"))
                .chain(replies)
                .collect(),
        )
        .await;
        let model = Arc::new(ChatCompletions::new(&fake.base_url, ApiKey::new(KEY)).unwrap());
        let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
        let runtime = RuntimeContext {
            memory_home: Some(root.path().join("state")),
            protected_paths: vec![root.path().join("state")],
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
            ]
            .into(),
            ..Default::default()
        };
        let settings = serde_json::from_value(json!({"cwd":root.path().join("work"),"model":"fake-model","name":null,"approvalPolicy":"allowAll"})).unwrap();
        let (parent, _) =
            Session::create(store, "parent".into(), settings, runtime, model, options)
                .await
                .unwrap();
        let mut params = input("Parent task");
        params.non_interactive = non_interactive;
        parent.input(params).await.unwrap();
        idle(&parent).await;
        Self { root, parent, fake }
    }
    async fn stop(self, children: &[Session]) {
        for child in children {
            child.close().await.unwrap();
        }
        self.parent.close().await.unwrap();
        self.fake.stop().await;
    }
}

#[tokio::test]
async fn explorer_rejects_writes_and_nested_agents_but_keeps_its_own_checklist() {
    let test = Fixture::new(vec![
        tool("write", json!({"path":"forbidden.txt","content":"no"})),
        shell("touch forbidden-shell.txt"),
        tool("update_plan", json!({"plan":[{"step":"Inspect parser","status":"completed"}]})),
        tool("spawn_agent", json!({"task":"Nested","type":"general"})),
        tool("ask_user", json!({"question":"Continue?","options":[{"label":"Yes","description":"Proceed"},{"label":"No","description":"Stop"}]})),
        complete("Inspection complete"),
    ]).await;
    let (child, _) = test
        .parent
        .spawn_agent_of_type(
            "explorer".into(),
            "Inspect parser".into(),
            AgentType::Explorer,
            None,
        )
        .await
        .unwrap();
    let done = idle(&child).await;
    assert!(!test.root.path().join("work/forbidden.txt").exists());
    assert!(!test.root.path().join("work/forbidden-shell.txt").exists());
    assert_eq!(done.current_plan().unwrap()[0].step, "Inspect parser");
    assert!(
        test.parent
            .snapshot()
            .await
            .session
            .current_plan()
            .is_none()
    );
    let requests = test.fake.requests.lock().unwrap().clone();
    let names: Vec<_> = requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    for forbidden in [
        "write",
        "edit",
        "ask_user",
        "submit_plan",
        "mcp_call",
        "spawn_agent",
        "wait_agent",
        "send_input",
        "close_agent",
    ] {
        assert!(!names.contains(&forbidden), "{forbidden}");
    }
    assert!(names.contains(&"update_plan"));
    assert!(
        !requests[1]["messages"]
            .to_string()
            .contains("memory_instructions")
    );
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("You are a sub-agent")
    );
    assert!(
        child
            .spawn_agent("nested".into(), "Do work".into())
            .await
            .is_err()
    );
    test.stop(&[child]).await;
}

#[tokio::test]
async fn child_uses_the_parents_new_permission_policy_on_its_next_check() {
    let test = Fixture::new(vec![
        complete("First task done"),
        shell("touch needs-approval"),
        complete("Second task done"),
    ])
    .await;
    let (child, _) = test
        .parent
        .spawn_agent("child".into(), "First task".into())
        .await
        .unwrap();
    idle(&child).await;
    test.parent
        .update(serde_json::from_value(json!({"approvalPolicy":"alwaysAsk"})).unwrap())
        .await
        .unwrap();
    child.input(input("Next task")).await.unwrap();
    let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = child.snapshot().await.session;
            if !snapshot.pending_requests.is_empty() || snapshot.status == SessionStatus::Idle {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        snapshot.pending_requests.len(),
        1,
        "the next check must use the parent's current policy"
    );
    assert!(!test.root.path().join("work/needs-approval").exists());
    test.stop(&[child]).await;
}

#[tokio::test]
async fn child_completion_wakes_interactive_parent_but_exec_waits_for_next_input() {
    for non_interactive in [false, true] {
        let test = Fixture::with_input(
            vec![
                complete("Parser findings"),
                complete("Parent received findings"),
            ],
            non_interactive,
        )
        .await;
        let (child, _) = test
            .parent
            .spawn_agent("child".into(), "Inspect parser".into())
            .await
            .unwrap();
        idle(&child).await;
        if non_interactive {
            tokio::time::sleep(Duration::from_millis(2200)).await;
            assert_eq!(
                test.parent.snapshot().await.session.turns.len(),
                1,
                "exec must not wake automatically"
            );
            test.parent.input(input("Use the result")).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = test.parent.snapshot().await.session;
                if snapshot.turns.len() == 2 && snapshot.status == SessionStatus::Idle {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("interactive parents should wake; exec should deliver on next input");
        let requests = test.fake.requests.lock().unwrap().clone();
        let delivered = requests.last().unwrap()["messages"].to_string();
        assert!(delivered.contains("Parser findings"));
        assert!(delivered.contains("Inspect parser"));
        assert!(delivered.contains("completed"));
        test.stop(&[child]).await;
    }
}

#[tokio::test]
async fn running_general_child_follows_parent_plan_mode_on_every_check() {
    running_child_follows_readonly_parent(false).await;
}

#[tokio::test]
async fn running_general_child_follows_parent_review_on_every_check() {
    running_child_follows_readonly_parent(true).await;
}

async fn running_child_follows_readonly_parent(review: bool) {
    let mut replies = vec![tool("write", json!({"path":"pending.txt","content":"no"}))];
    if review {
        replies.push(Reply {
            hang: true,
            ..Reply::sse("")
        });
    }
    replies.extend([
        tool("write", json!({"path":"next.txt","content":"no"})),
        shell("touch shell-write.txt"),
        tool("read", json!({"path":"readable.txt"})),
        tool(
            "update_plan",
            json!({"plan":[{"step":"Inspect only","status":"completed"}]}),
        ),
        complete("Read-only work done"),
    ]);
    let test = Fixture::new(replies).await;
    std::fs::write(
        test.root.path().join("work/readable.txt"),
        "read-only evidence",
    )
    .unwrap();
    test.parent
        .update(serde_json::from_value(json!({"approvalPolicy":"alwaysAsk"})).unwrap())
        .await
        .unwrap();
    let (child, _) = test
        .parent
        .spawn_agent("child".into(), "Work on files".into())
        .await
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(request) = child.snapshot().await.session.pending_requests.first() {
                break request.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    test.parent
        .update(serde_json::from_value(json!({"approvalPolicy":"allowAll"})).unwrap())
        .await
        .unwrap();
    if review {
        let mut params = input("Review changes");
        params.review = Some("Review the current changes".into());
        test.parent.input(params).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while test.fake.requests.lock().unwrap().len() < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    } else {
        test.parent
            .update(serde_json::from_value(json!({"planMode":true})).unwrap())
            .await
            .unwrap();
    }
    child
        .resolve(
            &request.id,
            serde_json::from_value(json!({"decision":"accept","clientName":"test"})).unwrap(),
        )
        .await
        .unwrap();
    let done = idle(&child).await;
    let changed: Vec<_> = ["pending.txt", "next.txt", "shell-write.txt"]
        .into_iter()
        .filter(|path| test.root.path().join("work").join(path).exists())
        .collect();
    assert!(
        changed.is_empty(),
        "parent read-only mode must reject pending approvals and subsequent writes: {changed:?}"
    );
    assert_eq!(done.current_plan().unwrap()[0].step, "Inspect only");
    assert!(
        test.fake.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("read-only evidence")
    );
    test.stop(&[child]).await;
}

#[tokio::test]
async fn interrupting_parent_interrupts_running_children_without_waking_it() {
    let hang = || Reply {
        hang: true,
        ..Reply::sse("")
    };
    let test = Fixture::new(vec![hang(), hang()]).await;
    let (child, _) = test
        .parent
        .spawn_agent("child".into(), "Long task".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.fake.arrivals.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    test.parent.input(input("Keep working")).await.unwrap();
    let turn_id = test
        .parent
        .snapshot()
        .await
        .session
        .turns
        .last()
        .unwrap()
        .id
        .clone();
    test.parent
        .interrupt(SessionInterruptParams {
            turn_id,
            retract: None,
        })
        .await
        .unwrap();
    assert_eq!(
        child.snapshot().await.session.agent.unwrap().status,
        AgentStatus::Interrupted
    );
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(test.parent.snapshot().await.session.turns.len(), 2);
    assert_eq!(
        test.parent.snapshot().await.session.status,
        SessionStatus::Idle
    );
    test.stop(&[child]).await;
}

#[tokio::test]
async fn child_spending_reaches_parent_goal_budget_and_stops_siblings() {
    let charged = Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"Charged result"},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":1,"total_tokens":1}})
    ));
    let options = CoreOptions {
        prices: serde_json::from_value(
            json!({"fake-model":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}),
        )
        .unwrap(),
        ..Default::default()
    };
    let hang = || Reply {
        hang: true,
        ..Reply::sse("")
    };
    let test = Fixture::with_options(vec![hang(), hang(), charged], false, options).await;
    test.parent
        .set_goal(GoalSetParams {
            objective: "Complete parent and child work".into(),
            budget_usd: Some(1.0),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.fake.arrivals.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (waiting, _) = test
        .parent
        .spawn_agent("waiting".into(), "Wait for result".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.fake.arrivals.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (charged, _) = test
        .parent
        .spawn_agent("charged".into(), "Produce result".into())
        .await
        .unwrap();
    let goal = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let goal = test.parent.goal().await.goal.unwrap();
            if goal.status == GoalStatus::BudgetLimited {
                break goal;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("child usage must count toward the parent's budget");
    assert_eq!(goal.cost.usd, 1.0);
    assert!(!goal.cost.unknown);
    assert_eq!(
        idle(&waiting).await.agent.unwrap().status,
        AgentStatus::Interrupted
    );
    test.stop(&[waiting, charged]).await;
}

#[test]
fn recovery_skips_a_locked_child_and_still_recovers_other_running_children() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = Store::new(root.path()).unwrap();
    let settings = SessionSnapshot::empty("parent").settings;
    let source = SessionParent {
        agent_type: AgentType::General,
        session_id: "parent".into(),
        turn_id: "p1".into(),
        task: "Work".into(),
    };
    let locked = store
        .create_child("locked", settings.clone(), source.clone(), 1)
        .unwrap();
    drop(
        store
            .create_child("recoverable", settings, source, 1)
            .unwrap(),
    );
    assert!(
        canter_core::recover_agent_sessions(&store).is_ok(),
        "one failed open must not block startup"
    );
    assert_eq!(
        store
            .open("recoverable")
            .unwrap()
            .snapshot()
            .agent
            .as_ref()
            .unwrap()
            .status,
        AgentStatus::Interrupted
    );
    drop(locked);
    canter_core::recover_agent_sessions(&store).unwrap();
    assert_eq!(
        store
            .open("locked")
            .unwrap()
            .snapshot()
            .agent
            .as_ref()
            .unwrap()
            .status,
        AgentStatus::Interrupted
    );
}

#[tokio::test]
async fn explorer_from_plan_mode_uses_its_checklist_without_the_parent_plan_workflow() {
    let test = Fixture::new(vec![
        tool(
            "update_plan",
            json!({"plan":[{"step":"Inspect parser","status":"completed"}]}),
        ),
        complete("Read-only findings"),
        complete("Summary of read-only findings"),
        complete("Continued findings"),
    ])
    .await;
    test.parent
        .update(serde_json::from_value(json!({"planMode":true})).unwrap())
        .await
        .unwrap();
    let (child, _) = test
        .parent
        .spawn_agent_of_type(
            "explorer".into(),
            "Inspect parser".into(),
            AgentType::Explorer,
            None,
        )
        .await
        .unwrap();
    let result = idle(&child).await;
    assert_eq!(result.current_plan().unwrap()[0].step, "Inspect parser");
    child.compact().await.unwrap();
    idle(&child).await;
    child.input(input("Continue inspecting")).await.unwrap();
    idle(&child).await;
    let requests = test.fake.requests.lock().unwrap().clone();
    assert!(
        !requests.last().unwrap()["messages"]
            .to_string()
            .contains("[Plan mode enabled]"),
        "the explorer must not be instructed to ask_user, avoid update_plan or submit_plan after compaction"
    );
    test.stop(&[child]).await;
}
