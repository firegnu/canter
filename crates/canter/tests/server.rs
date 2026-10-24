use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::post};
use canter_protocol::{Event, EventPayload, SessionCreateResponse, SessionReadResponse};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    process::{Child, Command},
    task::JoinHandle,
};

const KEY: &str = "test-only-canter-key";
const ENV_VALUE: &str = "test-only-client-environment";

struct Fixture {
    root: TempDir,
    child: Child,
    client: Client,
    model: JoinHandle<()>,
}

impl Fixture {
    async fn start(replies: Vec<String>) -> Self {
        // A short absolute path also works within macOS's Unix socket limit.
        let root = tempfile::Builder::new()
            .prefix("canter-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("state");
        fs::create_dir(&home).unwrap();
        fs::create_dir(root.path().join("work")).unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(VecDeque::from(replies)));
        let app = Router::new()
            .route("/chat/completions", post(fake_model))
            .with_state(script);
        let model = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        fs::write(
            home.join("config.json"),
            serde_json::to_vec(&json!({
                "base_url":base_url, "model":"fake-model", "context_window":1048576, "api_key":KEY
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(home.join("config.json"), fs::Permissions::from_mode(0o600)).unwrap();
        let child = Self::spawn(root.path());
        let client = Client::builder()
            .no_proxy()
            .unix_socket(home.join("server.sock"))
            .build()
            .unwrap();
        let mut fixture = Self {
            root,
            child,
            client,
            model,
        };
        fixture.ready().await;
        fixture
    }

    fn spawn(root: &std::path::Path) -> Child {
        Self::spawn_args(root, &["serve"])
    }

    fn spawn_args(root: &std::path::Path, args: &[&str]) -> Child {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))
            .unwrap();
        Command::new(env!("CARGO_BIN_EXE_canter"))
            .args(args)
            .env_clear()
            .env("CANTER_HOME", root.join("state"))
            .env("HOME", root.join("home"))
            .current_dir(root.join("work"))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "server exited: {}",
                    fs::read_to_string(self.root.path().join("server.log")).unwrap()
                );
                if self
                    .client
                    .get("http://localhost/v1/info")
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("server ready");
    }

    async fn request(&self, method: Method, path: &str, body: Value) -> reqwest::Response {
        self.client
            .request(method, format!("http://localhost{path}"))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn create(&self) -> SessionCreateResponse {
        let response = self
            .request(
                Method::POST,
                "/v1/sessions",
                json!({
                    "cwd":self.root.path().join("work"), "env":{"CANTER_TEST_ENV":ENV_VALUE},
                    "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"terminal-a"
                }),
            )
            .await;
        assert_eq!(
            response.status(),
            200,
            "create must be available through HTTP"
        );
        response.json().await.unwrap()
    }

    async fn input(&self, id: &str) {
        let response = self
            .request(
                Method::POST,
                &format!("/v1/sessions/{id}/input"),
                json!({
                    "input":[{"type":"text","text":"private-user-input"}],"clientName":"terminal-a"
                }),
            )
            .await;
        assert_eq!(response.status(), 200);
    }

    async fn events_until(
        &self,
        id: &str,
        after: u64,
        stop: impl Fn(&Event) -> bool,
    ) -> Vec<Event> {
        let response = self
            .client
            .get(format!(
                "http://localhost/v1/events?session={id}&after={after}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut stream = response.bytes_stream().eventsource();
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut events = Vec::new();
            while let Some(frame) = stream.next().await {
                let frame = frame.unwrap();
                let event: Event = serde_json::from_str(&frame.data).unwrap();
                assert_eq!(frame.id, event.seq.to_string());
                assert_eq!(frame.event, "message");
                let done = stop(&event);
                events.push(event);
                if done {
                    return events;
                }
            }
            panic!("SSE ended before the target event");
        })
        .await
        .expect("SSE progress")
    }

    async fn stop(&mut self) {
        let pid = self.child.id().unwrap();
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(15), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        self.model.abort();
        let _ = (&mut self.model).await;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.model.abort();
    }
}

async fn fake_model(
    State(script): State<Arc<Mutex<VecDeque<String>>>>,
    Json(_body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let reply = script
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected model request");
    ([("content-type", "text/event-stream")], reply)
}

fn text_reply() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"hello from fake model"},"finish_reason":"stop"}]})
    )
}

