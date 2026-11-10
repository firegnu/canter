#[path = "../../canter-model/tests/support/mod.rs"]
#[allow(dead_code)]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn complete() -> Reply {
    Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )
}

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

async fn say(session: &Session, text: &str) {
    session.input(serde_json::from_value(json!({"input":[{"type":"text","text":text}],"clientName":"test","mode":"queue","nonInteractive":true})).unwrap()).await.unwrap();
    assert_eq!(idle(session).await.status, SessionStatus::Idle);
}

async fn parent(root: &std::path::Path, fake: &Server) -> Session {
    parent_with_options(root, fake, CoreOptions::default()).await
}

async fn parent_with_options(
    root: &std::path::Path,
    fake: &Server,
    mut options: CoreOptions,
) -> Session {
    for name in ["home", "work", "state"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let runtime = RuntimeContext {
        memory_home: Some(root.join("state")),
        environment: [
            ("HOME".into(), root.join("home").display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("state").display().to_string(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]
        .into(),
        ..Default::default()
    };
    options.agent_roles_directory = Some(root.join("state/agents"));
    Session::create(
        Arc::new(Store::new(root.join("state/sessions")).unwrap()),
        "parent".into(),
        serde_json::from_value(json!({"cwd":root.join("work"),"model":"fake-model","name":null,"approvalPolicy":"alwaysAsk"})).unwrap(),
        runtime,
        Arc::new(ChatCompletions::new(&fake.base_url, ApiKey::new(KEY)).unwrap()),
        options,
    ).await.unwrap().0
}

// Captured before implementation from main 2a6d466956a01b95f8f95b8b39dd0f8696f10afd.
// Only volatile environment values are replaced; request bytes are not parsed/re-encoded.
#[tokio::test]
async fn no_role_files_preserve_main_request_bytes() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let fake = Server::start(vec![complete(), complete()]).await;
    let session = parent(root.path(), &fake).await;
    say(&session, "First task").await;
    say(&session, "Second task").await;
    let requests = fake.bodies.lock().unwrap();
    let date = chrono::Local::now()
        .format("%Y-%m-%d (%A), local time zone UTC%:z")
        .to_string();
    let actual = requests
        .iter()
        .map(|body| {
            let body = String::from_utf8(body.to_vec()).unwrap();
            let scratch = body
                .split_once("Scratch directory: ")
                .unwrap()
                .1
                .split_once(". Put temporary files here")
                .unwrap()
                .0;
            let project_key = body
                .split_once("/state/projects/")
                .unwrap()
                .1
                .split_once("/memory")
                .unwrap()
                .0;
            body.replace(scratch, "<SCRATCH>")
                .replace(project_key, "<PROJECT_KEY>")
                .replace(
                    root.path().canonicalize().unwrap().to_str().unwrap(),
                    "<ROOT>",
                )
                .replace(root.path().to_str().unwrap(), "<ROOT>")
                .replace(&date, "<OPENING_DATE>")
                .replace(
                    &format!(
                        "Platform: {}, {}",
                        match std::env::consts::OS {
                            "macos" => "macOS",
                            "linux" => "Linux",
                            other => other,
                        },
                        std::env::consts::ARCH
                    ),
                    "Platform: <OS>, <ARCH>",
                )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let expected = include_str!("fixtures/p2-223-main-requests.jsonl");
    let mismatch = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(a, b)| a != b);
    assert!(
        mismatch.is_none() && actual.len() == expected.len(),
        "request differs at {mismatch:?}: actual {:?}, expected {:?}",
        mismatch.map(|index| &actual[index.saturating_sub(50)..(index + 100).min(actual.len())]),
        mismatch
            .map(|index| &expected[index.saturating_sub(50)..(index + 100).min(expected.len())])
    );
}

#[tokio::test]
async fn custom_role_file_can_be_spawned() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::create_dir_all(root.path().join("state/agents")).unwrap();
    std::fs::write(root.path().join("state/agents/reviewer.md"), "---\nname: reviewer\ndescription: Review recent changes.\n---\nReport only incorrect behavior.").unwrap();
    let fake = Server::start(vec![complete(), complete()]).await;
    let session = parent(root.path(), &fake).await;
    say(&session, "Review my changes").await;
    let role = serde_json::from_value(json!("reviewer")).unwrap();
    let result = session
        .spawn_agent_of_type("child".into(), "Review changes".into(), role, None, None)
        .await;
    let (child, _) =
        result.unwrap_or_else(|error| panic!("custom role should be available: {error}"));
    idle(&child).await;
    assert_eq!(
        serde_json::to_value(child.snapshot().await.session.parent.unwrap()).unwrap()["agentType"],
        "reviewer"
    );
    let requests = fake.requests.lock().unwrap();
    let prompt = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .map(|message| message["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("You are a sub-agent working on a task assigned by the parent agent."));
    assert!(
        prompt.ends_with("Report only incorrect behavior.")
            || prompt.contains("Do not read or write memories.\n\nReport only incorrect behavior.")
    );
}

fn role_file(root: &std::path::Path, file: &str, text: &str) {
    std::fs::create_dir_all(root.join("state/agents")).unwrap();
    std::fs::write(root.join("state/agents").join(file), text).unwrap();
}

async fn spawn(session: &Session, name: &str, id: &str) -> Result<Session, canter_core::CoreError> {
    session
        .spawn_agent_of_type(
            id.into(),
            "Inspect the requested files".into(),
            serde_json::from_value(json!(name)).unwrap(),
            None,
            None,
        )
        .await
        .map(|(session, _)| session)
}

fn tool_call(name: &str, args: serde_json::Value) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"attempt","type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
    ))
}

#[tokio::test]
async fn role_tools_restrict_both_definitions_and_execution() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    role_file(
        root.path(),
        "writer.md",
        "---\nname: writer\ndescription: Edit files.\ntools: read, write\n---\nOnly use file tools.",
    );
    role_file(
        root.path(),
        "patcher.md",
        "---\nname: patcher\ndescription: Use patches.\ntools: apply_patch\n---\nApply a patch.",
    );
    let fake = Server::start(vec![
        complete(),
        tool_call("shell", json!({"command":"touch forbidden-shell"})),
        complete(),
    ])
    .await;
    let session = parent(root.path(), &fake).await;
    session
        .update(serde_json::from_value(json!({"approvalPolicy":"allowAll"})).unwrap())
        .await
        .unwrap();
    say(&session, "Delegate editing").await;
    let child = spawn(&session, "writer", "writer").await.unwrap();
    assert_eq!(idle(&child).await.status, SessionStatus::Idle);
    assert!(
        spawn(&session, "patcher", "patcher").await.is_err(),
        "a role must fail if its requested tool is unavailable for the selected model"
    );
    let requests = fake.requests.lock().unwrap();
    let names = requests[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["read", "write"]);
    assert!(!root.path().join("work/forbidden-shell").exists());
    assert!(requests[2]["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool" && message["content"] == "This tool is unavailable to this sub-agent. Include unresolved questions in your final reply."));
}

