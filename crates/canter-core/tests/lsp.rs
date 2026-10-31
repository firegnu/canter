#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use canter_tools::lsp::{LspConfig, LspService};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use support::{KEY, Reply, Server};

struct WarmService {
    service: Arc<LspService>,
    _instance: Option<canter_tools::lsp::Instance>,
}
impl std::ops::Deref for WarmService {
    type Target = Arc<LspService>;
    fn deref(&self) -> &Self::Target {
        &self.service
    }
}
async fn diagnostics_ready(session: &Session) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !session.diagnostics().await.data.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn release(root: &Path) {
    std::fs::write(root.join("release-diagnostics"), "").unwrap();
}

fn executable() -> PathBuf {
    static FIXTURE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            assert!(
                std::process::Command::new("rustc")
                    .args([
                        "--edition=2024",
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/../canter-tools/tests/fixtures/lsp.rs"
                        ),
                        "-o"
                    ])
                    .arg(directory.path().join("fake-lsp"))
                    .status()
                    .unwrap()
                    .success()
            );
            // macOS validates a newly built executable on first launch. Warm the fixture
            // outside the measured write, without changing the production 500 ms budget.
            let status = std::process::Command::new(directory.path().join("fake-lsp"))
                .env_clear()
                .env("HOME", directory.path())
                .env("CANTER_HOME", directory.path().join("state"))
                .env("LSP_TEST_DIR", directory.path())
                .env("LSP_TEST_MODE", "fail")
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(1));
            directory
        })
        .path()
        .join("fake-lsp")
}
fn reply(delta: Value, reason: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    ))
}
fn write(text: &str) -> Reply {
    write_to("main.fake", text)
}
fn write_to(path: &str, text: &str) -> Reply {
    reply(
        json!({"tool_calls":[{"index":0,"id":"write-file","function":{"name":"write","arguments":json!({"path":path,"content":text}).to_string()}}]}),
        "tool_calls",
    )
}
fn done() -> Reply {
    reply(json!({"content":"done"}), "stop")
}
fn input() -> SessionInputParams {
    SessionInputParams {
        non_interactive: false,
        approval_policy: None,
        input: vec![UserInput::Text {
            text: "modify".into(),
        }],
        mode: InputMode::Queue,
        client_name: "test".into(),
        review: None,
    }
}
async fn finished(events: &mut canter_core::Subscription) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let EventPayload::TurnCompleted { turn } =
                events.recv().await.unwrap().unwrap().payload
            {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                break;
            }
        }
    })
    .await
    .unwrap();
}
/// Quick diagnostics get a generous wait so a busy machine cannot push them past the window;
/// the 500 ms boundary itself is tested with the production options.
fn options() -> CoreOptions {
    CoreOptions {
        lsp_inline_wait: Duration::from_secs(5),
        ..CoreOptions::default()
    }
}
async fn create(
    root: &Path,
    server: &Server,
    enabled: bool,
    mode: &str,
) -> (Session, WarmService, Arc<Store>) {
    create_attached(root, server, enabled, mode, true, options()).await
}
async fn create_attached(
    root: &Path,
    server: &Server,
    enabled: bool,
    mode: &str,
    attached: bool,
    options: CoreOptions,
) -> (Session, WarmService, Arc<Store>) {
    std::fs::write(root.join("project.marker"), "").unwrap();
    let service = LspService::new(serde_json::from_value::<LspConfig>(json!({"enabled":enabled,"servers":{"fake":{"command":executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}})).unwrap()).unwrap();
    let runtime = runtime(root, attached.then(|| service.clone()), mode);
    let instance = if enabled && !matches!(mode, "fail" | "hang_init") {
        std::fs::write(root.join("warm.fake"), "").unwrap();
        Some(
            service
                .for_file(
                    &root.join("warm.fake"),
                    &canter_tools::lsp::FileContext {
                        cwd: root.into(),
                        additional_directories: vec![],
                        environment: runtime.environment.clone(),
                        round: canter_tools::lsp::Round::Modify,
                    },
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap()
                .unwrap(),
        )
    } else {
        None
    };
    let store = Arc::new(Store::new(root.join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        if attached {
            "lsp-test".into()
        } else {
            "baseline".into()
        },
        SessionSettings {
            tags: Default::default(),
            cwd: root.display().to_string(),
            name: None,
            model: "fake".into(),
            reasoning_effort: Default::default(),
            approval_policy: PermissionMode::AllowAll,
            additional_directories: vec![],
            plan_enabled: None,
            sandbox_enabled: None,
            plan_mode: false,
            pre_plan_approval_policy: None,
        },
        runtime,
        Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
        options,
    )
    .await
    .unwrap();
    (
        session,
        WarmService {
            service,
            _instance: instance,
        },
        store,
    )
}
fn runtime(root: &Path, lsp: Option<Arc<LspService>>, mode: &str) -> RuntimeContext {
    RuntimeContext {
        lsp,
        environment: [
            ("HOME".into(), root.join("home").display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("private").display().to_string(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LSP_TEST_DIR".into(), root.display().to_string()),
            ("LSP_TEST_MODE".into(), mode.into()),
        ]
        .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn quick_diagnostics_are_in_the_file_tool_result() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    let tool = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    let output = tool.to_owned();
    assert_eq!(server.arrivals.lock().unwrap().len(), 2);
    let snapshot = session.snapshot().await;
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(output.contains("[LSP diagnostics]"), "{output}");
    assert!(
        output.contains("main.fake:1:1: error fixture error"),
        "{output}"
    );
    assert!(output.contains("First report for this file: pre-existing problems may be included."));
    assert!(output.contains("1 error, 0 warnings."));
    // P2-135: the public item carries the counts and that it came with the tool result.
    let items = serde_json::to_value(&snapshot.session.turns[0].items).unwrap();
    let diagnostics: Vec<&Value> = items
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "diagnostics")
        .collect();
    assert_eq!(diagnostics.len(), 1, "{items}");
    assert_eq!(diagnostics[0]["errorCount"], 1);
    assert_eq!(diagnostics[0]["warningCount"], 0);
    assert_eq!(diagnostics[0]["delivery"], "inline");
}

#[tokio::test]
async fn late_diagnostics_wait_for_a_real_request_and_preserve_the_sent_prefix() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, store) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let before = server.requests.lock().unwrap()[1].clone();
    release(root.path());
    diagnostics_ready(&session).await;
    assert_eq!(
        server.requests.lock().unwrap().len(),
        2,
        "LSP must not wake the model"
    );
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    let history = store.model_history::<Value>("lsp-test").unwrap();
    let snapshot = session.snapshot().await;
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    // P2-135: the pending status came with the tool result, the errors later.
    let items: Vec<Value> = snapshot
        .session
        .turns
        .iter()
        .flat_map(|turn| {
            serde_json::to_value(&turn.items)
                .unwrap()
                .as_array()
                .unwrap()
                .clone()
        })
        .filter(|item| item["type"] == "diagnostics")
        .collect();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["delivery"], "inline");
    assert!(items[0].get("errorCount").is_none());
    assert_eq!(items[1]["status"], "available");
    assert_eq!(items[1]["delivery"], "late");
    assert_eq!(items[1]["errorCount"], 1);
    assert_eq!(items[1]["warningCount"], 0);
    let messages = requests[2]["messages"].as_array().unwrap();
    assert_eq!(
        &messages[..before["messages"].as_array().unwrap().len()],
        before["messages"].as_array().unwrap()
    );
    assert!(before.to_string().contains("still pending after 500 ms"));
    assert!(
        messages
            .iter()
            .any(|m| m["content"].as_str().is_some_and(|s| s
                .starts_with("[LSP diagnostics update] (arrived after the file change)")
                && s.contains("fixture error")))
    );
    assert!(
        history
            .iter()
            .any(|entry| entry["type"] == "diagnostics_notice")
    );
}

fn tool_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .into()
}
#[tokio::test]
async fn reports_are_bounded_and_only_delivered_diagnostics_are_deduplicated() {
    let root = tempfile::tempdir().unwrap();
    let diagnostics:Vec<_>=(0..30).map(|i|json!({"range":{"start":{"line":i,"character":0},"end":{"line":i,"character":1}},"severity":if i==0{3}else{1},"code":format!("E{i}"),"message":format!("issue {i} {}","字".repeat(500))})).collect();
    std::fs::write(
        root.path().join("diagnostics.json"),
        serde_json::to_vec(&diagnostics).unwrap(),
    )
    .unwrap();
    let server = Server::start(vec![write("bad"), done(), write("bad again"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    let first = tool_text(&requests[1]);
    let first = first.split_once("[LSP diagnostics]").unwrap().1;
    assert!(first.len() + "[LSP diagnostics]".len() <= 8192);
    assert!(first.contains("29 errors, 0 warnings."));
    assert!(first.contains("not shown"));
    assert!(!first.contains("[E0]"));
    assert!(first.contains("[truncated]"));
    let lines: Vec<_> = first.lines().filter(|l| l.contains(": error ")).collect();
    assert!(lines.len() <= 20 && lines.iter().all(|line| line.len() <= 1024));
    let second = tool_text(&requests[3]);
    assert!(!second.contains("[E1]"), "already shown issue was repeated");
    assert!(second.contains("Only new or changed diagnostics are listed."));
    assert!(
        second.contains(": error "),
        "omitted entries must remain eligible"
    );
}
#[tokio::test]
async fn confirmed_empty_reports_resolve_issues_without_claiming_project_checks_passed() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), write("good"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let text = tool_text(&server.requests.lock().unwrap()[3]);
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(text.contains("Resolved since last report: 1."), "{text}");
    assert!(text.contains("This is not a project check result."));
    assert!(!text.contains("passed"));
}
#[tokio::test]
async fn changing_the_file_discards_pending_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    std::fs::write(root.path().join("main.fake"), "good").unwrap();
    release(root.path());
    diagnostics_ready(&session).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(!request.to_string().contains("fixture error"));
    assert!(!request.to_string().contains("[LSP diagnostics update]"));
}
#[tokio::test]
async fn subscription_stop_and_retry_are_visible_without_waking_the_model() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let data = session.language_servers().await.unwrap().data;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].state, LanguageServerState::Running);
    assert_eq!(data[0].name, "fake");
    assert_ne!(data[0].id, "fake");
    let id = data[0].id.clone();
    assert_eq!(session.diagnostics().await.data[0].diagnostics.len(), 1);
    let stopped = session.stop_language_server(id.clone()).await.unwrap();
    assert_eq!(stopped.data[0].state, LanguageServerState::Stopped);
    assert!(session.diagnostics().await.data.is_empty());
    session.retry_language_server(id.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !session.diagnostics().await.data.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.language_servers().await.unwrap().data[0].id, id);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
}

