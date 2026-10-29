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

#[path = "support/mcp.rs"]
mod mcp;
#[path = "support/plan.rs"]
mod plan;

const KEY: &str = "test-only-canter-key";
const ENV_VALUE: &str = "test-only-client-environment";

struct Fixture {
    root: TempDir,
    child: Child,
    client: Client,
    model: JoinHandle<()>,
    script: Arc<Mutex<ModelScript>>,
}

struct ModelScript {
    replies: VecDeque<String>,
    requests: Vec<Value>,
}

impl Fixture {
    async fn start(replies: Vec<String>) -> Self {
        Self::start_with_models(replies, "fake-model", None).await
    }

    async fn start_with_models(
        replies: Vec<String>,
        default_model: &str,
        models: Option<&[&str]>,
    ) -> Self {
        Self::start_with_prices(replies, default_model, models, json!({})).await
    }

    async fn start_with_prices(
        replies: Vec<String>,
        default_model: &str,
        models: Option<&[&str]>,
        prices: Value,
    ) -> Self {
        Self::start_with_mcp(replies, default_model, models, prices, json!({})).await
    }

    async fn start_with_mcp(
        replies: Vec<String>,
        default_model: &str,
        models: Option<&[&str]>,
        prices: Value,
        mcp_servers: Value,
    ) -> Self {
        Self::start_with_mcp_env(replies, default_model, models, prices, mcp_servers, &[]).await
    }