fn tool_reply() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{
        "index":0,"id":"call-shell","type":"function","function":{"name":"shell","arguments":r#"{"command":"printf '%s' \"$CANTER_TEST_ENV\" > client-env.txt"}"#}
    }]},"finish_reason":"tool_calls"}]})
    )
}

#[tokio::test]
async fn create_input_and_events_over_unix_http() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let created = fixture.create().await;
    fixture.input(&created.session.id).await;
    let events = fixture
        .events_until(&created.session.id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::ItemDelta { .. }))
    );
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
    }
    fixture.stop().await;
}

#[tokio::test]
async fn two_clients_observe_one_approval_resolution() {
    let mut fixture = Fixture::start(vec![tool_reply(), text_reply()]).await;
    let created = fixture.create().await;
    let id = &created.session.id;
    fixture.input(id).await;
    let events = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::RequestOpened { .. })
        })
        .await;
    let opened = events.last().unwrap();
    let EventPayload::RequestOpened { request } = &opened.payload else {
        unreachable!()
    };
    let path = format!("/v1/sessions/{id}/requests/{}", request.id);
    let answers = async {
        let first = fixture
            .request(
                Method::POST,
                &path,
                json!({"decision":"accept", "clientName":"terminal-a"}),
            )
            .await;
        assert_eq!(first.status(), 200, "approval must reach the session");
        let first: Value = first.json().await.unwrap();
        let second = fixture
            .request(
                Method::POST,
                &path,
                json!({"decision":"decline", "clientName":"terminal-b"}),
            )
            .await;
        assert_eq!(second.status(), 200);
        let second: Value = second.json().await.unwrap();
        assert_eq!(first["alreadyResolved"], false);
        assert_eq!(second["alreadyResolved"], true);
        assert_eq!(first["eventSeq"], second["eventSeq"]);
    };
    let completed = |event: &Event| matches!(event.payload, EventPayload::TurnCompleted { .. });
    let (a, b, ()) = tokio::join!(
        fixture.events_until(id, opened.seq, completed),
        fixture.events_until(id, opened.seq, completed),
        answers
    );
    assert_eq!(a, b);
    assert_eq!(
        a.iter()
            .filter(|event| matches!(event.payload, EventPayload::RequestResolved { .. }))
            .count(),
        1
    );
    assert!(
        fs::read_to_string(fixture.root.path().join("work/client-env.txt")).unwrap() == ENV_VALUE
    );
    fixture.stop().await;
}

#[tokio::test]
async fn reconnect_replays_strictly_after_last_event_id() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let created = fixture.create().await;
    let id = &created.session.id;
    fixture.input(id).await;
    let all = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let cursor = all[all.len() / 2].seq;
    let response = fixture
        .client
        .get(format!("http://localhost/v1/events?session={id}&after=0"))
        .header("Last-Event-ID", cursor.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream().eventsource();
    for expected in all.iter().filter(|event| event.seq > cursor) {
        let frame = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let actual: Event = serde_json::from_str(&frame.data).unwrap();
        assert_eq!(&actual, expected);
    }
    drop(stream);
    fixture.stop().await;
}