#[tokio::test]
async fn disabled_lsp_requests_match_the_original_path_byte_for_byte() {
    let root = tempfile::tempdir().unwrap();
    let baseline = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) =
        create_attached(root.path(), &baseline, false, "sync", false, options()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    session.close().await.unwrap();
    service.shutdown().await;
    let expected = baseline.requests.lock().unwrap().clone();
    baseline.stop().await;
    std::fs::remove_file(root.path().join("main.fake")).unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) = create(root.path(), &server, false, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    assert_eq!(server.requests.lock().unwrap().as_slice(), expected);
    assert!(!root.path().join("starts").exists());
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
}
#[tokio::test]
async fn unsupported_files_and_stopped_subscriptions_preserve_plain_tool_results() {
    for (file, stopped) in [("notes.md", false), ("main.fake", true)] {
        let root = tempfile::tempdir().unwrap();
        let replies = || {
            vec![
                write_to(file, "bad"),
                done(),
                write_to(file, "good"),
                done(),
            ]
        };
        let baseline = Server::start(replies()).await;
        let (session, service, _) =
            create_attached(root.path(), &baseline, false, "sync", false, options()).await;
        let mut events = session.subscribe(0).await.unwrap();
        for _ in 0..2 {
            session.input(input()).await.unwrap();
            finished(&mut events).await;
        }
        let expected = tool_text(&baseline.requests.lock().unwrap()[3]);
        session.close().await.unwrap();
        service.shutdown().await;
        baseline.stop().await;
        std::fs::remove_file(root.path().join(file)).unwrap();

        let server = Server::start(replies()).await;
        let (session, service, _) = create(root.path(), &server, true, "sync").await;
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        if stopped {
            let id = session.language_servers().await.unwrap().data[0].id.clone();
            session.stop_language_server(id).await.unwrap();
        }
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        let output = tool_text(&server.requests.lock().unwrap()[3]);
        let snapshot = session.snapshot().await;
        session.close().await.unwrap();
        service.shutdown().await;
        server.stop().await;
        assert_eq!(output, expected, "{file}");
        assert!(
            !snapshot
                .session
                .turns
                .last()
                .unwrap()
                .items
                .iter()
                .any(|item| matches!(item, Item::Diagnostics { .. })),
            "{file}"
        );
    }
}

