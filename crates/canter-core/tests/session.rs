use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{
    AssistantContent, FinishReason, Message, Model, ModelEvent, ModelStream, Request, ToolCall,
};
use canter_protocol::*;
use canter_store::Store;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[path = "support/telemetry.rs"]
mod telemetry;

struct ScriptModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Mutex<Vec<Request>>,
}
impl ScriptModel {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}
impl Model for ScriptModel {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request");
        Box::pin(async_stream::stream! {
            let complete = events.iter().any(|event| matches!(event, ModelEvent::Finished(_)));
            for event in events { yield Ok(event); }
            if !complete { cancel.cancelled().await; }
        })
    }
}

fn assert_request_extension(previous: &Request, next: &Request) {
    assert!(next.messages.len() > previous.messages.len());
    let mut prefix = next.clone();
    prefix.messages.truncate(previous.messages.len());
    assert_eq!(
        serde_json::to_string(&prefix).unwrap(),
        serde_json::to_string(previous).unwrap(),
        "messages, tools, model and options must stay byte-for-byte unchanged"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_model_request_exposes_scope_only_when_enabled() {
    for enabled in [false, true] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        for name in ["home", "project", "state"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let model = ScriptModel::new(vec![vec![ModelEvent::Finished(FinishReason::Stop)]]);
        let (session, _) = Session::create(
            Arc::new(Store::new(temp.path().join("state/sessions")).unwrap()),
            "sandbox-definition".into(),
            settings(&temp.path().join("project")),
            RuntimeContext {
                environment: BTreeMap::from([
                    (
                        "HOME".into(),
                        temp.path().join("home").display().to_string(),
                    ),
                    (
                        "CANTER_HOME".into(),
                        temp.path().join("state").display().to_string(),
                    ),
                ]),
                protected_paths: vec![temp.path().join("state")],
                ..Default::default()
            },
            model.clone(),
            CoreOptions {
                sandbox: enabled,
                request_time: || 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session
            .input(input("Describe the workspace."))
            .await
            .unwrap();
        finished(&mut events).await;
        let mut request = model.requests.lock().unwrap()[0].clone();
        let shell = request
            .tools
            .iter()
            .find(|tool| tool.name == "shell")
            .unwrap();
        assert_eq!(
            shell.parameters["properties"]
                .get("sandboxRequest")
                .is_some(),
            enabled
        );
        assert_eq!(
            shell.parameters["properties"]
                .get("justification")
                .is_some(),
            enabled
        );
        assert_eq!(
            shell.description.contains("Commands run in a sandbox:"),
            enabled
        );
        if !enabled {
            for message in &mut request.messages {
                if let Message::System { text } = message {
                    *text = text
                        .lines()
                        .map(|line| {
                            if line.starts_with("Session opened:") {
                                "Session opened: <date>"
                            } else if line.starts_with("Platform:") {
                                "Platform: <platform>"
                            } else {
                                line
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
            let serialized = serde_json::to_string(&request)
                .unwrap()
                .replace(&temp.path().display().to_string(), "/fixture");
            assert_eq!(
                serialized.as_bytes(),
                include_bytes!("sandbox-disabled-request.json")
            );
        }
        session.close().await.unwrap();
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_session_uses_one_explicit_approval_and_preserves_decline_feedback() {
    for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
        for decision in [
            ApprovalDecision::Accept,
            ApprovalDecision::DeclineWithFeedback,
        ] {
            let temp = tempfile::tempdir_in("/tmp").unwrap();
            for name in ["home", "project", "state", "outside"] {
                std::fs::create_dir(temp.path().join(name)).unwrap();
            }
            let arguments = serde_json::json!({"command":"printf approved > ../outside/result",
                "sandboxRequest":{"writePaths":["../outside/result"]}, "justification":"Write the requested output."}).to_string();
            let model = ScriptModel::new(vec![
                vec![
                    call("requested", "shell", &arguments),
                    ModelEvent::Finished(FinishReason::ToolCalls),
                ],
                vec![ModelEvent::Finished(FinishReason::Stop)],
            ]);
            let mut config = settings(&temp.path().join("project"));
            config.approval_policy = mode;
            let (session, _) = Session::create(
                Arc::new(Store::new(temp.path().join("state/sessions")).unwrap()),
                "sandbox-approval".into(),
                config,
                RuntimeContext {
                    environment: BTreeMap::from([
                        (
                            "HOME".into(),
                            temp.path().join("home").display().to_string(),
                        ),
                        (
                            "CANTER_HOME".into(),
                            temp.path().join("state").display().to_string(),
                        ),
                        ("PATH".into(), "/usr/bin:/bin".into()),
                    ]),
                    protected_paths: vec![temp.path().join("state")],
                    ..Default::default()
                },
                model.clone(),
                CoreOptions {
                    sandbox: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let mut events = session.subscribe(0).await.unwrap();
            session.input(input("Write the output.")).await.unwrap();
            let event = until(&mut events, |payload| {
                matches!(payload, EventPayload::RequestOpened { .. })
            })
            .await;
            let EventPayload::RequestOpened { request } = event.payload else {
                panic!("approval event")
            };
            let RequestKind::SandboxApproval(approval) = &request.kind else {
                panic!("scope must not appear as an ordinary approval")
            };
            assert_eq!(
                approval.arguments["justification"],
                "Write the requested output."
            );
            assert_eq!(
                approval.arguments["sandboxRequest"]["writePaths"],
                serde_json::json!([temp.path().canonicalize().unwrap().join("outside/result")])
            );
            assert!(!temp.path().join("outside/result").exists());
            let answer = |decision, feedback| RequestResolveParams {
                decision: Some(decision),
                feedback,
                execpolicy_amendment: None,
                plan: None,
                answer: None,
                client_name: "test-terminal".into(),
            };
            let invalid = session
                .resolve(
                    &request.id,
                    answer(ApprovalDecision::AcceptForSession, None),
                )
                .await
                .unwrap_err();
            assert_eq!(invalid.code, ErrorCode::InvalidDecision);
            let feedback = (decision == ApprovalDecision::DeclineWithFeedback)
                .then(|| "Keep the output inside the project.".to_owned());
            session
                .resolve(&request.id, answer(decision, feedback.clone()))
                .await
                .unwrap();
            let rest = finished(&mut events).await;
            assert!(
                !rest
                    .iter()
                    .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
            );
            assert_eq!(
                temp.path().join("outside/result").exists(),
                decision == ApprovalDecision::Accept
            );
            if let Some(feedback) = feedback {
                assert!(model.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, Message::ToolResult { text, .. } if text.contains(&feedback))));
                let snapshot = session.snapshot().await;
                assert_eq!(
                    snapshot.session.approval_feedback.get(&approval.item_id),
                    Some(&feedback)
                );
            }
            session.close().await.unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_session_applies_configuration_add_dir_and_closes_private_temp() {
    for enabled in [true, false] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        for name in ["home", "project", "extra", "state"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let command = serde_json::json!({"command":"printf '%s' \"$TMPDIR\" > temp-path; printf written > ../extra/result"}).to_string();
        let model = ScriptModel::new(vec![
            vec![
                call("first", "shell", &command),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![ModelEvent::Finished(FinishReason::Stop)],
            vec![
                call("second", "shell", &command),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![ModelEvent::Finished(FinishReason::Stop)],
        ]);
        let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
        let runtime = RuntimeContext {
            environment: BTreeMap::from([
                (
                    "HOME".into(),
                    temp.path().join("home").display().to_string(),
                ),
                (
                    "CANTER_HOME".into(),
                    temp.path().join("state").display().to_string(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]),
            protected_paths: vec![temp.path().join("state")],
            ..Default::default()
        };
        let (session, _) = Session::create(
            store,
            "sandbox".into(),
            settings(&temp.path().join("project")),
            runtime,
            model.clone(),
            CoreOptions {
                sandbox: enabled,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("before adding")).await.unwrap();
        finished(&mut events).await;
        assert_eq!(temp.path().join("extra/result").exists(), !enabled);
        if enabled {
            assert!(model.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, Message::ToolResult { text, .. } if text.contains("sandbox") && text.contains("!"))));
        }
        session
            .update(SessionUpdateParams {
                add_directories: vec!["../extra".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        session.input(input("after adding")).await.unwrap();
        finished(&mut events).await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("extra/result")).unwrap(),
            "written"
        );
        session
            .shell_command(SessionShellCommandParams {
                command: "printf user > ../user".into(),
                timeout_ms: None,
            })
            .await
            .unwrap();
        finished(&mut events).await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("user")).unwrap(),
            "user"
        );
        let temporary = std::fs::read_to_string(temp.path().join("project/temp-path")).unwrap();
        if enabled {
            assert!(std::path::Path::new(&temporary).is_dir());
        }
        session.close().await.unwrap();
        if enabled {
            assert!(!std::path::Path::new(&temporary).exists());
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandbox_status_follows_the_executor_directories_and_permission_mode() {
    for enabled in [true, false] {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        for name in ["home", "project", "extra", "state"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let store = Arc::new(Store::new(root.join("state/sessions")).unwrap());
        let runtime = || RuntimeContext {
            environment: BTreeMap::from([
                ("HOME".into(), root.join("home").display().to_string()),
                (
                    "CANTER_HOME".into(),
                    root.join("state").display().to_string(),
                ),
            ]),
            protected_paths: vec![root.join("state"), root.join("state/config.json")],
            ..Default::default()
        };
        let options = |sandbox| CoreOptions {
            sandbox,
            ..Default::default()
        };
        let mut start = settings(&root.join("project"));
        start.approval_policy = PermissionMode::AskForExecution;
        let (session, created) = Session::create(
            store.clone(),
            "status".into(),
            start,
            runtime(),
            ScriptModel::new(Vec::new()),
            options(enabled),
        )
        .await
        .unwrap();
        assert_eq!(created.session.settings.sandbox_enabled, Some(enabled));
        let text = |path: &str| root.join(path).display().to_string();
        let scope = session.sandbox().await;
        if !enabled {
            assert_eq!(scope, SessionSandboxResponse::default());
        } else {
            assert!(scope.enabled);
            assert!(!scope.network);
            assert_eq!(scope.writable[0], text("project"));
            assert_eq!(scope.writable.len(), 4, "{scope:?}");
            assert!(scope.writable[1].contains("/canter-shell-"));
            assert_eq!(
                &scope.writable[2..],
                [
                    text("home/Library/Caches/canter/cargo"),
                    text("home/Library/Caches/canter/npm")
                ]
            );
            assert_eq!(scope.read_only, [text("home/.cargo"), text("home/.npm")]);
            assert_eq!(
                scope.no_access,
                [
                    text("home/.ssh"),
                    text("home/.aws"),
                    text("home/.gnupg"),
                    text("home/.canter"),
                    text("state"),
                ]
            );
        }
        session
            .update(SessionUpdateParams {
                add_directories: vec!["../extra".into()],
                approval_policy: Some(PermissionMode::AllowAll),
                ..Default::default()
            })
            .await
            .unwrap();
        let scope = session.sandbox().await;
        assert_eq!(scope.enabled, enabled);
        assert_eq!(scope.network, enabled);
        if enabled {
            assert_eq!(&scope.writable[..2], [text("project"), text("extra")]);
            assert_eq!(scope.writable.len(), 5);
        }
        session
            .update(SessionUpdateParams {
                approval_policy: Some(PermissionMode::AlwaysAsk),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!session.sandbox().await.network);
        session.close().await.unwrap();
        drop(session);
        // A server started with the other setting reports it on reopen.
        let (reopened, read) = Session::open(
            store,
            "status".into(),
            runtime(),
            ScriptModel::new(Vec::new()),
            options(!enabled),
        )
        .await
        .unwrap();
        assert_eq!(read.session.settings.sandbox_enabled, Some(!enabled));
        assert_eq!(reopened.sandbox().await.enabled, !enabled);
    }
}

#[tokio::test]
async fn added_directories_are_normalized_persisted_and_appended_to_model_context() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["home", "main", "first", "second", "home/third", "state"] {
        std::fs::create_dir_all(temp.path().join(name)).unwrap();
    }
    std::fs::write(
        temp.path().join("second/AGENTS.md"),
        "EXTRA_INSTRUCTIONS_MUST_NOT_LOAD",
    )
    .unwrap();
    let runtime = || RuntimeContext {
        environment: [
            (
                "HOME".into(),
                temp.path().join("home").display().to_string(),
            ),
            (
                "CANTER_HOME".into(),
                temp.path().join("state").display().to_string(),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![ModelEvent::Finished(FinishReason::Stop)]; 3]);
    let mut config = settings(&temp.path().join("main"));
    config.additional_directories = vec!["../first".into()];
    let (session, created) = Session::create(
        store.clone(),
        "dirs".into(),
        config,
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let first = temp
        .path()
        .join("first")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        created.session.settings.additional_directories,
        vec![first.clone()]
    );
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("before adding")).await.unwrap();
    finished(&mut events).await;
    session
        .update(SessionUpdateParams {
            add_directories: vec![
                "../second".into(),
                "~/third".into(),
                "../second/.".into(),
                ".".into(),
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.settings.additional_directories.len(), 3);
    for invalid in ["../absent", "../second/AGENTS.md"] {
        let error = session
            .update(SessionUpdateParams {
                add_directories: vec![invalid.into()],
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.message.contains(invalid));
    }
    assert_eq!(
        session
            .snapshot()
            .await
            .session
            .settings
            .additional_directories,
        snapshot.settings.additional_directories
    );
    session.input(input("after adding")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert_request_extension(&requests[0], &requests[1]);
        assert!(
            matches!(&requests[0].messages[0], Message::System { text } if text.contains(&first))
        );
        let text = serde_json::to_string(&requests[1]).unwrap();
        assert!(text.contains(&snapshot.settings.additional_directories[1]));
        assert!(!text.contains("EXTRA_INSTRUCTIONS_MUST_NOT_LOAD"));
    }
    session.close().await.unwrap();
    drop(events);
    drop(session);
    let (reopened, response) = Session::open(
        store,
        "dirs".into(),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        response.session.settings.additional_directories,
        snapshot.settings.additional_directories
    );
    let mut events = reopened.subscribe(response.event_seq).await.unwrap();
    reopened.input(input("reopened")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap();
    assert!(
        matches!(&requests[2].messages[0], Message::System { text } if snapshot.settings.additional_directories.iter().all(|dir| text.contains(dir)))
    );
}

#[tokio::test]
async fn session_directory_updates_reach_the_file_permission_gate() {
    let temp = tempfile::tempdir().unwrap();
    for dir in ["main", "first", "second", "outside", "home", "state"] {
        std::fs::create_dir(temp.path().join(dir)).unwrap();
    }
    let runtime = || RuntimeContext {
        environment: [
            (
                "HOME".into(),
                temp.path().join("home").display().to_string(),
            ),
            (
                "CANTER_HOME".into(),
                temp.path().join("state").display().to_string(),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let calls = |paths: &[&str]| {
        let mut events: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                call(
                    &format!("write-{i}"),
                    "write",
                    &serde_json::json!({"path":path,"content":"written"}).to_string(),
                )
            })
            .collect();
        events.push(ModelEvent::Finished(FinishReason::ToolCalls));
        events
    };
    let model = ScriptModel::new(vec![
        calls(&["../first/file", "../second/file", "../outside/file"]),
        vec![ModelEvent::Finished(FinishReason::Stop)],
        calls(&["../second/after-reopen"]),
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let mut config = settings(&temp.path().join("main"));
    config.approval_policy = PermissionMode::AskForExecution;
    config.additional_directories = vec!["../first".into()];
    let (session, _) = Session::create(
        store.clone(),
        "dirs-gate".into(),
        config,
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    session
        .update(SessionUpdateParams {
            add_directories: vec!["../second".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("write files")).await.unwrap();
    let event = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = event.payload else {
        unreachable!()
    };
    let RequestKind::Approval(ref approval) = request.kind else {
        unreachable!()
    };
    assert_eq!(approval.arguments["path"], "../outside/file");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("first/file")).unwrap(),
        "written"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("second/file")).unwrap(),
        "written"
    );
    assert!(!temp.path().join("outside/file").exists());
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                execpolicy_amendment: None,
                decision: Some(ApprovalDecision::Accept),
                answer: None,
                feedback: None,
                plan: None,
                client_name: "test".into(),
            },
        )
        .await
        .unwrap();
    finished(&mut events).await;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("outside/file")).unwrap(),
        "written"
    );
    session.close().await.unwrap();
    drop(events);
    drop(session);
    let (session, response) = Session::open(
        store,
        "dirs-gate".into(),
        runtime(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(response.event_seq).await.unwrap();
    session.input(input("write after reopen")).await.unwrap();
    let events = finished(&mut events).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
    );
    assert!(temp.path().join("second/after-reopen").exists());
}

struct InterruptedModel {
    scripts: Mutex<VecDeque<Vec<Result<ModelEvent, canter_model::ModelError>>>>,
    requests: Mutex<Vec<Request>>,
}
impl Model for InterruptedModel {
    fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(futures_util::stream::iter(script))
    }
}

#[tokio::test]
async fn completed_items_survive_disconnect_and_tools_run_once() {
    for tool in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let completed = if tool {
            AssistantContent::ToolCall(ToolCall {
                id: "once".into(),
                name: "shell".into(),
                arguments: r#"{"command":"printf x >> executions"}"#.into(),
            })
        } else {
            AssistantContent::Text {
                text: "complete answer".into(),
            }
        };
        let model = Arc::new(InterruptedModel {
            requests: Mutex::new(Vec::new()),
            scripts: Mutex::new(VecDeque::from([
                vec![
                    Ok(ModelEvent::ItemCompleted {
                        item_id: "complete".into(),
                        content: completed.clone(),
                    }),
                    Ok(ModelEvent::TextDelta {
                        item_id: "unfinished".into(),
                        delta: "partial answer".into(),
                    }),
                    Ok(ModelEvent::ReasoningDelta {
                        item_id: "unfinished-thinking".into(),
                        delta: "partial thought".into(),
                    }),
                    Err(canter_model::ModelError {
                        kind: canter_model::ErrorKind::Network,
                        status: None,
                        retry_after: None,
                        retries: 0,
                    }),
                ],
                vec![
                    Ok(ModelEvent::TextDelta {
                        item_id: "next".into(),
                        delta: "continued".into(),
                    }),
                    Ok(ModelEvent::Finished(FinishReason::Stop)),
                ],
            ])),
        });
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let (session, _) = Session::create(
            store.clone(),
            "retry".into(),
            settings(temp.path()),
            RuntimeContext::default(),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("start")).await.unwrap();
        finished(&mut events).await;
        let snapshot = session.snapshot().await.session;
        assert_eq!(snapshot.turns[0].status, TurnStatus::Completed);
        let requests = model.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_request_extension(&requests[0], &requests[1]);
        assert!(requests[1].messages.iter().any(|message| matches!(message, Message::Assistant { content } if content == &vec![completed.clone()])));
        let history = serde_json::to_string(&requests[1].messages).unwrap();
        assert!(!history.contains("partial answer") && !history.contains("partial thought"));
        if tool {
            assert_eq!(
                std::fs::read_to_string(temp.path().join("executions")).unwrap(),
                "x"
            );
            assert_eq!(requests[1].messages.iter().filter(|message| matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == "once")).count(), 1);
        } else {
            assert!(snapshot.turns[0].items.iter().any(
                |item| matches!(item, Item::AgentMessage { text, .. } if text == "complete answer")
            ));
        }
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn authentication_error_preserves_partial_reply_for_the_next_turn() {
    let temp = tempfile::tempdir().unwrap();
    let model = Arc::new(InterruptedModel {
        requests: Mutex::new(Vec::new()),
        scripts: Mutex::new(VecDeque::from([
            vec![
                Ok(ModelEvent::ReasoningDelta {
                    item_id: "reasoning".into(),
                    delta: "partial thought".into(),
                }),
                Ok(ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "partial answer".into(),
                }),
                Ok(call(
                    "unfinished",
                    "shell",
                    r#"{"command":"touch must-not-exist"}"#,
                )),
                Err(canter_model::ModelError {
                    kind: canter_model::ErrorKind::Authentication,
                    status: Some(401),
                    retry_after: None,
                    retries: 0,
                }),
            ],
            vec![Ok(ModelEvent::Finished(FinishReason::Stop))],
        ])),
    });
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store,
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("start")).await.unwrap();
    let seen = finished(&mut events).await;
    let EventPayload::TurnCompleted { turn } = &seen.last().unwrap().payload else {
        unreachable!()
    };
    assert_eq!(turn.status, TurnStatus::Failed);
    assert!(
        turn.error
            .as_ref()
            .unwrap()
            .message
            .contains("Authentication")
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);

    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_request_extension(&requests[0], &requests[1]);
    let assistant = requests[1]
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Assistant { content } => Some(content),
            _ => None,
        })
        .expect("last partial reply must be retained");
    assert!(assistant.contains(&AssistantContent::Text {
        text: "partial answer".into()
    }));
    assert!(assistant.contains(&AssistantContent::Reasoning {
        text: "partial thought".into()
    }));
    assert!(
        assistant.iter().any(
            |part| matches!(part, AssistantContent::ToolCall(call) if call.id == "unfinished")
        )
    );
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "unfinished" && text == "No result: the model request did not finish")));
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn ordinary_and_compaction_requests_persist_their_model_and_charge() {
    // 1970-01-05 was Monday. Each case replays an ordinary and a compaction request.
    for (model_name, clock, expected) in [
        ("deepseek-flash", (|| 349_200_000) as fn() -> i64, 0.1848), // Mon 01:00
        ("deepseek-flash", (|| 360_000_000) as fn() -> i64, 0.0924), // Mon 04:00
        ("deepseek-v4-pro", (|| 367_200_000) as fn() -> i64, 0.6952), // Mon 06:00
        ("deepseek-v4-pro", (|| 781_200_000) as fn() -> i64, 0.3476), // Sat 01:00
        ("no-price", (|| 349_200_000) as fn() -> i64, 0.0),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let reply = || {
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "summary or reply".into(),
                },
                ModelEvent::Usage(canter_model::Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 100_000,
                    total_tokens: 1_100_000,
                    cache_read_tokens: Some(800_000),
                    cache_miss_tokens: Some(200_000),
                    reasoning_tokens: Some(50_000),
                }),
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        let mut config = settings(temp.path());
        config.model = model_name.into();
        let (session, _) = Session::create(
            store.clone(),
            "billing".into(),
            config,
            RuntimeContext::default(),
            ScriptModel::new(vec![reply(), reply()]),
            CoreOptions {
                request_time: clock,
                ..CoreOptions::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("hello")).await.unwrap();
        finished(&mut events).await;
        session.compact().await.unwrap();
        finished(&mut events).await;
        let usages = store
            .events_after("billing", 0)
            .unwrap()
            .into_iter()
            .filter(|event| matches!(event.payload, EventPayload::SessionUsage { .. }))
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(usages.len(), 2);
        for event in &usages {
            assert_eq!(event["tokenUsage"]["request"]["model"], model_name);
            assert_eq!(event["tokenUsage"]["request"]["startedAt"], clock());
            assert_ne!(
                event["timestamp"],
                clock(),
                "charge uses request time, not completion time"
            );
            if model_name == "no-price" {
                assert!(event["tokenUsage"]["request"]["costUsd"].is_null());
                assert_eq!(event["tokenUsage"]["cost"]["unknown"], true);
            } else {
                assert!(
                    (event["tokenUsage"]["request"]["costUsd"].as_f64().unwrap() - expected).abs()
                        < 1e-10
                );
            }
        }
        let snapshot = session.snapshot().await.session;
        let cost = snapshot.token_usage.unwrap().cost.unwrap();
        assert!((cost.usd - expected * 2.0).abs() < 1e-10);
        for turn in snapshot.turns {
            assert!((turn.usage.unwrap().cost.unwrap().usd - expected).abs() < 1e-10);
        }
        session.close().await.unwrap();
        drop(session);
        let (reopened, response) = Session::open(
            store,
            "billing".into(),
            RuntimeContext::default(),
            ScriptModel::new(vec![]),
            CoreOptions {
                // Removing all prices cannot change previously recorded costs or unknowns.
                prices: Default::default(),
                ..CoreOptions::default()
            },
        )
        .await
        .unwrap();
        let cost = response.session.token_usage.unwrap().cost.unwrap();
        assert!((cost.usd - expected * 2.0).abs() < 1e-10);
        assert_eq!(cost.unknown, model_name == "no-price");
        reopened.close().await.unwrap();
    }
}

#[tokio::test]
async fn request_prefix_is_frozen_until_reopening_and_only_compaction_replaces_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("AGENTS.md");
    std::fs::write(&path, "original instructions").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let reply = || {
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "answer".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ]
    };
    let model = ScriptModel::new(vec![
        reply(),
        reply(),
        reply(),
        reply(),
        reply(),
        vec![
            call("denied", "shell", r#"{"command":"touch must-not-exist"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        reply(),
        reply(),
        reply(),
        reply(),
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AlwaysAsk;
    let (mut session, _) = Session::create(
        store.clone(),
        "s1".into(),
        config,
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    std::fs::write(&path, "changed before first request").unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    for text in ["first", "second"] {
        session.input(input(text)).await.unwrap();
        finished(&mut events).await;
    }
    std::fs::write(&path, "changed instructions").unwrap();
    session
        .input(input("after editing AGENTS.md"))
        .await
        .unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert!(matches!(&requests[0].messages[0], Message::System { text }
            if text.contains("original instructions") && !text.contains("changed before first request")));
        for pair in requests.windows(2) {
            assert_request_extension(&pair[0], &pair[1]);
        }
    }
    // Reopening with unchanged instructions extends the last request too.
    std::fs::write(&path, "original instructions").unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    (session, _) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    events = session.subscribe(closed.event_seq).await.unwrap();
    session.input(input("reopened unchanged")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert_request_extension(&requests[2], &requests[3]);
    }
    assert_eq!(
        store
            .model_history::<serde_json::Value>("s1")
            .unwrap()
            .iter()
            .filter(|entry| entry["type"] == "context")
            .count(),
        1
    );

    std::fs::write(&path, "changed instructions").unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    (session, _) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    events = session.subscribe(closed.event_seq).await.unwrap();
    // The read happens on open, not on the first request.
    std::fs::write(&path, "later instructions").unwrap();
    session.input(input("reopened changed")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert!(matches!(&requests[4].messages[0], Message::System { text }
            if text.contains("changed instructions") && !text.contains("later instructions")));
        let mut previous = requests[3].clone();
        previous.messages[0] = requests[4].messages[0].clone();
        assert_request_extension(&previous, &requests[4]);
    }
    assert_eq!(
        store
            .model_history::<serde_json::Value>("s1")
            .unwrap()
            .iter()
            .filter(|entry| entry["type"] == "context")
            .count(),
        2
    );

    session.input(input("ask for approval")).await.unwrap();
    let opened = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = opened.payload else {
        unreachable!()
    };
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                execpolicy_amendment: None,
                plan: None,
                feedback: None,
                decision: Some(ApprovalDecision::Decline),
                answer: None,
                client_name: "terminal".into(),
            },
        )
        .await
        .unwrap();
    finished(&mut events).await;
    assert!(!temp.path().join("must-not-exist").exists());
    session.compact().await.unwrap();
    finished(&mut events).await;
    for text in ["after compaction", "continue"] {
        session.input(input(text)).await.unwrap();
        finished(&mut events).await;
    }
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 10);
    for pair in requests[4..7].windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(requests[6].messages.iter().any(|message|
        matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == "denied")));
    assert!(requests[7].tools.is_empty(), "independent summary request");
    assert_eq!(requests[7].messages[0], requests[6].messages[0]);
    assert_eq!(requests[8].messages[0], requests[6].messages[0]);
    assert!(requests[8].messages.iter().any(
        |message| matches!(message, Message::User { text } if text.contains("[Context summary]"))
    ));
    assert_request_extension(&requests[8], &requests[9]);
    session.close().await.unwrap();
}

async fn until(
    events: &mut canter_core::Subscription,
    predicate: impl Fn(&EventPayload) -> bool,
) -> Event {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if predicate(&event.payload) {
                return event;
            }
            assert!(
                !matches!(event.payload, EventPayload::TurnCompleted { .. }),
                "turn ended before expected event: {event:?}"
            );
        }
    })
    .await
    .expect("expected event did not arrive")
}

#[tokio::test]
async fn client_close_conflicts_only_when_shutdown_closed_first() {
    for (shutdown_first, expected_reason) in [
        (false, SessionCloseReason::ClientRequest),
        (true, SessionCloseReason::ServerShutdown),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let (session, _) = Session::create(
            store,
            "s1".into(),
            settings(temp.path()),
            RuntimeContext::default(),
            ScriptModel::new(vec![]),
            CoreOptions::default(),
        )
        .await
        .unwrap();

        let closed = if shutdown_first {
            session.close_for_shutdown().await.unwrap()
        } else {
            session.close().await.unwrap()
        };
        if shutdown_first {
            let error = session.close().await.expect_err(
                "a client close must not report success for a session closed by shutdown",
            );
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(error.message, "server is stopping");
        } else {
            let shutdown = session.close_for_shutdown().await.unwrap();
            assert_eq!(shutdown.event_seq, closed.event_seq);
        }
        let snapshot = session.snapshot().await.session;
        assert_eq!(snapshot.status, SessionStatus::Closed);
        assert_eq!(snapshot.close_reason, Some(expected_reason));
        assert_eq!(snapshot.last_event_seq, closed.event_seq);
    }
}

#[tokio::test]
async fn interrupt_during_model_stream_repairs_history_and_discards_queued_input() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "partial".into(),
            },
            call(
                "unfinished",
                "shell",
                r#"{"command":"touch must-not-exist"}"#,
            ),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "continued".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let first = session.input(input("start")).await.unwrap();
    until(&mut events, |payload| {
        matches!(payload, EventPayload::ItemDelta { .. })
    })
    .await;
    session.input(input("discard this")).await.unwrap();
    let interrupted = session
        .interrupt(SessionInterruptParams {
            retract: None,
            turn_id: first.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert_eq!(seen.last().unwrap().seq, interrupted.event_seq);
    assert!(seen.iter().any(|event| matches!(
        &event.payload,
        EventPayload::InputRemoved {
            reason: InputRemovalReason::Interrupted,
            ..
        }
    )));
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert!(snapshot.queued_inputs.is_empty());
    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    assert_request_extension(&requests[0], &requests[1]);
    assert!(
        requests[1].messages.iter().any(
            |message| matches!(message, Message::User { text } if text.contains("interrupted"))
        )
    );
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "unfinished" && text.contains("interrupted"))));
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "discard this"))
    );
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn user_interrupt_preserves_partial_history_and_normal_item_status_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let partial = vec![
        call(
            "unfinished",
            "shell",
            r#"{"command":"touch must-not-exist"}"#,
        ),
        ModelEvent::ReasoningDelta {
            item_id: "reasoning".into(),
            delta: "partial thought".into(),
        },
        ModelEvent::TextDelta {
            item_id: "text".into(),
            delta: "partial answer".into(),
        },
    ];
    let model = ScriptModel::new(vec![
        partial,
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session
        .input(input("start"))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    until(&mut events, |payload| {
        matches!(
            payload,
            EventPayload::ItemDelta {
                delta: ItemDelta::AgentMessage { .. },
                ..
            }
        )
    })
    .await;
    session
        .interrupt(SessionInterruptParams {
            turn_id: turn,
            retract: None,
        })
        .await
        .unwrap();
    finished(&mut events).await;
    let interrupted = session.snapshot().await.session.turns[0].clone();
    assert_eq!(interrupted.status, TurnStatus::Interrupted);
    assert!(interrupted.items.iter().any(|item| matches!(item, Item::AgentMessage { text, status: None, .. } if text == "partial answer")));
    assert!(interrupted.items.iter().any(|item| matches!(item, Item::Reasoning { content, status: None, .. } if content == &vec!["partial thought".to_string()])));
    for item in &interrupted.items {
        if matches!(item, Item::AgentMessage { .. } | Item::Reasoning { .. }) {
            assert!(serde_json::to_value(item).unwrap().get("status").is_none());
        }
    }
    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    let assistant = requests[1]
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Assistant { content } => Some(content),
            _ => None,
        })
        .unwrap();
    assert!(assistant.contains(&AssistantContent::Text {
        text: "partial answer".into()
    }));
    assert!(assistant.contains(&AssistantContent::Reasoning {
        text: "partial thought".into()
    }));
    assert!(
        assistant.iter().any(
            |part| matches!(part, AssistantContent::ToolCall(call) if call.id == "unfinished")
        )
    );
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "unfinished" && text.contains("interrupted"))));
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
    let (reopened, response) = Session::open(
        store,
        "s1".into(),
        RuntimeContext::default(),
        ScriptModel::new(vec![]),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(response.session.turns[0], interrupted);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn interrupt_while_waiting_for_approval_cancels_the_request_without_running_tools() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![
        call("pending", "shell", r#"{"command":"touch must-not-exist"}"#),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AlwaysAsk;
    let (session, _) = Session::create(
        store,
        "s1".into(),
        config,
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.input(input("run")).await.unwrap();
    until(&mut events, |payload| {
        matches!(payload, EventPayload::RequestOpened { .. })
    })
    .await;
    assert_eq!(
        session.snapshot().await.session.status,
        SessionStatus::WaitingApproval
    );
    session
        .interrupt(SessionInterruptParams {
            retract: None,
            turn_id: response.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(
        event.payload,
        EventPayload::RequestResolved {
            resolution: ApprovalResolution::Cancelled,
            ..
        }
    )));
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert!(session.snapshot().await.session.pending_requests.is_empty());
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn two_clients_resolve_once_and_steering_precedes_the_next_model_request() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call("run", "shell", r#"{"command":"printf x >> count"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "steered answer".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "queued answer".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AskForExecution;
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        config,
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let first = session.input(input("run")).await.unwrap();
    let opened = until(&mut events, |payload| {
        matches!(payload, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = opened.payload else {
        unreachable!()
    };
    let queued = session.input(input("queued follow-up")).await.unwrap();
    let steer = session
        .input(SessionInputParams {
            review: None,
            mode: InputMode::Steer,
            ..input("change plan")
        })
        .await
        .unwrap();
    assert_eq!(steer.turn_id, first.turn_id);
    assert!(queued.turn_id.is_none());
    let params = RequestResolveParams {
        execpolicy_amendment: None,
        plan: None,
        feedback: None,
        decision: Some(ApprovalDecision::Accept),
        answer: None,
        client_name: "client-a".into(),
    };
    let (a, b) = tokio::join!(
        session.resolve(&request.id, params.clone()),
        session.resolve(
            &request.id,
            RequestResolveParams {
                execpolicy_amendment: None,
                plan: None,
                client_name: "client-b".into(),
                ..params
            }
        )
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.event_seq, b.event_seq);
    assert_ne!(a.already_resolved, b.already_resolved);
    let mut seen = finished(&mut events).await;
    seen.extend(finished(&mut events).await);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("count")).unwrap(),
        "x"
    );
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event.payload, EventPayload::RequestResolved { .. }))
            .count(),
        1
    );
    assert!(seen.iter().any(|event| event.seq == steer.event_seq
        && matches!(event.payload, EventPayload::InputQueued { .. })));
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::InputRemoved { reason: InputRemovalReason::Steered, turn_id, .. } if turn_id == &first.turn_id)));
    assert!(session.snapshot().await.session.queued_inputs.is_empty());
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    for pair in requests.windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "change plan"))
    );
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "queued follow-up"))
    );
    assert!(
        matches!(requests[2].messages.last(), Some(Message::User { text }) if text == "queued follow-up")
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupt_during_a_command_waits_until_its_process_group_is_gone() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![
        call(
            "running",
            "shell",
            r#"{"command":"sleep 60 & printf '%s %s\\n' \"$$\" \"$!\"; wait"}"#,
        ),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]]);
    let (session, _) = Session::create(
        store,
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.input(input("run")).await.unwrap();
    let output = until(&mut events, |payload| {
        matches!(
            payload,
            EventPayload::ItemDelta {
                delta: ItemDelta::CommandExecution { .. },
                ..
            }
        )
    })
    .await;
    let EventPayload::ItemDelta {
        delta: ItemDelta::CommandExecution { delta },
        ..
    } = output.payload
    else {
        unreachable!()
    };
    let pids: Vec<i32> = delta
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    tokio::time::timeout(
        Duration::from_secs(5),
        session.interrupt(SessionInterruptParams {
            retract: None,
            turn_id: response.turn_id.unwrap(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    for pid in pids {
        tokio::time::timeout(Duration::from_secs(5), async {
            while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command process survived interruption");
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn reopening_restores_the_exact_model_context_and_skips_future_history_entries() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project/subdir");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(temp.path().join("project/.git")).unwrap();
    std::fs::write(temp.path().join("project/AGENTS.md"), "root instruction").unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "nested instruction").unwrap();
    let mut contexts = Vec::new();
    for restore in [false, true] {
        let root = temp
            .path()
            .join(if restore { "restored" } else { "continuous" });
        let store = Arc::new(Store::new(&root).unwrap());
        let model = ScriptModel::new(vec![
            vec![
                ModelEvent::ReasoningDelta {
                    item_id: "reasoning".into(),
                    delta: "read instructions".into(),
                },
                call("read", "read", "{ \"path\": \"AGENTS.md\" }"),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "first answer".into(),
                },
                ModelEvent::Finished(FinishReason::Stop),
            ],
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "second answer".into(),
                },
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ]);
        let runtime = || RuntimeContext {
            environment: BTreeMap::from([("PRIVATE_FIXTURE".into(), "not-for-disk".into())]),
            protected_paths: vec![root.clone()],
            ..RuntimeContext::default()
        };
        let (mut session, _) = Session::create(
            store.clone(),
            "s1".into(),
            settings(&cwd),
            runtime(),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("read instructions")).await.unwrap();
        finished(&mut events).await;
        if restore {
            let closed = session.close().await.unwrap();
            drop(session);
            let mut writer = store.open("s1").unwrap();
            writer
                .append_model_history(
                    &serde_json::json!({"type":"future_history_kind", "payload":{"new":true}}),
                )
                .unwrap();
            writer.append_model_history(&serde_json::json!({"type":"message", "message":{"type":"future_message_kind"}})).unwrap();
            drop(writer);
            (session, _) = Session::open(
                store,
                "s1".into(),
                runtime(),
                model.clone(),
                CoreOptions::default(),
            )
            .await
            .unwrap();
            events = session.subscribe(closed.event_seq).await.unwrap();
        }
        session.input(input("continue")).await.unwrap();
        finished(&mut events).await;
        contexts.push(model.requests.lock().unwrap()[2].clone());
        assert!(contexts.last().unwrap().messages.iter().any(|message| matches!(message, Message::System { text } if text.contains("root instruction") && text.contains("nested instruction"))));
        session.close().await.unwrap();
        for file in ["events.jsonl", "model-history.jsonl"] {
            assert!(
                !std::fs::read_to_string(root.join("s1").join(file))
                    .unwrap()
                    .contains("not-for-disk")
            );
        }
        assert!(!root.join("s1/private.json").exists());
    }
    assert_eq!(contexts[0], contexts[1]);
}

#[tokio::test]
async fn automatic_compaction_keeps_recent_user_words_and_can_continue_after_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "old output ".repeat(16_000),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "Summary: first task completed; continue with the second task.".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "after compaction".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "resumed".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let options = CoreOptions {
        context_window: 100_000,
        max_output_tokens: 32_768,
        ..CoreOptions::default()
    };
    let mut session_settings = settings(temp.path());
    session_settings.reasoning_effort = ReasoningEffort::None;
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        session_settings,
        RuntimeContext::default(),
        model.clone(),
        options.clone(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session
        .input(input("first task, preserve these exact words"))
        .await
        .unwrap();
    finished(&mut events).await;
    std::fs::write(
        temp.path().join("AGENTS.md"),
        "changed before automatic compaction",
    )
    .unwrap();
    session
        .input(input("continue with the second task"))
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::ContextCompaction { summary, .. }, .. } if summary.contains("first task completed"))));
    let requests = model.requests.lock().unwrap().clone();
    assert!(
        requests[1].tools.is_empty(),
        "summarization must not execute tools"
    );
    let compacted = &requests[2].messages;
    assert!(compacted.iter().any(|message| matches!(message, Message::User { text } if text == "first task, preserve these exact words")));
    assert!(compacted.iter().any(|message| matches!(message, Message::User { text } if text == "continue with the second task")));
    assert!(!format!("{compacted:?}").contains("old output old output"));
    std::fs::remove_file(temp.path().join("AGENTS.md")).unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store,
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        options,
    )
    .await
    .unwrap();
    let mut events = session.subscribe(closed.event_seq).await.unwrap();
    session.input(input("resume")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    assert_request_extension(&requests[2], &requests[3]);
    assert_eq!(requests[0].messages[0], requests[1].messages[0]);
    assert_eq!(requests[0].messages[0], requests[2].messages[0]);
    let resumed = requests[3].messages.clone();
    assert_eq!(&resumed[..compacted.len()], compacted);
    session.close().await.unwrap();
}