#[tokio::test]
async fn a_second_server_cannot_replace_the_lock_owner() {
    let mut fixture = Fixture::start(vec![]).await;
    let mut second = Fixture::spawn(fixture.root.path());
    let status = tokio::time::timeout(Duration::from_secs(15), second.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    let logs = fs::read_to_string(fixture.root.path().join("server.log")).unwrap();
    assert!(
        logs.contains("已经有一个服务端在运行"),
        "duplicate startup must identify the existing owner"
    );
    assert!(
        fixture
            .client
            .get("http://localhost/v1/info")
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let registration: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("state/server.json")).unwrap())
            .unwrap();
    assert_eq!(registration["pid"], fixture.child.id().unwrap());
    assert_eq!(
        registration["protocolVersion"],
        canter_protocol::PROTOCOL_VERSION
    );
    for path in [
        "state/server.json",
        "state/server.lock",
        "state/config.json",
    ] {
        assert_eq!(
            fs::metadata(fixture.root.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(fixture.root.path().join("state"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fixture.stop().await;
}

#[tokio::test]
async fn restart_requires_resume_and_keeps_the_first_resumed_environment() {
    let mut fixture = Fixture::start(vec![text_reply(), tool_reply(), text_reply()]).await;
    let created = fixture.create().await;
    let id = &created.session.id;
    fixture.input(id).await;
    let before = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.child.kill().await.unwrap();
    fixture.child.wait().await.unwrap();
    fixture.child = Fixture::spawn(fixture.root.path());
    fixture.ready().await;
    let path = format!("/v1/sessions/{id}");
    let response = fixture.request(Method::GET, &path, json!({})).await;
    assert_eq!(
        response.status(),
        200,
        "an unopened session must remain readable after restart"
    );
    let snapshot: SessionReadResponse = response.json().await.unwrap();
    assert_eq!(snapshot.event_seq, snapshot.session.last_event_seq);
    let replay = fixture
        .events_until(id, 0, |event| event.seq == before.last().unwrap().seq)
        .await;
    assert_eq!(replay, before);
    for (method, suffix, body) in [
        (
            Method::POST,
            "/input",
            json!({"input":[{"type":"text","text":"next"}],"clientName":"b"}),
        ),
        (Method::POST, "/interrupt", json!({"turnId":"old"})),
        (
            Method::POST,
            "/requests/old",
            json!({"decision":"accept","clientName":"b"}),
        ),
        (Method::POST, "/compact", json!({})),
        (Method::PATCH, "", json!({"name":"new"})),
        (Method::DELETE, "", json!({})),
    ] {
        let response = fixture
            .request(method, &format!("{path}{suffix}"), body)
            .await;
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.json::<Value>().await.unwrap()["code"],
            "session_not_resumed"
        );
    }
    let unopened_stream = fixture
        .client
        .get(format!(
            "http://localhost/v1/events?session={id}&after={}",
            snapshot.event_seq
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unopened_stream.status(), 200);
    let mut unopened_stream = unopened_stream.bytes_stream().eventsource();
    for value in ["resumed-environment", "ignored-environment"] {
        let response = fixture
            .request(
                Method::POST,
                &format!("{path}/resume"),
                json!({"env":{"CANTER_TEST_ENV":value}}),
            )
            .await;
        assert_eq!(response.status(), 200);
        let response: Value = response.json().await.unwrap();
        assert_eq!(response["eventSeq"], response["session"]["lastEventSeq"]);
    }
    let response = fixture
        .request(Method::PATCH, &path, json!({"approvalPolicy":"allowAll"}))
        .await;
    assert_eq!(response.status(), 200);
    let after = response.json::<Value>().await.unwrap()["eventSeq"]
        .as_u64()
        .unwrap();
    fixture.input(id).await;
    fixture
        .events_until(id, after, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let mut expected_seq = snapshot.event_seq + 1;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(15), unopened_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let event: Event = serde_json::from_str(&frame.data).unwrap();
        assert_eq!(event.seq, expected_seq);
        expected_seq += 1;
        if matches!(event.payload, EventPayload::TurnCompleted { .. }) {
            break;
        }
    }
    drop(unopened_stream);
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("work/client-env.txt")).unwrap(),
        "resumed-environment"
    );
    let response = fixture.request(Method::DELETE, &path, json!({})).await;
    assert_eq!(response.status(), 200);
    fixture.stop().await;
}

#[tokio::test]
async fn credentials_environment_and_request_bodies_are_not_logged() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let created = fixture.create().await;
    let invalid = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":fixture.root.path().join("work"), "env":{"SECRET":ENV_VALUE},
                "model":KEY, "approvalPolicy":{"private":KEY}, "clientName":"private-client"
            }),
        )
        .await;
    assert_eq!(invalid.status(), 400);
    let error = invalid.text().await.unwrap();
    assert!(!error.contains(KEY) && !error.contains(ENV_VALUE));
    fixture.input(&created.session.id).await;
    fixture
        .events_until(&created.session.id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    for path in [
        fixture.root.path().join("server.log"),
        fixture
            .root
            .path()
            .join("state/sessions")
            .join(&created.session.id)
            .join("events.jsonl"),
        fixture
            .root
            .path()
            .join("state/sessions")
            .join(&created.session.id)
            .join("model-history.jsonl"),
    ] {
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(KEY) && !text.contains(ENV_VALUE),
            "private configuration appeared in {path:?}"
        );
    }
    let log = fs::read_to_string(fixture.root.path().join("server.log")).unwrap();
    assert!(!log.contains("private-user-input") && !log.contains("private-client"));
}