#[tokio::test]
async fn reopening_keeps_delivered_history_but_drops_undelivered_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, store) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    release(root.path());
    diagnostics_ready(&session).await;
    let old_id = session.language_servers().await.unwrap().data[0].id.clone();
    session.close().await.unwrap();
    drop(session);
    let (session, snapshot) = Session::open(
        store,
        "lsp-test".into(),
        runtime(root.path(), Some(service.clone()), "late_sync"),
        Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
        options(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.session.language_servers[0].id, old_id);
    assert_eq!(
        snapshot.session.language_servers[0].state,
        LanguageServerState::Reconnecting
    );
    let mut events = session.subscribe(snapshot.event_seq).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(request.to_string().contains("still pending after 500 ms"));
    assert!(!request.to_string().contains("fixture error"));
    assert!(!request.to_string().contains("[LSP diagnostics update]"));
}
#[tokio::test]
async fn compaction_labels_late_diagnostics_as_tools_and_does_not_replay_them() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done(), done(), done()]).await;
    let (session, service, store) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    release(root.path());
    diagnostics_ready(&session).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    session.compact().await.unwrap();
    finished(&mut events).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    let history = store.model_history::<Value>("lsp-test").unwrap();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(
        requests[3]
            .to_string()
            .contains("Tool diagnostics: [LSP diagnostics update]")
    );
    let compact = history.iter().find(|e| e["type"] == "compaction").unwrap();
    assert!(!compact["user_messages"].to_string().contains("LSP"));
    assert!(!requests[4].to_string().contains("[LSP diagnostics update]"));
}
#[tokio::test]
async fn retracting_a_late_notice_keeps_later_user_words_labeled_as_user() {
    let root = tempfile::tempdir().unwrap();
    let mut hanging = Reply::sse("");
    hanging.hang = true;
    let server = Server::start(vec![write("bad"), done(), hanging, done(), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    release(root.path());
    diagnostics_ready(&session).await;
    let turn_id = session.input(input()).await.unwrap().turn_id.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while server.requests.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        server.requests.lock().unwrap()[2]
            .to_string()
            .contains("[LSP diagnostics update]")
    );
    let response = session
        .interrupt(SessionInterruptParams {
            turn_id,
            retract: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(response.retracted, Some(true));
    session
        .shell_command(SessionShellCommandParams {
            command: "echo hi".into(),
            timeout_ms: None,
        })
        .await
        .unwrap();
    finished(&mut events).await;
    let mut message = input();
    message.input = vec![UserInput::Text {
        text: "RETRACT user words".into(),
    }];
    session.input(message).await.unwrap();
    finished(&mut events).await;
    session.compact().await.unwrap();
    finished(&mut events).await;
    let summary_request = server.requests.lock().unwrap()[4].to_string();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(
        summary_request.contains("User: RETRACT user words"),
        "{summary_request}"
    );
    assert!(!summary_request.contains("Tool diagnostics: RETRACT user words"));
}

#[tokio::test]
async fn history_changes_restart_reports_without_replaying_old_batches() {
    for action in ["rewind", "retract", "compact"] {
        let root = tempfile::tempdir().unwrap();
        let mut replies = vec![write("bad"), done()];
        if action == "retract" {
            let mut hanging = Reply::sse("");
            hanging.hang = true;
            replies.push(hanging);
        } else if action == "compact" {
            replies.push(done());
        }
        replies.extend([done(), write("bad"), done()]);
        let server = Server::start(replies).await;
        let (session, service, _) = create(root.path(), &server, true, "sync").await;
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        assert!(
            tool_text(&server.requests.lock().unwrap()[1])
                .contains("main.fake:1:1: error fixture error")
        );
        match action {
            "rewind" => {
                let mut params = SessionRewindParams {
                    before_turn_id: session.snapshot().await.session.turns[0].id.clone(),
                    rewind_files: true,
                    rewind_conversation: true,
                    confirmed_external_changes: vec![],
                    last_event_seq: None,
                };
                params.last_event_seq = Some(
                    session
                        .rewind_preview(params.clone())
                        .await
                        .unwrap()
                        .last_event_seq,
                );
                session.rewind(params).await.unwrap();
                assert!(!root.path().join("main.fake").exists());
            }
            "retract" => {
                let turn_id = session.input(input()).await.unwrap().turn_id.unwrap();
                tokio::time::timeout(Duration::from_secs(10), async {
                    while server.requests.lock().unwrap().len() < 3 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let response = session
                    .interrupt(SessionInterruptParams {
                        turn_id,
                        retract: Some(true),
                    })
                    .await
                    .unwrap();
                assert_eq!(response.retracted, Some(true));
            }
            "compact" => {
                session.compact().await.unwrap();
                finished(&mut events).await;
            }
            _ => unreachable!(),
        }
        // Clearing the baseline must not replay the already delivered batch.
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        let request = server.requests.lock().unwrap().last().unwrap().to_string();
        assert!(
            !request.contains("[LSP diagnostics update]"),
            "{action}: {request}"
        );
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        let output = tool_text(server.requests.lock().unwrap().last().unwrap());
        session.close().await.unwrap();
        service.shutdown().await;
        server.stop().await;
        assert!(
            output.contains("main.fake:1:1: error fixture error"),
            "{action}: {output}"
        );
        assert!(
            output.contains("First report for this file: pre-existing problems may be included."),
            "{action}: {output}"
        );
        assert!(
            !output.contains("Only new or changed diagnostics are listed."),
            "{action}: {output}"
        );
    }
}

#[tokio::test]
async fn rewind_discards_the_removed_changes_pending_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let before = session.snapshot().await;
    let mut params = SessionRewindParams {
        before_turn_id: before.session.turns[0].id.clone(),
        rewind_files: true,
        rewind_conversation: true,
        confirmed_external_changes: vec![],
        last_event_seq: None,
    };
    params.last_event_seq = Some(
        session
            .rewind_preview(params.clone())
            .await
            .unwrap()
            .last_event_seq,
    );
    session.rewind(params).await.unwrap();
    release(root.path());
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(!request.to_string().contains("fixture error"));
    assert!(!request.to_string().contains("[LSP diagnostics update]"));
}
#[tokio::test]
async fn unversioned_empty_lists_do_not_claim_issues_are_resolved() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), write("good"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "unversioned_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let output = tool_text(&server.requests.lock().unwrap()[3]);
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(output.contains("Earlier problems are not confirmed resolved."));
    assert!(!output.contains("Resolved since last report:"));
}
#[tokio::test]
async fn expired_delivery_is_not_revived_by_a_later_model_request() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let window = CoreOptions {
        lsp_delivery_window: Duration::from_secs(2),
        ..CoreOptions::default()
    };
    let (session, service, _) =
        create_attached(root.path(), &server, true, "late_sync", true, window).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    release(root.path());
    diagnostics_ready(&session).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].to_string();
    let snapshot = session.snapshot().await;
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(!request.contains("fixture error"));
    assert!(request.contains("60-second diagnostic delivery window has expired"));
    assert!(snapshot.session.turns[1].items.iter().any(|i| matches!(
        i,
        Item::Diagnostics {
            status: DiagnosticStatus::TimedOut,
            ..
        }
    )));
}

#[tokio::test]
async fn diagnostics_at_the_500ms_boundary_are_delivered_once() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, _) = create_attached(
        root.path(),
        &server,
        true,
        "edge_sync",
        true,
        CoreOptions::default(),
    )
    .await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    diagnostics_ready(&session).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert_eq!(requests[2].to_string().matches("fixture error").count(), 1);
    let old = requests[1]["messages"].as_array().unwrap();
    assert_eq!(
        &requests[2]["messages"].as_array().unwrap()[..old.len()],
        old
    );
}
#[tokio::test]
async fn server_failure_does_not_turn_a_successful_file_change_into_failure() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "fail").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let output = tool_text(&server.requests.lock().unwrap()[1]);
    let snapshot = session.snapshot().await;
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.fake")).unwrap(),
        "bad"
    );
    assert!(output.contains("The file change succeeded. Diagnostics are unavailable:"));
    assert!(snapshot.session.turns[0].items.iter().any(|i| matches!(
        i,
        Item::FileChange {
            status: ItemStatus::Completed,
            ..
        }
    )));
}

