#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn reply() -> Reply {
    Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Summary or answer.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )
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

fn install(root: &Path, name: &str, disabled: bool, body: &str) -> std::path::PathBuf {
    let path = root.join(format!("project/.agents/skills/{name}/SKILL.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("---\nname: {name}\ndescription: Example instructions\ndisable-model-invocation: {disabled}\n---\n{body}")).unwrap();
    path
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
}

impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in ["home", "state", "project/.git"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        install(root.path(), "example", false, "EXAMPLE_BODY_V1");
        install(root.path(), "manual", true, "MANUAL_ONLY_BODY");
        let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
        let server = Server::start(replies).await;
        let (session, _) = Session::create(
            store.clone(), "skills".into(),
            serde_json::from_value(json!({"cwd":root.path().join("project"),"model":"fake","name":null,"approvalPolicy":"allowAll"})).unwrap(),
            runtime(root.path()),
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
            CoreOptions::default(),
        ).await.unwrap();
        Self {
            root,
            store,
            session,
            server,
        }
    }

    async fn send(&self, input: Value) -> String {
        let mut events = self
            .session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap();
        let response = self
            .session
            .input(params(input))
            .await
            .expect("skill input must be accepted");
        finished(&mut events).await;
        response.turn_id.unwrap()
    }

    async fn close(self) {
        self.session.close().await.unwrap();
        self.server.stop().await;
    }

    async fn compact(&self) {
        let mut events = self
            .session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap();
        self.session.compact().await.unwrap();
        finished(&mut events).await;
    }

    async fn reopen(&mut self) {
        self.session.close().await.unwrap();
        self.session = Session::open(
            self.store.clone(),
            "skills".into(),
            runtime(self.root.path()),
            Arc::new(ChatCompletions::new(&self.server.base_url, ApiKey::new(KEY)).unwrap()),
            CoreOptions::default(),
        )
        .await
        .unwrap()
        .0;
    }
}

fn params(input: Value) -> SessionInputParams {
    serde_json::from_value(json!({"input":input,"clientName":"test"})).unwrap()
}

async fn finished(events: &mut Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
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

#[tokio::test]
async fn named_skills_send_full_server_content_and_keep_public_input_small() {
    let fixture = Fixture::new(vec![reply(), reply()]).await;
    for (name, body) in [
        ("example", "EXAMPLE_BODY_V1"),
        ("manual", "MANUAL_ONLY_BODY"),
    ] {
        let input = json!([{"type":"skill","name":name},{"type":"text","text":"Please inspect the project."}]);
        fixture.send(input.clone()).await;
        let request = fixture
            .server
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        let messages = request["messages"].as_array().unwrap();
        let text = messages.last().unwrap()["content"].as_str().unwrap();
        let path = fixture
            .root
            .path()
            .join(format!("project/.agents/skills/{name}/SKILL.md"));
        assert!(
            text.contains(&format!(
                "<skill>\n<name>{name}</name>\n<path>{}</path>",
                path.display()
            )),
            "{text}"
        );
        assert!(text.contains(&format!(
            "Base directory for this skill: {}",
            path.parent().unwrap().display()
        )));
        assert!(text.contains(body));
        assert!(text.contains("</skill>\nPlease inspect the project."));
        if name == "manual" {
            assert!(
                !messages[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("MANUAL_ONLY_BODY")
            );
        }
        let snapshot = fixture.session.snapshot().await.session;
        let public = serde_json::to_value(snapshot.turns.last().unwrap()).unwrap();
        assert_eq!(public["input"], input);
        assert!(!public.to_string().contains(body));
    }
    {
        let requests = fixture.server.requests.lock().unwrap();
        let earlier = requests[0]["messages"].as_array().unwrap();
        assert_eq!(
            &requests[1]["messages"].as_array().unwrap()[..earlier.len()],
            earlier
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn compaction_restates_only_latest_invoked_snapshot_even_after_file_deletion_and_reopen() {
    let mut fixture = Fixture::new((0..6).map(|_| reply()).collect()).await;
    fixture
        .send(json!([{"type":"skill","name":"example"},{"type":"text","text":"First request"}]))
        .await;
    let path = install(fixture.root.path(), "example", false, "EXAMPLE_BODY_V2");
    fixture
        .send(json!([{"type":"skill","name":"example"}]))
        .await;
    std::fs::remove_file(path).unwrap();
    for reopen in [false, true] {
        if reopen {
            fixture.reopen().await;
        }
        fixture.compact().await;
        fixture
            .send(json!([{"type":"text","text":"Continue"}]))
            .await;
        let requests = fixture.server.requests.lock().unwrap();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let text = messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("EXAMPLE_BODY_V2").count(), 1, "{text}");
        assert!(!text.contains("EXAMPLE_BODY_V1"), "{text}");
        assert!(text.contains("First request"));
        assert!(
            messages[messages.len() - 2]["content"]
                .as_str()
                .unwrap()
                .contains("<skill>")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn rewind_removes_invocations_and_restores_the_previous_version() {
    for keep_previous in [false, true] {
        let mut fixture = Fixture::new((0..8).map(|_| reply()).collect()).await;
        fixture
            .send(json!([{"type":"text","text":"Baseline"}]))
            .await;
        if keep_previous {
            fixture
                .send(json!([{"type":"skill","name":"example"}]))
                .await;
        }
        install(fixture.root.path(), "example", false, "EXAMPLE_BODY_V2");
        let target = fixture
            .send(json!([{"type":"skill","name":"example"}]))
            .await;
        let mut params: SessionRewindParams = serde_json::from_value(json!({
            "beforeTurnId":target,"rewindFiles":false,"rewindConversation":true
        }))
        .unwrap();
        params.last_event_seq = Some(
            fixture
                .session
                .rewind_preview(params.clone())
                .await
                .unwrap()
                .last_event_seq,
        );
        fixture.session.rewind(params).await.unwrap();
        for reopen in [false, true] {
            if reopen {
                fixture.reopen().await;
            }
            fixture.compact().await;
            fixture
                .send(json!([{"type":"text","text":"Continue"}]))
                .await;
            let requests = fixture.server.requests.lock().unwrap();
            let text = requests.last().unwrap()["messages"].to_string();
            assert!(
                !text.contains("EXAMPLE_BODY_V2"),
                "removed invocation was restored"
            );
            assert_eq!(
                text.matches("EXAMPLE_BODY_V1").count(),
                usize::from(keep_previous)
            );
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn retract_removes_skill_before_compaction_and_reopen() {
    let hanging = Reply {
        hang: true,
        ..Reply::sse("")
    };
    let mut fixture =
        Fixture::new(vec![reply(), hanging, reply(), reply(), reply(), reply()]).await;
    fixture
        .send(json!([{"type":"text","text":"Baseline"}]))
        .await;
    let turn = fixture
        .session
        .input(params(json!([{"type":"skill","name":"example"}])))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.server.arrivals.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = fixture
        .session
        .interrupt(serde_json::from_value(json!({"turnId":turn,"retract":true})).unwrap())
        .await
        .unwrap();
    assert_eq!(response.retracted, Some(true));
    for reopen in [false, true] {
        if reopen {
            fixture.reopen().await;
        }
        fixture.compact().await;
        fixture
            .send(json!([{"type":"text","text":"Continue"}]))
            .await;
        assert!(
            !fixture.server.requests.lock().unwrap().last().unwrap()["messages"]
                .to_string()
                .contains("EXAMPLE_BODY_V1")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn unknown_newly_installed_and_unreadable_skills_reject_without_opening_a_turn() {
    let fixture = Fixture::new(Vec::new()).await;
    install(fixture.root.path(), "newly-installed", false, "NEW_BODY");
    std::fs::remove_file(
        fixture
            .root
            .path()
            .join("project/.agents/skills/example/SKILL.md"),
    )
    .unwrap();
    let before = fixture.session.snapshot().await;
    for (name, reason) in [
        ("missing", "not available"),
        ("newly-installed", "not available"),
        ("example", "Cannot read skill"),
    ] {
        let error = fixture
            .session
            .input(params(json!([{"type":"skill","name":name}])))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(
            error.message.contains(reason) && error.message.contains(name),
            "{}",
            error.message
        );
        assert_eq!(fixture.session.snapshot().await.event_seq, before.event_seq);
        assert!(fixture.session.snapshot().await.session.turns.is_empty());
    }
    assert!(fixture.server.requests.lock().unwrap().is_empty());
    fixture.close().await;
}