#[tokio::test]
async fn auto_compact_tokens_sets_the_compaction_line_and_usage_reports_it() {
    // The window's own line: 1_048_576 − 65_536 − 16_384.
    const WINDOW_LINE: u64 = 966_656;
    // (configured line, context the first answer reports, compacts, line in effect)
    for (auto, measured, compacts, line) in [
        (Some(200_000), 300_000, true, 200_000),
        (Some(400_000), 300_000, false, 400_000),
        (Some(2_000_000), 970_000, true, WINDOW_LINE),
        (None, 300_000, false, WINDOW_LINE),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let reply = |text: &str| {
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: text.into(),
                },
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        let mut first = reply("first");
        first.insert(
            1,
            ModelEvent::Usage(canter_model::Usage {
                input_tokens: measured,
                output_tokens: 10,
                total_tokens: measured + 10,
                cache_read_tokens: None,
                cache_miss_tokens: None,
                reasoning_tokens: None,
            }),
        );
        let mut scripts = vec![first];
        if compacts {
            scripts.push(reply("Summary: the first task."));
        }
        scripts.push(reply("second"));
        let model = ScriptModel::new(scripts);
        let (session, _) = Session::create(
            store,
            "s1".into(),
            settings(temp.path()),
            RuntimeContext::default(),
            model.clone(),
            CoreOptions {
                auto_compact_tokens: auto,
                ..CoreOptions::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("first task")).await.unwrap();
        let seen = finished(&mut events).await;
        let usage = seen
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::SessionUsage { token_usage } => Some(token_usage.clone()),
                _ => None,
            })
            .expect("usage reported");
        assert_eq!(usage.auto_compact_tokens, Some(line), "{auto:?}");
        assert_eq!(
            serde_json::to_value(&usage).unwrap()["autoCompactTokens"],
            line,
            "{auto:?}"
        );
        session.input(input("second task")).await.unwrap();
        let seen = finished(&mut events).await;
        let compacted = seen.iter().any(|event| {
            matches!(
                &event.payload,
                EventPayload::ItemCompleted {
                    item: Item::ContextCompaction { .. },
                    ..
                }
            )
        });
        assert_eq!(compacted, compacts, "{auto:?} at {measured}");
        assert_eq!(
            model.requests.lock().unwrap().len(),
            if compacts { 3 } else { 2 },
            "{auto:?}"
        );
        session.close().await.unwrap();
    }
}
fn call(id: &str, name: &str, arguments: &str) -> ModelEvent {
    ModelEvent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    })
}
fn input(text: &str) -> SessionInputParams {
    SessionInputParams {
        review: None,
        input: vec![UserInput::Text { text: text.into() }],
        mode: InputMode::Queue,
        client_name: "terminal".into(),
    }
}
fn settings(cwd: &std::path::Path) -> SessionSettings {
    SessionSettings {
        non_interactive: false,
        tags: Default::default(),
        additional_directories: Vec::new(),
        plan_mode: false,
        pre_plan_approval_policy: None,
        plan_enabled: None,
        sandbox_enabled: None,
        cwd: cwd.to_string_lossy().into(),
        name: None,
        model: "fake".into(),
        reasoning_effort: Default::default(),
        approval_policy: PermissionMode::AllowAll,
    }
}
async fn finished(events: &mut canter_core::Subscription) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut seen = Vec::new();
        loop {
            let event = events
                .recv()
                .await
                .unwrap()
                .expect("event stream closed early");
            let done = matches!(event.payload, EventPayload::TurnCompleted { .. });
            seen.push(event);
            if done {
                return seen;
            }
        }
    })
    .await
    .expect("turn did not finish")
}