#[tokio::test]
async fn rewriting_the_same_bytes_invalidates_the_original_pending_change() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    std::fs::write(root.path().join("main.fake"), "bad").unwrap();
    release(root.path());
    diagnostics_ready(&session).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].to_string();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(!request.contains("[LSP diagnostics update]"));
}
#[tokio::test]
async fn a_stopped_subscription_remains_stopped_on_reopen_and_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, store) = create(root.path(), &server, true, "sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let id = session.language_servers().await.unwrap().data[0].id.clone();
    session.stop_language_server(id.clone()).await.unwrap();
    session.close().await.unwrap();
    drop(session);
    let (session, snapshot) = Session::open(
        store,
        "lsp-test".into(),
        runtime(root.path(), Some(service.clone()), "sync"),
        Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
        options(),
    )
    .await
    .unwrap();
    let restored = snapshot.session.language_servers[0].state;
    let retry = session.retry_language_server(id.clone()).await;
    if retry.is_ok() {
        diagnostics_ready(&session).await;
    }
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert_eq!(restored, LanguageServerState::Stopped);
    assert!(retry.is_ok());
}

#[tokio::test]
async fn push_and_pull_reports_for_one_file_are_not_duplicated() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done(), write("good"), done()]).await;
    let (session, service, _) = create(root.path(), &server, true, "both_sync_pull").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    diagnostics_ready(&session).await;
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    let first = tool_text(&requests[1]);
    let second = tool_text(&requests[3]);
    assert_eq!(first.matches("fixture error").count(), 1, "{first}");
    assert_eq!(first.matches("main.fake (fake).").count(), 1, "{first}");
    assert_eq!(
        second.matches("Resolved since last report: 1.").count(),
        1,
        "{second}"
    );
}

