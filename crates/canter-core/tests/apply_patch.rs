#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{
    FinishReason, Message, Model, ModelContext, ModelError, ModelEvent, ModelStream, Request,
    Responses, ResponsesAuth, ResponsesCredential, ToolCall,
};
use canter_protocol::*;
use canter_store::Store;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{KEY, Reply, Server};
use tokio_util::sync::CancellationToken;

struct Auth;
impl ResponsesAuth for Auth {
    fn credential<'a>(
        &'a self,
        _: &'a Request,
        _: Option<&'a ModelContext>,
    ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>> {
        Box::pin(async { Ok(Box::new(Credential) as Box<dyn ResponsesCredential>) })
    }
}
struct Credential;
impl ResponsesCredential for Credential {
    fn authorization(&mut self) -> BoxFuture<'_, Result<axum::http::HeaderValue, ModelError>> {
        Box::pin(async { Ok(axum::http::HeaderValue::from_str(&format!("Bearer {KEY}")).unwrap()) })
    }
    fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>> {
        Box::pin(async {
            Err(ModelError {
                kind: canter_model::ErrorKind::Authentication,
                status: None,
                retry_after: None,
                retries: 0,
            })
        })
    }
    fn replay_account(&self) -> String {
        "test-account".into()
    }
}

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
    fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(async_stream::stream! {
            for event in events {
                yield Ok(event);
            }
        })
    }
}

fn patch_call(patch: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCall(ToolCall {
            id: "call-patch".into(),
            name: "apply_patch".into(),
            arguments: patch.into(),
        }),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]
}

fn input(review: Option<&str>) -> SessionInputParams {
    SessionInputParams {
        from_suggestion: false,
        non_interactive: false,
        approval_policy: None,
        review: review.map(str::to_owned),
        input: vec![UserInput::Text {
            text: "Apply the requested patch.".into(),
        }],
        mode: InputMode::Queue,
        client_name: "test".into(),
    }
}

fn settings(root: &Path, model: &str, approval_policy: PermissionMode) -> SessionSettings {
    serde_json::from_value(json!({
        "cwd":root,
        "model":model,
        "name":null,
        "approvalPolicy":approval_policy
    }))
    .unwrap()
}

fn runtime(root: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            ("HOME".into(), root.join("home").display().to_string()),
            (
                "CANTER_HOME".into(),
                root.join("state").display().to_string(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}

async fn create(
    root: &Path,
    model: Arc<dyn Model>,
    approval_policy: PermissionMode,
    plan_mode: bool,
) -> (Arc<Store>, Session) {
    for directory in ["home", "state", "project/.git"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let store = Arc::new(Store::new(root.join("state/sessions")).unwrap());
    let mut settings = settings(&root.join("project"), "chatgpt/test", approval_policy);
    settings.plan_mode = plan_mode;
    let (session, _) = Session::create(
        store.clone(),
        "apply-patch".into(),
        settings,
        runtime(root),
        model,
        CoreOptions {
            apply_patch_models: ["chatgpt/test".into()].into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (store, session)
}

async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if predicate(&event.payload) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}

async fn finished(events: &mut Subscription) -> TurnStatus {
    let event = until(events, |event| {
        matches!(event, EventPayload::TurnCompleted { .. })
    })
    .await;
    let EventPayload::TurnCompleted { turn } = event.payload else {
        unreachable!()
    };
    turn.status
}

async fn approve(session: &Session, events: &mut Subscription) -> ApprovalRequest {
    let event = until(events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = event.payload else {
        unreachable!()
    };
    let RequestKind::Approval(approval) = &request.kind else {
        panic!("expected file approval")
    };
    let approval = approval.clone();
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                decision: Some(ApprovalDecision::Accept),
                feedback: None,
                execpolicy_amendment: None,
                plan: None,
                answer: None,
                client_name: "test".into(),
            },
        )
        .await
        .unwrap();
    approval
}

async fn rewind_files(session: &Session, turn_id: &str) -> RewindPreviewResponse {
    let mut params = SessionRewindParams {
        before_turn_id: turn_id.into(),
        rewind_files: true,
        rewind_conversation: false,
        confirmed_external_changes: Vec::new(),
        last_event_seq: None,
    };
    let preview = session.rewind_preview(params.clone()).await.unwrap();
    params.last_event_seq = Some(preview.last_event_seq);
    session.rewind(params).await.unwrap();
    preview
}

fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(&event).unwrap()))
        .collect()
}