    async fn start_with_mcp_env(
        replies: Vec<String>,
        default_model: &str,
        models: Option<&[&str]>,
        prices: Value,
        mcp_servers: Value,
        environment: &[(&str, &str)],
    ) -> Self {
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
        let script = Arc::new(Mutex::new(ModelScript {
            replies: replies.into(),
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/chat/completions", post(fake_model))
            .with_state(script.clone());
        let model = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = json!({
            "base_url":base_url, "model":default_model, "context_window":1048576, "api_key":KEY
        });
        if let Some(models) = models {
            config["models"] = json!(models);
        }
        config["prices"] = prices;
        config["mcp_servers"] = mcp_servers;
        fs::write(
            home.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        fs::set_permissions(home.join("config.json"), fs::Permissions::from_mode(0o600)).unwrap();
        let child = Self::spawn_args_with_env(root.path(), &["serve"], environment);
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
            script,
        };
        fixture.ready().await;
        fixture
    }

    fn spawn(root: &std::path::Path) -> Child {
        Self::spawn_args(root, &["serve"])
    }

    fn spawn_args(root: &std::path::Path, args: &[&str]) -> Child {
        Self::spawn_args_with_env(root, args, &[])
    }

    fn spawn_args_with_env(
        root: &std::path::Path,
        args: &[&str],
        environment: &[(&str, &str)],
    ) -> Child {
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
            .env("TZ", "Asia/Singapore")
            .envs(environment.iter().copied())
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

#[tokio::test]
async fn configured_prices_override_builtin_prices_for_model_requests() {
    let reply = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000000,\"prompt_cache_hit_tokens\":800000,\"completion_tokens\":100000,\"total_tokens\":1100000}}\n\n",
        "data: [DONE]\n\n"
    );
    let mut fixture = Fixture::start_with_prices(
        vec![reply.into()],
        "deepseek-flash",
        None,
        json!({"deepseek-flash":{"peak":{"cache_hit":1.0,"cache_miss":2.0,"output":3.0}}}),
    )
    .await;
    let response = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":fixture.root.path().join("work"), "env":{}, "model":"deepseek-flash",
                "approvalPolicy":"alwaysAsk", "clientName":"test"
            }),
        )
        .await;
    let created: SessionCreateResponse = response.json().await.unwrap();
    fixture.input(&created.session.id).await;
    let events = fixture
        .events_until(&created.session.id, created.event_seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let usage = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionUsage { token_usage } => Some(token_usage),
            _ => None,
        })
        .unwrap();
    assert!((usage.request.as_ref().unwrap().cost_usd.unwrap() - 1.5).abs() < 1e-10);
    let report: Value = fixture
        .request(
            Method::GET,
            &format!("/v1/sessions/{}/usage", created.session.id),
            json!({}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(report["lastTurnCost"], json!({"usd":1.5,"unknown":false}));
    assert_eq!(report["total"]["cost"], report["lastTurnCost"]);
    fixture.stop().await;
}

#[tokio::test]
async fn usage_replays_legacy_model_changes_and_preserves_recorded_costs() {
    let mut fixture = Fixture::start_with_prices(
        vec![],
        "fake-model",
        None,
        json!({"priced":{"peak":{"cache_hit":1.0,"cache_miss":2.0,"output":3.0}}}),
    )
    .await;
    let directory = fixture.root.path().join("state/sessions/legacy");
    fs::create_dir_all(&directory).unwrap();
    let counts = json!({"inputTokens":1000000,"cachedInputTokens":800000,
        "outputTokens":100000,"reasoningOutputTokens":50000,"totalTokens":1100000});
    let usage = json!({"type":"session.usage","tokenUsage":{
        "last":counts,"total":counts,"contextTokens":0,"modelContextWindow":1048576}});
    let settings = |model| {
        json!({"type":"session.updated","settings":{
        "model":model,"cwd":directory,"name":null,"approvalPolicy":"alwaysAsk"}})
    };
    let mut stored = usage.clone();
    stored["tokenUsage"]["request"] = json!({"model":"priced","startedAt":0,"costUsd":0.25});
    let mut unknown = stored.clone();
    unknown["tokenUsage"]["request"]["costUsd"] = Value::Null;
    // Singapore was UTC+07:30 in 1970; cross local midnight within one session.
    let entries = [
        (349_200_000, settings("deepseek-flash")), // Mon 01:00 UTC
        (349_200_000, usage.clone()),              // peak: .1848
        (402_600_000, usage.clone()),              // Mon 15:50 UTC, off-peak: .0924
        (405_600_000, settings("priced")),         // Mon 16:40 UTC = Tue local
        (405_600_000, usage.clone()),              // flat override: 1.5
        (405_600_000, stored),                     // original .25, never repriced
        (405_600_000, unknown),                    // original unknown, never repriced
        (405_600_000, settings("unpriced")),
        (405_600_000, usage),
    ];
    let text = entries
        .into_iter()
        .enumerate()
        .map(|(index, (at, mut event))| {
            event["seq"] = json!(index + 1);
            event["sessionId"] = json!("legacy");
            event["timestamp"] = json!(at);
            serde_json::to_string(&event).unwrap() + "\n"
        })
        .collect::<String>();
    fs::write(directory.join("events.jsonl"), &text).unwrap();
    let response = fixture.request(Method::GET, "/v1/usage", json!({})).await;
    assert_eq!(
        response.status(),
        200,
        "usage must be available without resuming sessions"
    );
    let report: Value = response.json().await.unwrap();
    assert_eq!(report["data"].as_array().unwrap().len(), 3);
    assert_eq!(report["data"][0]["date"], "1970-01-05");
    assert_eq!(report["data"][0]["model"], "deepseek-flash");
    assert!((report["data"][0]["cost"]["usd"].as_f64().unwrap() - 0.2772).abs() < 1e-10);
    assert_eq!(report["data"][1]["date"], "1970-01-06");
    assert_eq!(
        report["data"][1]["cost"],
        json!({"usd":1.75,"unknown":true})
    );
    assert_eq!(report["data"][2]["cost"], json!({"usd":0.0,"unknown":true}));
    assert_eq!(report["total"]["cachedInputTokens"], 4_800_000);
    assert_eq!(report["total"]["uncachedInputTokens"], 1_200_000);
    assert_eq!(report["total"]["outputTokens"], 600_000);
    assert!((report["total"]["cost"]["usd"].as_f64().unwrap() - 2.0272).abs() < 1e-10);
    assert_eq!(report["total"]["cost"]["unknown"], true);
    let response = fixture
        .request(Method::GET, "/v1/sessions/legacy/usage", json!({}))
        .await;
    assert_eq!(response.status(), 200);
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["total"], report["total"]);
    let snapshot: Value = fixture
        .request(Method::GET, "/v1/sessions/legacy", json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        snapshot["session"]["tokenUsage"]["cost"],
        report["total"]["cost"]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_canter"))
        .arg("usage")
        .env_clear()
        .env("HOME", fixture.root.path().join("home"))
        .env("CANTER_HOME", fixture.root.path().join("state"))
        .current_dir(fixture.root.path())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let rows = stdout
        .lines()
        .skip(1)
        .map(|row| row.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![
                "1970-01-05",
                "deepseek-flash",
                "1600000",
                "400000",
                "200000",
                "$0.28"
            ],
            vec![
                "1970-01-06",
                "priced",
                "2400000",
                "600000",
                "300000",
                "$1.75+?"
            ],
            vec![
                "1970-01-06",
                "unpriced",
                "800000",
                "200000",
                "100000",
                "unknown"
            ],
            vec!["Total", "4800000", "1200000", "600000", "$2.03+?"],
        ]
    );
    assert_eq!(
        fs::read_to_string(directory.join("events.jsonl")).unwrap(),
        text,
        "no backfill writes"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn models_configuration_lists_defaults_and_prepends_the_default() {
    for (default_model, configured, expected) in [
        (
            "deepseek-flash",
            None,
            vec!["deepseek-flash", "deepseek-v4-pro"],
        ),
        (
            "fake-model",
            None,
            vec!["fake-model", "deepseek-flash", "deepseek-v4-pro"],
        ),
        (
            "fake-model",
            Some(vec!["custom-model"]),
            vec!["fake-model", "custom-model"],
        ),
        (
            "fake-model",
            Some(vec!["custom-model", "fake-model"]),
            vec!["custom-model", "fake-model"],
        ),
    ] {
        let mut fixture =
            Fixture::start_with_models(Vec::new(), default_model, configured.as_deref()).await;
        let response = fixture.request(Method::GET, "/v1/models", json!({})).await;
        assert_eq!(response.status(), 200);
        let listed: Value = response.json().await.unwrap();
        assert_eq!(listed["nextCursor"], Value::Null);
        let data = listed["data"].as_array().unwrap();
        assert_eq!(data.len(), expected.len());
        for (model, expected) in data.iter().zip(expected) {
            assert_eq!(model["id"], expected);
            assert_eq!(model["model"], expected);
            assert_eq!(model["displayName"], expected);
            assert_eq!(model["isDefault"], expected == default_model);
        }
        fixture.stop().await;
    }
}

#[tokio::test]
async fn unlisted_models_are_rejected_without_changing_settings() {
    let mut fixture = Fixture::start(Vec::new()).await;
    let response = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":fixture.root.path().join("work"), "env":{},
                "model":"unlisted", "approvalPolicy":"alwaysAsk", "clientName":"test"
            }),
        )
        .await;
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "invalid_request"
    );
    let sessions: Value = fixture
        .request(Method::GET, "/v1/sessions", json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert!(sessions["data"].as_array().unwrap().is_empty());

    let created = fixture.create().await;
    let path = format!("/v1/sessions/{}", created.session.id);
    let before: Value = fixture
        .request(Method::GET, &path, json!({}))
        .await
        .json()
        .await
        .unwrap();
    let response = fixture
        .request(
            Method::PATCH,
            &path,
            json!({
                "model":"unlisted", "reasoningEffort":"max", "name":"must not change"
            }),
        )
        .await;
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "invalid_request"
    );
    let after: Value = fixture
        .request(Method::GET, &path, json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(after, before);
    fixture.stop().await;
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.model.abort();
    }
}