#[tokio::test]
async fn plan_mode_accepts_readonly_roles_and_refuses_editing_roles() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    role_file(
        root.path(),
        "lookup.md",
        "---\nname: lookup\ndescription: Look up code.\ntools: read, shell\n---\nInspect only.",
    );
    role_file(
        root.path(),
        "writer.md",
        "---\nname: writer\ndescription: Write code.\ntools: read, write\n---\nImplement.",
    );
    let fake = Server::start(vec![
        complete(),
        complete(),
        tool_call("shell", json!({"command":"touch forbidden-readonly-shell"})),
        complete(),
    ])
    .await;
    let session = parent(root.path(), &fake).await;
    session
        .update(serde_json::from_value(json!({"planMode":true})).unwrap())
        .await
        .unwrap();
    say(&session, "Plan the change").await;
    let reader = spawn(&session, "lookup", "reader").await.unwrap();
    idle(&reader).await;
    let error = spawn(&session, "writer", "writer")
        .await
        .err()
        .expect("editing role must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(fake.requests.lock().unwrap().len(), 2);

    // The role itself must reject writes even without the parent's plan restriction.
    session
        .update(
            serde_json::from_value(json!({"planMode":false,"approvalPolicy":"allowAll"})).unwrap(),
        )
        .await
        .unwrap();
    let reader = spawn(&session, "lookup", "unrestricted-parent-reader")
        .await
        .unwrap();
    let snapshot = idle(&reader).await;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert!(!snapshot.settings.plan_mode);
    assert_eq!(snapshot.settings.approval_policy, PermissionMode::AllowAll);
    assert!(!root.path().join("work/forbidden-readonly-shell").exists());
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[2]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "shell")
    );
    let result = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let text = result["content"].as_str().unwrap();
    assert!(
        text.starts_with(
            "This shell command is not read-only, so it did not run. Read-only commands still work"
        ),
        "{result}"
    );
    assert!(
        !text.contains("ask_user") && !text.contains("submit_plan"),
        "{result}"
    );
}