#[tokio::test]
async fn one_turn_runs_multiple_real_tools_and_publishes_the_durable_events() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Keep edits small.").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::ReasoningDelta {
                item_id: "reasoning".into(),
                delta: "Inspect and edit.".into(),
            },
            call(
                "w",
                "write",
                "{ \"path\": \"file\", \"content\": \"before\" }",
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            call(
                "e",
                "edit",
                r#"{"path":"file","edits":[{"oldText":"before","newText":"after"}]}"#,
            ),
            call("r", "read", r#"{"path":"file"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            call(
                "s",
                "shell",
                r#"{"command":"printf '%s' \"$CORE_TEST_VALUE\""}"#,
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "Done".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let runtime = RuntimeContext {
        environment: BTreeMap::from([("CORE_TEST_VALUE".into(), "isolated".into())]),
        protected_paths: vec![temp.path().join("sessions")],
        ..RuntimeContext::default()
    };
    let (session, created) = Session::create(
        store.clone(),
        "s1".into(),
        settings(&cwd),
        runtime,
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session
        .input(input("edit then read and run a command"))
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert_eq!(seen[created.event_seq as usize - 1].seq, created.event_seq);
    assert!(seen.iter().any(|event| event.seq == response.event_seq
        && matches!(event.payload, EventPayload::TurnStarted { .. })));
    assert_eq!(std::fs::read_to_string(cwd.join("file")).unwrap(), "after");
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    let disk = store.events_after("s1", 0).unwrap();
    assert_eq!(&disk[..seen.len()], seen.as_slice());
    assert!(seen.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    for pair in requests.windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(requests[0].messages.iter().any(|message| matches!(message, Message::System { text } if text.contains("Keep edits small.") && text.contains(&*cwd.to_string_lossy()))));
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::Assistant { content } if content.contains(&AssistantContent::Reasoning { text: "Inspect and edit.".into() }))));
    assert!(requests[2].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "r" && text.contains("1: after"))));
    assert!(requests[3].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "s" && text.contains("isolated"))));
    assert!(!temp.path().join("sessions/s1/private.json").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn rewind_after_reopen_uses_filtered_history_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let model = ScriptModel::new(
        (0..4)
            .map(|_| {
                vec![
                    ModelEvent::TextDelta {
                        item_id: "text".into(),
                        delta: "done".into(),
                    },
                    ModelEvent::Finished(FinishReason::Stop),
                ]
            })
            .collect(),
    );
    let runtime = || RuntimeContext {
        environment: BTreeMap::from([
            ("HOME".into(), temp.path().to_string_lossy().into_owned()),
            (
                "CANTER_HOME".into(),
                temp.path().join("state").to_string_lossy().into_owned(),
            ),
        ]),
        ..Default::default()
    };
    let (session, _) = Session::create(
        store.clone(),
        "rewind".into(),
        settings(temp.path()),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("retained")).await.unwrap();
    finished(&mut events).await;
    session.close().await.unwrap();
    drop(session);
    let mut writer = store.open("rewind").unwrap();
    for message in [
        serde_json::json!({"type":"future_message_kind"}),
        serde_json::json!({"type":"assistant","content":[{"type":"future_content_kind"}]}),
    ] {
        writer
            .append_model_history(
                &serde_json::json!({"type":"message","message":message,"user_input":true}),
            )
            .unwrap();
    }
    drop(writer);
    let (session, _) = Session::open(
        store.clone(),
        "rewind".into(),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    let target = session
        .input(input("remove this"))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    finished(&mut events).await;
    session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store.clone(),
        "rewind".into(),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut params: SessionRewindParams = serde_json::from_value(
        serde_json::json!({"beforeTurnId":target,"rewindFiles":false,"rewindConversation":true}),
    )
    .unwrap();
    params.last_event_seq = Some(
        session
            .rewind_preview(params.clone())
            .await
            .unwrap()
            .last_event_seq,
    );
    session.rewind(params).await.unwrap();
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session.input(input("immediate")).await.unwrap();
    finished(&mut events).await;
    let expected = {
        let requests = model.requests.lock().unwrap();
        let old = &requests[1].messages;
        let immediate = &requests[2].messages;
        assert_eq!(
            serde_json::to_vec(&old[..old.len() - 1]).unwrap(),
            serde_json::to_vec(&immediate[..immediate.len() - 1]).unwrap()
        );
        let mut expected = immediate.clone();
        expected.push(Message::Assistant {
            content: vec![AssistantContent::Text {
                text: "done".into(),
            }],
        });
        expected
    };
    session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store,
        "rewind".into(),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session.input(input("after restart")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        let last = &requests[3].messages;
        assert_eq!(
            serde_json::to_vec(&expected).unwrap(),
            serde_json::to_vec(&last[..last.len() - 1]).unwrap()
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn rewind_large_outputs_keeps_small_originals_and_confirms_unknown_changes() {
    let temp = tempfile::tempdir().unwrap();
    let large = "x".repeat(1024 * 1024 + 1);
    std::fs::write(temp.path().join("existing.txt"), "original").unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call(
                "write-existing",
                "write",
                &serde_json::json!({"path":"existing.txt","content":large}).to_string(),
            ),
            call(
                "write-new",
                "write",
                &serde_json::json!({"path":"new.txt","content":large}).to_string(),
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "done".into(),
            },
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let runtime = || RuntimeContext {
        environment: BTreeMap::from([
            ("HOME".into(), temp.path().to_string_lossy().into_owned()),
            (
                "CANTER_HOME".into(),
                temp.path().join("state").to_string_lossy().into_owned(),
            ),
        ]),
        ..Default::default()
    };
    let (session, _) = Session::create(
        store.clone(),
        "large".into(),
        settings(temp.path()),
        runtime(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let target = session
        .input(input("write large files"))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    finished(&mut events).await;
    session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store,
        "large".into(),
        runtime(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut params: SessionRewindParams = serde_json::from_value(
        serde_json::json!({"beforeTurnId":target,"rewindFiles":true,"rewindConversation":false}),
    )
    .unwrap();
    let preview = session.rewind_preview(params.clone()).await.unwrap();
    assert_eq!(preview.files.len(), 2);
    for file in &preview.files {
        assert!(
            file.unavailable.is_none(),
            "the small original (or absence) must remain recoverable"
        );
        let wire = serde_json::to_value(file).unwrap();
        assert_eq!(wire["externalChangeUnknown"], true);
        assert_eq!(wire["delete"], file.path.ends_with("/new.txt"));
    }
    params.last_event_seq = Some(preview.last_event_seq);
    assert_eq!(
        session.rewind(params.clone()).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("existing.txt")).unwrap(),
        large
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("new.txt")).unwrap(),
        large
    );
    params.confirmed_external_changes = preview.files.into_iter().map(|file| file.path).collect();
    session.rewind(params).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("existing.txt")).unwrap(),
        "original"
    );
    assert!(!temp.path().join("new.txt").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn permanent_command_approval_writes_rules_and_runs_this_and_the_next_call() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::ToolCall(ToolCall {
                id: "first".into(),
                name: "shell".into(),
                arguments: r#"{"command":"touch first"}"#.into(),
            }),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::ToolCall(ToolCall {
                id: "second".into(),
                name: "shell".into(),
                arguments: r#"{"command":"touch second"}"#.into(),
            }),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AlwaysAsk;
    let (session, _) = Session::create(
        Arc::new(Store::new(temp.path().join("sessions")).unwrap()),
        "rules".into(),
        config,
        RuntimeContext {
            environment: [("HOME".into(), temp.path().display().to_string())].into(),
            ..Default::default()
        },
        model,
        CoreOptions {
            command_rules_path: Some(file.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("create the markers")).await.unwrap();
    let opened = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = opened.payload else {
        panic!()
    };
    let RequestKind::Approval(approval) = &request.kind else {
        panic!()
    };
    assert_eq!(
        approval.proposed_execpolicy_amendment,
        Some(vec![vec!["touch".into()]])
    );
    let params = RequestResolveParams {
        execpolicy_amendment: approval.proposed_execpolicy_amendment.clone(),
        decision: Some(ApprovalDecision::AcceptWithExecpolicyAmendment),
        plan: None,
        feedback: None,
        answer: None,
        client_name: "terminal".into(),
    };
    session.resolve(&request.id, params).await.unwrap();
    let mut saved_notice = false;
    let next = until(&mut events, |event| {
        matches!(
            event,
            EventPayload::TurnCompleted { .. }
                | EventPayload::RequestOpened { .. }
                | EventPayload::Warning { .. }
        )
    })
    .await;
    let terminal = if let EventPayload::Warning { message } = &next.payload {
        saved_notice = message.contains(&file.display().to_string());
        until(&mut events, |event| {
            matches!(
                event,
                EventPayload::TurnCompleted { .. } | EventPayload::RequestOpened { .. }
            )
        })
        .await
    } else {
        next
    };
    session.close().await.unwrap();
    assert!(
        matches!(terminal.payload, EventPayload::TurnCompleted { .. }),
        "next matching command must not ask again"
    );
    assert!(temp.path().join("first").exists());
    assert!(temp.path().join("second").exists());
    assert!(saved_notice, "conversation must name the saved rules file");
    let rules: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        rules,
        serde_json::json!({"rules":[{"pattern":["touch"],"decision":"allow"}]})
    );
    assert_eq!(
        std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn broken_rules_warn_once_and_recovered_forbidden_rules_apply_to_pending_approvals() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    std::fs::write(&file, "invalid").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call("first", "shell", r#"{"command":"pwd"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            call("second", "shell", r#"{"command":"pwd"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "broken".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions {
            command_rules_path: Some(file.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("check directory")).await.unwrap();
    for index in 0..2 {
        let event = until(&mut events, |event| {
            matches!(event, EventPayload::RequestOpened { .. })
        })
        .await;
        let EventPayload::RequestOpened { request } = event.payload else {
            panic!()
        };
        let RequestKind::Approval(approval) = &request.kind else {
            panic!()
        };
        assert!(
            !approval
                .available_decisions
                .contains(&ApprovalDecision::AcceptForSession)
        );
        assert!(
            !approval
                .available_decisions
                .contains(&ApprovalDecision::AcceptWithExecpolicyAmendment)
        );
        if index == 1 {
            std::fs::write(
                &file,
                r#"{"rules":[{"pattern":["pwd"],"decision":"forbidden"}]}"#,
            )
            .unwrap();
        }
        // An old client supplies only the original fields.
        let answer = serde_json::from_value(
            serde_json::json!({"decision":"accept","clientName":"old-terminal"}),
        )
        .unwrap();
        session.resolve(&request.id, answer).await.unwrap();
    }
    finished(&mut events).await;
    session.close().await.unwrap();
    let warnings: Vec<_> = store.events_after("broken", 0).unwrap().into_iter().filter(|event| matches!(&event.payload, EventPayload::Warning {message} if message.contains("command rules"))).collect();
    assert_eq!(warnings.len(), 1);
    assert!(model.requests.lock().unwrap().last().unwrap().messages.iter().any(|message| matches!(message, Message::ToolResult {tool_call_id,text,..} if tool_call_id == "second" && text.contains("forbidden"))));
}

#[tokio::test]
async fn rules_apply_to_readonly_plan_and_review_shell_but_never_to_user_shell() {
    for (plan, review) in [(false, false), (true, false), (false, true)] {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("rules.json");
        std::fs::write(&file, r#"{"rules":[{"pattern":["pwd"],"decision":"forbidden"},{"pattern":["touch"],"decision":"forbidden"}]}"#).unwrap();
        let model = ScriptModel::new(vec![
            vec![
                call("denied", "shell", r#"{"command":"pwd","background":true}"#),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![ModelEvent::Finished(FinishReason::Stop)],
        ]);
        let mut config = settings(temp.path());
        config.plan_mode = plan;
        let (session, _) = Session::create(
            Arc::new(Store::new(temp.path().join("sessions")).unwrap()),
            "restricted".into(),
            config,
            RuntimeContext::default(),
            model.clone(),
            CoreOptions {
                command_rules_path: Some(file),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        let mut params = input("inspect");
        params.review = review.then(|| "review current work".into());
        session.input(params).await.unwrap();
        let seen = finished(&mut events).await;
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
        );
        assert!(model.requests.lock().unwrap().last().unwrap().messages.iter().any(|message| matches!(message, Message::ToolResult {text,..} if text.contains("forbidden"))), "plan={plan} review={review}");
        session
            .shell_command(SessionShellCommandParams {
                command: "touch user-marker".into(),
                timeout_ms: None,
            })
            .await
            .unwrap();
        finished(&mut events).await;
        assert!(temp.path().join("user-marker").exists());
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn failed_rule_save_leaves_the_request_pending_and_single_approval_still_works() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let model = ScriptModel::new(vec![
        vec![
            call("first", "shell", r#"{"command":"touch marker"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AskForExecution;
    let (session, _) = Session::create(
        Arc::new(Store::new(temp.path().join("sessions")).unwrap()),
        "save-failure".into(),
        config,
        RuntimeContext::default(),
        model,
        CoreOptions {
            command_rules_path: Some(file.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("create marker")).await.unwrap();
    let event = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = event.payload else {
        panic!()
    };
    let mut params: RequestResolveParams = serde_json::from_value(
        serde_json::json!({"decision":"acceptWithExecpolicyAmendment","clientName":"terminal"}),
    )
    .unwrap();
    assert_eq!(
        session
            .resolve(&request.id, params.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidDecision
    );
    params.execpolicy_amendment = Some(vec![vec!["touch".into()]]);
    std::fs::create_dir(&file).unwrap();
    assert_eq!(
        session.resolve(&request.id, params).await.unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(session.snapshot().await.session.pending_requests.len(), 1);
    assert!(!temp.path().join("marker").exists());
    session
        .resolve(
            &request.id,
            serde_json::from_value(
                serde_json::json!({"decision":"accept","clientName":"terminal"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    finished(&mut events).await;
    assert!(temp.path().join("marker").exists());
    session.close().await.unwrap();
}