#[tokio::test]
async fn responses_custom_patch_has_one_approval_replays_and_rewinds_every_change() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("nested")).unwrap();
    std::fs::write(project.join("a.txt"), "old a\n").unwrap();
    std::fs::write(project.join("delete.txt"), "delete me\n").unwrap();
    std::fs::write(project.join("move.txt"), "old move\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old a\n+new a\n*** Add File: nested/new.txt\n+created\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: renamed.txt\n@@\n-old move\n+new move\n*** End Patch";
    let call = json!({
        "type":"custom_tool_call", "id":"custom-1", "call_id":"call-patch",
        "namespace":"canter", "name":"apply_patch", "input":patch, "status":"completed"
    });
    let server = Server::start(vec![
        Reply::sse(sse(vec![
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"custom_tool_call","id":"custom-1","call_id":"call-patch","namespace":"canter","name":"apply_patch","input":""}}),
            json!({"type":"response.custom_tool_call_input.delta","output_index":0,"item_id":"custom-1","delta":patch}),
            json!({"type":"response.output_item.done","output_index":0,"item":call}),
            json!({"type":"response.completed","response":{"status":"completed","output":[call]}}),
        ])),
        Reply::sse(sse(vec![json!({
            "type":"response.completed","response":{"status":"completed","output":[]}
        })])),
    ])
    .await;
    let model = Responses::for_test(
        Arc::new(Auth),
        format!("{}responses", server.base_url).parse().unwrap(),
    )
    .unwrap();
    let (_, session) = create(
        temp.path(),
        Arc::new(model),
        PermissionMode::AlwaysAsk,
        false,
    )
    .await;
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session.input(input(None)).await.unwrap().turn_id.unwrap();
    let approval = approve(&session, &mut events).await;
    assert_eq!(approval.tool, "apply_patch");
    assert_eq!(approval.arguments["changes"].as_array().unwrap().len(), 4);
    assert_eq!(finished(&mut events).await, TurnStatus::Completed);

    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "new a\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("nested/new.txt")).unwrap(),
        "created\n"
    );
    assert!(!project.join("delete.txt").exists());
    assert!(!project.join("move.txt").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("renamed.txt")).unwrap(),
        "new move\n"
    );
    let snapshot = session.snapshot().await;
    assert!(
        snapshot.session.turns[0]
            .items
            .iter()
            .any(|item| { matches!(item, Item::FileChange { changes, .. } if changes.len() == 4) })
    );
    assert_eq!(server.arrivals.lock().unwrap().len(), 2);
    let requests = server.requests.lock().unwrap().clone();
    let tools = requests[0]["tools"][0]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| { tool["type"] == "custom" && tool["name"] == "apply_patch" })
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["name"] != "write" && tool["name"] != "edit")
    );
    let definition = tools
        .iter()
        .find(|tool| tool["name"] == "apply_patch")
        .unwrap()["format"]["definition"]
        .as_str()
        .unwrap();
    assert!(!definition.contains("Derived from openai/codex"));
    assert!(!definition.contains("83086188ae0614bd882818ff1aa23f3194962012"));
    assert!(!definition.contains("Licensed under Apache-2.0"));
    let input = requests[1]["input"].as_array().unwrap();
    assert!(input.contains(&call));
    assert!(input.iter().any(|item| {
        item["type"] == "custom_tool_call_output" && item["call_id"] == "call-patch"
    }));
    drop(requests);

    let preview = rewind_files(&session, &turn).await;
    assert_eq!(preview.files.len(), 5);
    assert!(preview.files.iter().all(|file| !file.external_change));
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "old a\n"
    );
    assert!(!project.join("nested/new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("delete.txt")).unwrap(),
        "delete me\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("move.txt")).unwrap(),
        "old move\n"
    );
    assert!(!project.join("renamed.txt").exists());
    session.close().await.unwrap();
    server.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn partial_failure_keeps_and_rewinds_only_committed_files() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("first")).unwrap();
    std::fs::create_dir_all(project.join("second")).unwrap();
    std::fs::write(project.join("first/a.txt"), "old a\n").unwrap();
    std::fs::write(project.join("second/b.txt"), "old b\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: first/a.txt\n@@\n-old a\n+new a\n*** Update File: second/b.txt\n@@\n-old b\n+new b\n*** End Patch";
    let model = ScriptModel::new(vec![
        patch_call(patch),
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (_, session) = create(temp.path(), model.clone(), PermissionMode::AlwaysAsk, false).await;
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session.input(input(None)).await.unwrap().turn_id.unwrap();
    let request = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    std::fs::set_permissions(
        project.join("second"),
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let EventPayload::RequestOpened { request } = request.payload else {
        unreachable!()
    };
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                decision: Some(ApprovalDecision::Accept),
                feedback: None,
                execpolicy_amendment: None,
                plan: None,
                answer: None,
                client_name: "test".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await, TurnStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(project.join("first/a.txt")).unwrap(),
        "new a\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("second/b.txt")).unwrap(),
        "old b\n"
    );
    let requests = model.requests.lock().unwrap().clone();
    let result = requests[1]
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult { text, .. } => Some(text.as_str()),
            _ => None,
        });
    assert!(result.is_some_and(|text| {
        text.contains("Applied and left in place")
            && text.contains("first/a.txt")
            && text.contains("Not applied")
            && text.contains("second/b.txt")
    }));
    drop(requests);
    let snapshot = session.snapshot().await;
    assert!(snapshot.session.turns[0].items.iter().any(|item| {
        matches!(item, Item::FileChange { changes, status: ItemStatus::Failed, .. } if changes.len() == 1 && changes[0].path == "first/a.txt")
    }));

    std::fs::set_permissions(
        project.join("second"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let preview = rewind_files(&session, &turn).await;
    assert_eq!(preview.files.len(), 1);
    assert_eq!(
        std::fs::read_to_string(project.join("first/a.txt")).unwrap(),
        "old a\n"
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn switching_models_switches_tools_and_request_grammar() {
    let temp = tempfile::tempdir().unwrap();
    let model = ScriptModel::new(vec![
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![ModelEvent::Finished(FinishReason::Stop)],
        vec![ModelEvent::Finished(FinishReason::Stop)],
    ]);
    let (_, session) = create(temp.path(), model.clone(), PermissionMode::AllowAll, false).await;
    for next in [None, Some("other/model"), Some("chatgpt/test")] {
        if let Some(next) = next {
            session
                .update(SessionUpdateParams {
                    model: Some(next.into()),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let mut events = session
            .subscribe(session.snapshot().await.event_seq)
            .await
            .unwrap();
        session.input(input(None)).await.unwrap();
        assert_eq!(finished(&mut events).await, TurnStatus::Completed);
    }
    let requests = model.requests.lock().unwrap().clone();
    for (index, apply_patch) in [true, false, true].into_iter().enumerate() {
        let names: Vec<_> = requests[index]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names.contains(&"apply_patch"), apply_patch);
        assert_eq!(names.contains(&"write"), !apply_patch);
        assert_eq!(names.contains(&"edit"), !apply_patch);
        assert_eq!(
            requests[index].custom_tools.contains_key("apply_patch"),
            apply_patch
        );
        let Message::System { text } = &requests[index].messages[0] else {
            panic!("missing system prompt")
        };
        assert!(text.contains("file changes"));
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn review_and_plan_turns_reject_apply_patch_before_writing() {
    for (review, plan) in [(true, false), (false, true)] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "old\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End Patch";
        let model = ScriptModel::new(vec![
            patch_call(patch),
            vec![ModelEvent::Finished(FinishReason::Stop)],
        ]);
        let (_, session) = create(temp.path(), model.clone(), PermissionMode::AllowAll, plan).await;
        let mut events = session.subscribe(0).await.unwrap();
        session
            .input(input(review.then_some("Review the patch")))
            .await
            .unwrap();
        assert_eq!(finished(&mut events).await, TurnStatus::Completed);
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "old\n"
        );
        let requests = model.requests.lock().unwrap().clone();
        assert!(requests[1].messages.iter().any(|message| matches!(
            message,
            Message::ToolResult { text, .. } if text.contains("read-only") || text.contains("Plan mode")
        )));
        drop(requests);
        session.close().await.unwrap();
    }
}