async fn fake_model(
    State(script): State<Arc<Mutex<ModelScript>>>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let mut script = script.lock().unwrap();
    script.requests.push(body);
    let reply = script
        .replies
        .pop_front()
        .expect("unexpected model request");
    ([("content-type", "text/event-stream")], reply)
}

#[tokio::test]
async fn skills_prompt_lists_tdd_and_read_gets_the_full_skill_without_approval() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let home = fixture.root.path().join("home");
    let skill = home.join(".agents/skills/tdd/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    let contents = "---\nname: tdd\ndescription: Test-first development\n---\n\nWrite a failing test before implementation.\n";
    fs::write(&skill, contents).unwrap();
    fixture.script.lock().unwrap().replies.push_front(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{
            "index":0,"id":"read-skill","type":"function","function":{
                "name":"read","arguments":json!({"path":skill}).to_string()
            }
        }]},"finish_reason":"tool_calls"}]})
    ));
    let response = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":fixture.root.path().join("work"), "env":{"HOME":home},
                "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"skills-test"
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let created: SessionCreateResponse = response.json().await.unwrap();
    let id = &created.session.id;
    let response = fixture
        .request(
            Method::POST,
            &format!("/v1/sessions/{id}/input"),
            json!({
                "input":[{"type":"text","text":"用 tdd 的方式改 X"}], "clientName":"skills-test"
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let events = fixture
        .events_until(id, 0, |event| {
            matches!(
                event.payload,
                EventPayload::TurnCompleted { .. } | EventPayload::RequestOpened { .. }
            )
        })
        .await;
    fixture.stop().await;
    let script = fixture.script.lock().unwrap();
    let system = script.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(
        system.contains("tdd"),
        "the system prompt must list the tdd skill"
    );
    assert!(system.contains("Test-first development"));
    assert!(system.contains(skill.to_str().unwrap()));
    assert!(!system.contains("Write a failing test before implementation."));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
    );
    assert_eq!(script.requests.len(), 2);
    let messages = script.requests[1]["messages"].as_array().unwrap();
    let read = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(
        read["content"],
        "1: ---\n2: name: tdd\n3: description: Test-first development\n4: ---\n5: \n6: Write a failing test before implementation.\n"
    );
}

fn write_skill(base: &std::path::Path, directory: &str, contents: &str) -> std::path::PathBuf {
    let path = base.join(".agents/skills").join(directory).join("SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

impl Fixture {
    async fn create_with_skills(&self, cwd: &std::path::Path) -> SessionCreateResponse {
        let response = self
            .request(
                Method::POST,
                "/v1/sessions",
                json!({
                    "cwd":cwd, "env":{"HOME":self.root.path().join("home")},
                    "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"skills-test"
                }),
            )
            .await;
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }

    async fn skills(&self, id: &str) -> Value {
        let response = self
            .request(Method::GET, &format!("/v1/sessions/{id}/skills"), json!({}))
            .await;
        assert_eq!(
            response.status(),
            200,
            "the open session must expose its frozen skills"
        );
        response.json().await.unwrap()
    }
}

#[tokio::test]
async fn skills_prefer_project_and_list_manual_skills_but_skip_invalid_headers_with_warnings() {
    let mut fixture = Fixture::start(vec![text_reply(), text_reply()]).await;
    let home = fixture.root.path().join("home");
    let work = fixture.root.path().join("work");
    fs::create_dir(work.join(".git")).unwrap();
    let cwd = work.join("nested");
    fs::create_dir(&cwd).unwrap();
    write_skill(
        &home,
        "tdd",
        "---\nname: tdd\ndescription: global tdd\n---\n",
    );
    let tdd = write_skill(
        &work,
        "tdd",
        "---\nname: tdd\ndescription: \"项目：测试优先\"\nmetadata:\n  description: ignored\n---\nproject body\n",
    );
    let manual = write_skill(
        &home,
        "manual",
        "---\nname: manual\ndescription: Manual review\ndisable-model-invocation: true\n---\n",
    );
    let global = write_skill(
        &home,
        "zeta",
        "---\nname: zeta\ndescription: Global helper\n---\n",
    );
    let broken = [
        write_skill(&work, "missing-header", "not front matter\n"),
        write_skill(&work, "missing-name", "---\ndescription: no name\n---\n"),
        write_skill(
            &work,
            "missing-description",
            "---\nname: no-description\n---\n",
        ),
    ];
    let created = fixture.create_with_skills(&cwd).await;
    let id = &created.session.id;
    assert_eq!(
        fixture.skills(id).await,
        json!({"data":[
            {"name":"manual", "description":"Manual review", "path":manual, "source":"global", "modelInvocation":false},
            {"name":"tdd", "description":"项目：测试优先", "path":tdd, "source":"project", "modelInvocation":true},
            {"name":"zeta", "description":"Global helper", "path":global, "source":"global", "modelInvocation":true}
        ]})
    );
    fixture.input(id).await;
    let events = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let warnings: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::Warning { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 3);
    for path in broken {
        assert!(
            warnings
                .iter()
                .any(|message| message.contains(path.to_str().unwrap()))
        );
    }
    fixture.input(id).await;
    let later = fixture
        .events_until(id, events.last().unwrap().seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert!(
        !later
            .iter()
            .any(|event| matches!(event.payload, EventPayload::Warning { .. }))
    );
    fixture.stop().await;
    let script = fixture.script.lock().unwrap();
    let system = script.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(system.contains("项目：测试优先"));
    assert!(system.contains(tdd.to_str().unwrap()));
    assert!(!system.contains("global tdd"));
    assert!(!system.contains("manual"));
    assert!(!system.contains("project body"));
    assert!(system.find("- tdd:").unwrap() < system.find("- zeta:").unwrap());
}

#[tokio::test]
async fn skills_empty_prefix_and_list_stay_frozen_until_session_reopens() {
    let mut fixture = Fixture::start(vec![text_reply(), text_reply(), text_reply()]).await;
    let home = fixture.root.path().join("home");
    let work = fixture.root.path().join("work");
    let created = fixture.create_with_skills(&work).await;
    let id = &created.session.id;
    assert_eq!(fixture.skills(id).await, json!({"data":[]}));
    fixture.input(id).await;
    let first = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert!(
        !first
            .iter()
            .any(|event| matches!(event.payload, EventPayload::Warning { .. }))
    );
    let skill = write_skill(
        &home,
        "tdd",
        "---\nname: tdd\ndescription: Newly installed skill\n---\n",
    );
    assert_eq!(fixture.skills(id).await, json!({"data":[]}));
    fixture.input(id).await;
    fixture
        .events_until(id, first.last().unwrap().seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    let response = fixture
        .request(Method::DELETE, &format!("/v1/sessions/{id}"), json!({}))
        .await;
    assert_eq!(response.status(), 200);
    let response = fixture
        .request(
            Method::POST,
            &format!("/v1/sessions/{id}/resume"),
            json!({"env":{"HOME":home}}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let reopened: SessionReadResponse = response.json().await.unwrap();
    assert_eq!(fixture.skills(id).await["data"][0]["path"], json!(skill));
    fixture.input(id).await;
    fixture
        .events_until(id, reopened.event_seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    let script = fixture.script.lock().unwrap();
    let before = script.requests[0]["messages"].as_array().unwrap();
    let during = script.requests[1]["messages"].as_array().unwrap();
    assert!(!before[0]["content"].as_str().unwrap().contains("<skills>"));
    assert_eq!(before.as_slice(), &during[..before.len()]);
    assert!(
        script.requests[2]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Newly installed skill")
    );
}

#[tokio::test]
async fn global_agents_instructions_precede_project_instructions() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let home = fixture.root.path().join("client-home");
    let global = home.join(".canter/AGENTS.md");
    let work = fixture.root.path().join("work");
    let project = work.join("AGENTS.md");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(&global, "Global instruction marker").unwrap();
    fs::write(&project, "Project instruction marker").unwrap();
    let response = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":work, "env":{"HOME":home},
                "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"agents-test"
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let created: SessionCreateResponse = response.json().await.unwrap();
    let id = &created.session.id;
    fixture.input(id).await;
    fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    let script = fixture.script.lock().unwrap();
    let system = script.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    let global_position = system
        .find("Global instruction marker")
        .expect("the system prompt must include global instructions from the session HOME");
    let project_position = system.find("Project instruction marker").unwrap();
    assert!(global_position < project_position);
    assert!(system.contains(&format!(
        "<global_instructions path=\"{}\">",
        global.display()
    )));
    assert!(system.contains(&format!(
        "<project_instructions path=\"{}\">",
        project.display()
    )));
}

#[tokio::test]
async fn global_agents_missing_file_adds_no_prompt_section_or_warning() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let created = fixture
        .create_with_skills(&fixture.root.path().join("work"))
        .await;
    let id = &created.session.id;
    fixture.input(id).await;
    let events = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::Warning { .. }))
    );
    let script = fixture.script.lock().unwrap();
    let system = script.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(!system.contains("<global_instructions "));
}

#[tokio::test]
async fn global_agents_oversized_file_is_skipped_with_a_warning() {
    let mut fixture = Fixture::start(vec![text_reply()]).await;
    let global = fixture.root.path().join("home/.canter/AGENTS.md");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(&global, "x".repeat(64 * 1024 + 1)).unwrap();
    let created = fixture
        .create_with_skills(&fixture.root.path().join("work"))
        .await;
    let id = &created.session.id;
    fixture.input(id).await;
    let events = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    let warnings: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::Warning { message } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        warnings,
        [format!(
            "Skipped global instructions {}: exceeds the 64 KiB file limit",
            global.display()
        )]
    );
    let script = fixture.script.lock().unwrap();
    let system = script.requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(!system.contains("<global_instructions "));
    assert!(!system.contains(&"x".repeat(64 * 1024 + 1)));
}

#[tokio::test]
async fn global_agents_edits_do_not_change_the_open_session_prefix() {
    let mut fixture = Fixture::start(vec![text_reply(), text_reply()]).await;
    let global = fixture.root.path().join("home/.canter/AGENTS.md");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::write(&global, "Original global instructions").unwrap();
    let created = fixture
        .create_with_skills(&fixture.root.path().join("work"))
        .await;
    let id = &created.session.id;
    fixture.input(id).await;
    let first = fixture
        .events_until(id, 0, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fs::write(&global, "Changed global instructions").unwrap();
    fixture.input(id).await;
    fixture
        .events_until(id, first.last().unwrap().seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    fixture.stop().await;
    let script = fixture.script.lock().unwrap();
    let before = script.requests[0]["messages"].as_array().unwrap();
    let during = script.requests[1]["messages"].as_array().unwrap();
    assert!(
        before[0]["content"]
            .as_str()
            .unwrap()
            .contains("Original global instructions")
    );
    assert_eq!(before.as_slice(), &during[..before.len()]);
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
        logs.contains("a server is already running"),
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
async fn a_ready_line_is_printed_once_clients_can_connect_and_again_after_replace() {
    let mut fixture = Fixture::start(vec![]).await;
    let ready = |pid: u32| format!("canter server ready · {} · pid {pid}", canter::VERSION);
    let log = fs::read_to_string(fixture.root.path().join("server.log")).unwrap();
    let first = ready(fixture.child.id().unwrap());
    assert_eq!(
        log.lines().filter(|line| *line == first).count(),
        1,
        "{log}"
    );
    let replacement =
        Fixture::spawn_args(fixture.root.path(), &["serve", "--replace", "--foreground"]);
    let mut previous = std::mem::replace(&mut fixture.child, replacement);
    assert!(
        tokio::time::timeout(Duration::from_secs(15), previous.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    fixture.ready().await;
    let log = fs::read_to_string(fixture.root.path().join("server.log")).unwrap();
    let second = ready(fixture.child.id().unwrap());
    assert_eq!(
        log.lines().filter(|line| *line == second).count(),
        1,
        "{log}"
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
    let mut successor = replace
        .then(|| Fixture::spawn_args(fixture.root.path(), &["serve", "--replace", "--foreground"]));
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
            status: canter_protocol::SessionStatus::Closed,
            close_reason: Some(canter_protocol::SessionCloseReason::ServerShutdown),
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