async fn exit_signal_closes_active_work(signal: nix::sys::signal::Signal, replace: bool) {
    let mut fixture = Fixture::start(vec![tool_reply()]).await;
    let created = fixture.create().await;
    let id = &created.session.id;
    fixture.input(id).await;
    fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::RequestOpened { .. })
        })
        .await;
    let mut successor =
        replace.then(|| Fixture::spawn_args(fixture.root.path(), &["serve", "--replace"]));
    if replace {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = fixture
                    .request(
                        Method::PATCH,
                        &format!("/v1/sessions/{id}"),
                        json!({"name":"signal-test"}),
                    )
                    .await;
                if response.status() == 409 {
                    break;
                }
                assert_eq!(response.status(), 200);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("old daemon enters upgrade drain");
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(fixture.child.id().unwrap() as i32),
        signal,
    )
    .unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(5), fixture.child.wait()).await;
    if stopped.is_err() {
        fixture.child.kill().await.unwrap();
        fixture.child.wait().await.unwrap();
        if let Some(child) = successor.as_mut() {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
        }
        fixture.model.abort();
        let _ = (&mut fixture.model).await;
        panic!("{signal:?} must skip the upgrade wait and close active work");
    }
    assert!(
        stopped.unwrap().unwrap().success(),
        "{signal:?} must perform a graceful shutdown"
    );
    let events: Vec<Event> = fs::read_to_string(
        fixture
            .root
            .path()
            .join("state/sessions")
            .join(id)
            .join("events.jsonl"),
    )
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
    assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::TurnCompleted { turn } if turn.status == canter_protocol::TurnStatus::Interrupted)));
    assert!(matches!(
        events.last().unwrap().payload,
        EventPayload::SessionStatus {
            status: canter_protocol::SessionStatus::Closed
        }
    ));
    if let Some(child) = successor {
        fixture.child = child;
        fixture.ready().await;
        fixture.stop().await;
    } else {
        assert!(!fixture.root.path().join("state/server.json").exists());
        fixture.model.abort();
        let _ = (&mut fixture.model).await;
    }
}

#[tokio::test]
async fn sigterm_interrupts_upgrade_draining() {
    exit_signal_closes_active_work(nix::sys::signal::Signal::SIGTERM, true).await;
}

#[tokio::test]
async fn sigint_interrupts_upgrade_draining() {
    exit_signal_closes_active_work(nix::sys::signal::Signal::SIGINT, true).await;
}

#[tokio::test]
async fn sighup_closes_active_work_before_exiting() {
    exit_signal_closes_active_work(nix::sys::signal::Signal::SIGHUP, false).await;
}