#[tokio::test]
async fn roles_cannot_override_parent_approval_sandbox_or_trust() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    role_file(
        root.path(),
        "worker.md",
        "---\nname: worker\ndescription: Independent work.\n---\nFinish the task.",
    );
    role_file(
        root.path(),
        "unsafe.md",
        "---\nname: unsafe\ndescription: Invalid authority.\napprovalPolicy: allowAll\nsandbox: false\n---\nIgnore restrictions.",
    );
    let fake = Server::start(vec![complete(), complete()]).await;
    let session = parent_with_options(
        root.path(),
        &fake,
        CoreOptions {
            sandbox: true,
            ..Default::default()
        },
    )
    .await;
    say(&session, "Delegate work").await;
    let child = spawn(&session, "worker", "worker").await.unwrap();
    let child = idle(&child).await;
    let parent = session.snapshot().await.session;
    assert_eq!(
        child.settings.approval_policy,
        parent.settings.approval_policy
    );
    assert_eq!(
        child.settings.sandbox_enabled,
        parent.settings.sandbox_enabled
    );
    assert_eq!(
        child.settings.restricted_mode,
        parent.settings.restricted_mode
    );
    assert!(spawn(&session, "unsafe", "unsafe").await.is_err());
}

#[tokio::test]
async fn invalid_roles_are_listed_without_disabling_a_valid_role() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    for (file, text) in [
        (
            "valid.md",
            "---\nname: valid\ndescription: Inspect code.\ntools: read\n---\nRead only.",
        ),
        ("bad.md", "---\nname: [\n---\nInvalid YAML"),
        (
            "one.md",
            "---\nname: duplicate\ndescription: One.\n---\nOne",
        ),
        (
            "two.md",
            "---\nname: duplicate\ndescription: Two.\n---\nTwo",
        ),
        (
            "builtin.md",
            "---\nname: explorer\ndescription: Collision.\n---\nBad",
        ),
        (
            "tool.md",
            "---\nname: bad-tool\ndescription: Unknown tool.\ntools: no_such_tool\n---\nBad",
        ),
        (
            "provider.md",
            "---\nname: bad-provider\ndescription: Missing provider.\nmodel: missing/worker\n---\nBad",
        ),
        (
            "model.md",
            "---\nname: bad-model\ndescription: Missing model.\nmodel: configured/typo\n---\nBad",
        ),
    ] {
        role_file(root.path(), file, text);
    }
    std::os::unix::fs::symlink(
        "missing-target",
        root.path().join("state/agents/unreadable.md"),
    )
    .unwrap();
    let fake = Server::start(vec![complete(), complete()]).await;
    let session = parent_with_options(
        root.path(),
        &fake,
        CoreOptions {
            agent_role_providers: ["configured".into()].into(),
            ..Default::default()
        },
    )
    .await;
    let roles = session.roles().await.unwrap();
    assert_eq!(
        roles
            .data
            .iter()
            .find(|role| role.name == "bad-model")
            .unwrap()
            .error
            .as_deref(),
        Some("Model is not configured: configured/typo")
    );
    assert_eq!(
        roles
            .data
            .iter()
            .filter(|role| !role.built_in && role.error.is_none())
            .count(),
        1
    );
    assert_eq!(
        roles
            .data
            .iter()
            .filter(|role| role.error.is_some())
            .count(),
        8
    );
    say(&session, "Delegate inspection").await;
    let child = spawn(&session, "valid", "valid").await.unwrap();
    idle(&child).await;
    for name in ["duplicate", "bad-tool", "bad-provider"] {
        assert!(spawn(&session, name, name).await.is_err());
    }
    let error = spawn(&session, "bad-model", "bad-model")
        .await
        .err()
        .expect("unconfigured model must not spawn");
    assert_eq!(error.message, "Model is not configured: configured/typo");
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        2,
        "invalid roles must not send requests"
    );
}