#[tokio::test]
async fn late_diagnostics_do_not_enter_history_during_a_stream() {
    let root = tempfile::tempdir().unwrap();
    let mut hanging = Reply::sse(format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"still streaming"},"finish_reason":null}]})
    ));
    hanging.hang = true;
    let server = Server::start(vec![write("bad"), hanging, done()]).await;
    let (session, service, store) = create(root.path(), &server, true, "late_sync").await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if server.requests.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    release(root.path());
    diagnostics_ready(&session).await;
    assert!(
        !store
            .model_history::<Value>("lsp-test")
            .unwrap()
            .iter()
            .any(|e| e["type"] == "diagnostics_notice")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let turn_id = session.snapshot().await.session.turns[0].id.clone();
    session
        .interrupt(SessionInterruptParams {
            turn_id,
            retract: None,
        })
        .await
        .unwrap();
    loop {
        if let EventPayload::TurnCompleted { turn } = events.recv().await.unwrap().unwrap().payload
        {
            assert_eq!(turn.status, TurnStatus::Interrupted);
            break;
        }
    }
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let request = server.requests.lock().unwrap()[2].to_string();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert_eq!(request.matches("fixture error").count(), 1);
}
#[tokio::test]
async fn server_initialization_is_inside_the_file_tools_wait_budget() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(vec![write("bad"), done()]).await;
    let (session, service, _) = create_attached(
        root.path(),
        &server,
        true,
        "hang_init",
        true,
        CoreOptions::default(),
    )
    .await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    let output = tool_text(&server.requests.lock().unwrap()[1]);
    let arrivals = server.arrivals.lock().unwrap().clone();
    session.close().await.unwrap();
    service.shutdown().await;
    server.stop().await;
    assert!(output.contains("still pending after 500 ms"));
    assert!(arrivals[1].duration_since(arrivals[0]) < Duration::from_secs(1));
}