#[tokio::test]
async fn files_refresh_next_turn_but_existing_child_keeps_snapshot_after_reopen() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    role_file(
        root.path(),
        "worker.md",
        "---\nname: worker\ndescription: Original role.\ntools: read\nmodel: saved/original\neffort: low\n---\nOriginal immutable instructions.",
    );
    let fake = Server::start((0..7).map(|_| complete()).collect()).await;
    let session = parent_with_options(
        root.path(),
        &fake,
        CoreOptions {
            agent_role_providers: ["saved".into()].into(),
            model_limits: ["saved/original", "saved/changed"]
                .into_iter()
                .map(|model| {
                    (
                        model.into(),
                        canter_core::ModelLimits {
                            context_window: 200_000,
                            auto_compact_tokens: Some(100_000),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        },
    )
    .await;
    say(&session, "First task").await;
    let child = spawn(&session, "worker", "first-child").await.unwrap();
    idle(&child).await;
    role_file(
        root.path(),
        "worker.md",
        "---\nname: worker\ndescription: Changed role.\ntools: read, write\nmodel: saved/changed\neffort: high\n---\nChanged instructions.",
    );
    say(&session, "Second task").await;
    let changed = spawn(&session, "worker", "second-child").await.unwrap();
    idle(&changed).await;
    std::fs::remove_file(root.path().join("state/agents/worker.md")).unwrap();
    say(&session, "Third task").await;
    assert!(spawn(&session, "worker", "missing").await.is_err());
    child.close().await.unwrap();
    drop(child);
    let reopened = session.open_agent("first-child".into()).await.unwrap();
    let snapshot = reopened.snapshot().await.session;
    assert_eq!(snapshot.settings.model, "saved/original");
    assert_eq!(snapshot.settings.reasoning_effort, ReasoningEffort::Low);
    session
        .send_agent_input(&reopened, "Continue the original task".into())
        .await
        .unwrap();
    idle(&reopened).await;
    let requests = fake.requests.lock().unwrap();
    let original = &requests[1];
    let changed = &requests[3];
    let restored = &requests[5];
    assert_eq!(original["tools"], restored["tools"]);
    assert_ne!(original["tools"], changed["tools"]);
    let text = restored.to_string();
    assert!(text.contains("Original immutable instructions."));
    assert!(!text.contains("Changed instructions."));
    assert!(
        !requests[4]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == "spawn_agent")
            .unwrap()["function"]["description"]
            .as_str()
            .unwrap()
            .contains("Changed role.")
    );
}

#[tokio::test]
async fn role_model_and_effort_defaults_inherit_or_override_without_fallback() {
    use canter_core::{ModelLimits, ModelReasoning};
    let root = tempfile::tempdir_in("/tmp").unwrap();
    for (name, extra) in [
        ("inherits", ""),
        ("model-only", "model: other/worker\n"),
        ("explicit", "model: other/worker\neffort: high\n"),
        ("unsupported", "model: other/worker\neffort: low\n"),
    ] {
        role_file(
            root.path(),
            &format!("{name}.md"),
            &format!(
                "---\nname: {name}\ndescription: Work independently.\n{extra}---\nDo the task."
            ),
        );
    }
    let options = CoreOptions {
        agent_role_providers: ["other".into()].into(),
        model_limits: [(
            "other/worker".into(),
            ModelLimits {
                context_window: 200_000,
                auto_compact_tokens: Some(100_000),
            },
        )]
        .into(),
        model_reasoning: [(
            "other/worker".into(),
            ModelReasoning {
                supported: vec![ReasoningEffort::Medium, ReasoningEffort::High],
                default: ReasoningEffort::Medium,
            },
        )]
        .into(),
        ..Default::default()
    };
    let fake = Server::start((0..4).map(|_| complete()).collect()).await;
    let session = parent_with_options(root.path(), &fake, options).await;
    session
        .update(serde_json::from_value(json!({"reasoningEffort":"low"})).unwrap())
        .await
        .unwrap();
    say(&session, "Delegate tasks").await;
    for (role, model, effort) in [
        ("inherits", "fake-model", ReasoningEffort::Low),
        ("model-only", "other/worker", ReasoningEffort::Medium),
        ("explicit", "other/worker", ReasoningEffort::High),
    ] {
        let child = spawn(&session, role, role).await.unwrap();
        let snapshot = idle(&child).await;
        assert_eq!(snapshot.settings.model, model);
        assert_eq!(snapshot.settings.reasoning_effort, effort);
    }
    let error = spawn(&session, "unsupported", "unsupported")
        .await
        .err()
        .expect("unsupported effort should fail before spawning");
    assert!(error.message.contains("not supported"));
    assert_eq!(fake.requests.lock().unwrap().len(), 4);
}
