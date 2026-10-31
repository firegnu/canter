use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
};

const WAIT: Duration = Duration::from_secs(20);

fn reply(text: &str) -> Value {
    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
}
fn call(name: &str, arguments: Value) -> Value {
    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"exec-call","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]})
}

struct World {
    root: tempfile::TempDir,
    server: Child,
    model: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl World {
    async fn new(replies: Vec<Value>, sandbox: bool) -> Self {
        Self::configured(replies, sandbox, json!({})).await
    }
    async fn configured(replies: Vec<Value>, sandbox: bool, extra: Value) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-exec-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "state", "project", "extra"] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(request): Json<Value>| {
                let received = received.clone();
                let replies = replies.clone();
                async move {
                    received.lock().unwrap().push(request);
                    let response = replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected model request");
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {response}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = json!({"base_url":format!("http://{}/v1",listener.local_addr().unwrap()),"model":"fake","models":["fake"],"api_key":"test-only","context_window":1048576,"sandbox":sandbox});
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
        let model = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let server = Self::command(root.path())
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.path().join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let mut world = Self {
            root,
            server,
            model,
            requests,
        };
        tokio::time::timeout(WAIT, async {
            loop {
                if canter_server::discover(&world.root.path().join("state"))
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    break;
                }
                assert!(
                    world.server.try_wait().unwrap().is_none(),
                    "{}",
                    fs::read_to_string(world.root.path().join("server.log")).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        world
    }
    fn command(root: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_canter"));
        command
            .env_clear()
            .env("HOME", root.join("home"))
            .env("CANTER_HOME", root.join("state"))
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root.join("project"))
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
    async fn cli(&self, args: &[&str], stdin: &str) -> std::process::Output {
        let mut child = Self::command(self.root.path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(WAIT, child.wait_with_output())
            .await
            .expect("CLI must exit without interaction")
            .unwrap()
    }
    async fn client(&self) -> canter_tui::client::Client {
        let connection = canter_server::discover(&self.root.path().join("state"))
            .await
            .unwrap()
            .unwrap();
        canter_tui::client::Client::new(&connection.registration.socket_path).unwrap()
    }
    async fn stop(mut self) {
        self.server.kill().await.unwrap();
        self.server.wait().await.unwrap();
        self.model.abort();
        let _ = (&mut self.model).await;
    }
}
impl Drop for World {
    fn drop(&mut self) {
        self.model.abort();
    }
}

#[tokio::test]
async fn exec_reads_stdin_and_appends_piped_context() {
    for (args, expected) in [
        (vec!["exec"], "Task from stdin"),
        (vec!["exec", "-"], "Task from stdin"),
        (
            vec!["exec", "Task argument"],
            "Task argument\n\n<stdin>\nTask from stdin\n</stdin>",
        ),
    ] {
        let world = World::new(vec![reply("Read task.")], true).await;
        let output = world.cli(&args, "Task from stdin").await;
        assert!(output.status.success(), "{output:?}");
        {
            let requests = world.requests.lock().unwrap();
            let messages = requests[0]["messages"].as_array().unwrap();
            assert_eq!(messages.last().unwrap()["content"], expected);
        }
        world.stop().await;
    }
}

#[tokio::test]
async fn exec_runs_one_turn_prints_final_answer_and_keeps_the_session() {
    let mut world = World::new(
        vec![
            call(
                "write",
                json!({"path":"result.txt","content":"workspace write"}),
            ),
            call(
                "write",
                json!({"path":"../extra/result.txt","content":"additional directory write"}),
            ),
            reply("Finished the task."),
        ],
        true,
    )
    .await;
    // No server now: exec must use the normal automatic startup path.
    world.server.kill().await.unwrap();
    world.server.wait().await.unwrap();
    let output = world
        .cli(
            &[
                "exec",
                "--model",
                "fake",
                "--tag",
                "task=example",
                "--add-dir",
                "../extra",
                "Do the task",
            ],
            "",
        )
        .await;
    let registration = canter_server::discover(&world.root.path().join("state"))
        .await
        .unwrap()
        .unwrap()
        .registration;
    struct Daemon(nix::unistd::Pid);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = nix::sys::signal::kill(self.0, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let daemon = Daemon(nix::unistd::Pid::from_raw(registration.pid as i32));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Finished the task.\n"
    );
    let client = world.client().await;
    let sessions = client.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    let snapshot = client.read_session(&sessions[0].id).await.unwrap().session;
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.settings.tags["task"], "example");
    assert_eq!(snapshot.settings.additional_directories.len(), 1);
    assert_eq!(
        snapshot.settings.approval_policy,
        canter_protocol::PermissionMode::AskForExecution
    );
    let stats = world.cli(&["stats", &snapshot.id, "--json"], "").await;
    assert!(stats.status.success());
    assert_eq!(world.requests.lock().unwrap().len(), 3);
    assert_eq!(
        fs::read_to_string(world.root.path().join("project/result.txt")).unwrap(),
        "workspace write"
    );
    assert_eq!(
        fs::read_to_string(world.root.path().join("extra/result.txt")).unwrap(),
        "additional directory write"
    );
    nix::sys::signal::kill(daemon.0, nix::sys::signal::Signal::SIGTERM).unwrap();
    tokio::time::timeout(WAIT, async {
        while nix::sys::signal::kill(daemon.0, None) != Err(nix::errno::Errno::ESRCH) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::mem::forget(daemon);
}

#[tokio::test]
async fn exec_declines_approval_on_the_server_without_writing_outside_workspace() {
    for tool in [
        call(
            "write",
            json!({"path":"../outside.txt","content":"must not be written"}),
        ),
        call(
            "shell",
            json!({"command":"printf must-not-run > sandbox-request.txt","sandboxRequest":{"network":true},"justification":"Test the ordinary network approval path"}),
        ),
        call("read", json!({"path":"prompt.txt"})),
        call(
            "web_fetch",
            json!({"url":"http://127.0.0.1:9/ordinary-page"}),
        ),
    ] {
        let world = World::new(vec![tool, reply("Could not perform the action.")], true).await;
        fs::write(
            world.root.path().join("project/prompt.txt"),
            "synthetic protected content",
        )
        .unwrap();
        fs::write(
            world.root.path().join("state/rules.json"),
            json!({"fileRules":[{"pattern":"prompt.txt","decision":"prompt"}]}).to_string(),
        )
        .unwrap();
        let output = world.cli(&["exec", "Change a file"], "").await;
        assert_eq!(output.status.code(), Some(3), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("approval"));
        assert!(!world.root.path().join("outside.txt").exists());
        assert!(
            !world
                .root
                .path()
                .join("project/sandbox-request.txt")
                .exists()
        );
        {
            let requests = world.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(
                requests[1]["messages"]
                    .to_string()
                    .contains("No one is available to approve")
            );
        }
        world.stop().await;
    }
}

#[tokio::test]
async fn exec_answers_questions_without_a_client_reply() {
    let world = World::new(vec![call("ask_user", json!({"question":"Which way?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]})), reply("Chose A.")], true).await;
    let output = world.cli(&["exec", "Make a choice"], "").await;
    assert!(output.status.success(), "{output:?}");
    assert!(
        world.requests.lock().unwrap()[1]["messages"]
            .to_string()
            .contains("No one is available. Use your own judgment and continue.")
    );
    world.stop().await;
}

#[tokio::test]
async fn exec_json_outputs_events_and_a_final_usage_cost_and_session_record() {
    let world = World::new(vec![reply("JSON result.")], true).await;
    let output = world.cli(&["exec", "--json", "Report results"], "").await;
    assert!(output.status.success(), "{output:?}");
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "exec.started");
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "item.completed"
                && event["item"]["text"] == "JSON result.")
    );
    let result = events.last().unwrap();
    assert_eq!(result["type"], "exec.completed");
    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["usage"]["inputTokens"], 10);
    assert_eq!(result["usage"]["outputTokens"], 5);
    assert_eq!(result["cost"]["unknown"], true);
    assert_eq!(result["sessionId"], events[0]["sessionId"]);
    assert!(result["turnId"].as_str().is_some());
    let invalid = world.cli(&["exec", "--json", "--invalid"], "").await;
    assert_eq!(invalid.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(result["type"], "exec.completed");
    assert_eq!(result["outcome"], "error");
    assert!(result["sessionId"].is_null());
    world.stop().await;
}

#[tokio::test]
async fn exec_resumes_by_id_and_last_only_in_the_current_directory() {
    let world = World::new(
        vec![reply("First."), reply("Second."), reply("Third.")],
        true,
    )
    .await;
    assert!(
        world
            .cli(&["exec", "First task"], "")
            .await
            .status
            .success()
    );
    let client = world.client().await;
    let id = client.list_sessions().await.unwrap()[0].id.clone();
    let output = world
        .cli(&["exec", "--resume", &id, "Second task"], "")
        .await;
    assert!(output.status.success(), "{output:?}");
    let params = serde_json::from_value(json!({"cwd":world.root.path().join("extra"),"env":{},"model":"fake","approvalPolicy":"alwaysAsk","clientName":"test"})).unwrap();
    client.create_session(&params).await.unwrap();
    let output = world.cli(&["exec", "--last", "Third task"], "").await;
    assert!(output.status.success(), "{output:?}");
    let snapshot = client.read_session(&id).await.unwrap().session;
    assert_eq!(snapshot.turns.len(), 3);
    assert_eq!(
        snapshot.turns[2].input,
        vec![canter_protocol::UserInput::Text {
            text: "Third task".into()
        }]
    );
    {
        let requests = world.requests.lock().unwrap();
        assert!(requests[2]["messages"].to_string().contains("First task"));
        assert!(requests[2]["messages"].to_string().contains("Second task"));
    }
    world.stop().await;
}

#[tokio::test]
async fn exec_rejects_old_servers_before_any_session_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::Builder::new()
        .prefix("canter-exec-old-")
        .tempdir_in("/tmp")
        .unwrap();
    for dir in ["home", "state", "project"] {
        fs::create_dir(root.path().join(dir)).unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let router = Router::new().route("/v1/info", axum::routing::get(|| async {
        Json(json!({"serverVersion":canter_server::VERSION,"protocolVersion":1,"sessions":[]}))
    })).fallback(move || { observed.fetch_add(1,Ordering::SeqCst); async { axum::http::StatusCode::NOT_FOUND } });
    let socket = root.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let lock = fs::File::create(root.path().join("state/server.lock")).unwrap();
    lock.lock().unwrap();
    fs::write(
        root.path().join("state/server.json"),
        serde_json::to_vec(&canter_server::Registration {
            pid: std::process::id(),
            version: canter_server::VERSION.into(),
            protocol_version: 1,
            socket_path: socket,
            instance: "old-test".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    for args in [
        vec!["exec", "--json", "Task"],
        vec!["exec", "--resume", "s1", "Task"],
        vec!["exec", "--last", "Task"],
    ] {
        let output = World::command(root.path())
            .args(args)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("canter serve --replace"),
            "{output:?}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn exec_respects_disabled_sandbox_and_yolo_while_preserving_forbidden_rules() {
    let shell = call("shell", json!({"command":"printf allowed > result.txt"}));
    let world = World::new(
        vec![
            shell.clone(),
            reply("Declined."),
            shell,
            reply("Written."),
            call("write", json!({"path":"blocked.txt","content":"forbidden"})),
            reply("Blocked."),
        ],
        false,
    )
    .await;
    let output = world.cli(&["exec", "Run a command"], "").await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("Sandbox is off").count(), 1, "{stderr}");
    assert!(stderr.contains("--yolo"));
    assert!(!world.root.path().join("project/result.txt").exists());
    let output = world.cli(&["exec", "--yolo", "Run a command"], "").await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(world.root.path().join("project/result.txt")).unwrap(),
        "allowed"
    );
    fs::write(
        world.root.path().join("state/rules.json"),
        json!({"fileRules":[{"pattern":"blocked.txt","decision":"forbidden"}]}).to_string(),
    )
    .unwrap();
    let output = world
        .cli(&["exec", "--yolo", "Write a forbidden file"], "")
        .await;
    assert!(output.status.success(), "{output:?}");
    assert!(!world.root.path().join("project/blocked.txt").exists());
    world.stop().await;
}

#[tokio::test]
async fn exec_declines_plan_approval_and_keeps_plan_mode() {
    let world = World::new(
        vec![
            call("submit_plan", json!({"plan":"Implement the change."})),
            reply("Waiting for a person to approve."),
        ],
        true,
    )
    .await;
    let client = world.client().await;
    let params = serde_json::from_value(json!({"cwd":world.root.path().join("project"),"env":{},"model":"fake","approvalPolicy":"alwaysAsk","clientName":"test"})).unwrap();
    let id = client.create_session(&params).await.unwrap().session.id;
    client
        .update(
            &id,
            &canter_protocol::SessionUpdateParams {
                plan_mode: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let output = world
        .cli(&["exec", "--resume", &id, "Propose the plan"], "")
        .await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        client
            .read_session(&id)
            .await
            .unwrap()
            .session
            .settings
            .plan_mode
    );
    assert!(
        world.requests.lock().unwrap()[1]["messages"]
            .to_string()
            .contains("No one is available to approve")
    );
    world.stop().await;
}

#[tokio::test]
async fn exec_reports_final_model_failure_with_a_distinct_exit_code() {
    let failed = json!({"choices":[{"index":0,"delta":{},"finish_reason":"network_error"}]});
    let world = World::configured(vec![failed], true, json!({"model_max_retries":0})).await;
    let output = world.cli(&["exec", "--json", "Task"], "").await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let last: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["outcome"], "failed");
    assert!(
        last["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    assert!(!output.stderr.is_empty());
    world.stop().await;
}

#[tokio::test]
async fn exec_reports_goal_budget_limit_after_the_same_turn_summary() {
    let world = World::configured(
        vec![
            reply("Initial step."),
            reply("Reached limit."),
            reply("Budget summary."),
        ],
        true,
        json!({"prices":{"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}}),
    )
    .await;
    let client = world.client().await;
    let params = serde_json::from_value(json!({"nonInteractive":true,"cwd":world.root.path().join("project"),"env":{},"model":"fake","approvalPolicy":"askForExecution","clientName":"test"})).unwrap();
    let id = client.create_session(&params).await.unwrap().session.id;
    client
        .goal(
            &id,
            &canter_tui::GoalCommand::Set(
                serde_json::from_value(json!({"objective":"Finish task","budgetUsd":10})).unwrap(),
            ),
        )
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if client.read_session(&id).await.unwrap().session.status
                == canter_protocol::SessionStatus::Idle
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output = world
        .cli(&["exec", "--resume", &id, "--json", "Continue task"], "")
        .await;
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let last: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["outcome"], "budgetLimited");
    assert_eq!(last["cost"]["usd"], 10.0);
    let snapshot = client.read_session(&id).await.unwrap().session;
    assert_eq!(snapshot.turns.len(), 2);
    assert_eq!(
        snapshot.goal.unwrap().status,
        canter_protocol::GoalStatus::BudgetLimited
    );
    assert_eq!(world.requests.lock().unwrap().len(), 3);
    world.stop().await;
}

#[tokio::test]
async fn exec_ctrl_c_waits_for_the_turn_and_command_to_stop() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let world = World::new(
        vec![call(
            "shell",
            json!({"command":"echo $$ > command.pid; exec sleep 30"}),
        )],
        true,
    )
    .await;
    let child = World::command(world.root.path())
        .args(["exec", "--json", "Run a long command"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if world.root.path().join("project/command.pid").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let command_pid: i32 = fs::read_to_string(world.root.path().join("project/command.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    kill(Pid::from_raw(child.id().unwrap() as i32), Signal::SIGINT).unwrap();
    let output = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    let client = world.client().await;
    let id = client.list_sessions().await.unwrap()[0].id.clone();
    let snapshot = client.read_session(&id).await.unwrap().session;
    assert_eq!(snapshot.status, canter_protocol::SessionStatus::Idle);
    assert_eq!(
        snapshot.turns[0].status,
        canter_protocol::TurnStatus::Interrupted
    );
    assert_eq!(
        kill(Pid::from_raw(command_pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    let result: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["outcome"], "interrupted");
    world.stop().await;
}

#[tokio::test]
async fn exec_reports_tools_and_retries_on_stderr_and_only_the_final_answer_on_stdout() {
    let failed = json!({"choices":[{"index":0,"delta":{"content":"Discarded partial answer."},"finish_reason":"network_error"}]});
    let world = World::configured(
        vec![
            failed,
            call("shell", json!({"command":"printf tool-output; exit 7"})),
            reply("Final answer."),
        ],
        true,
        json!({"model_max_retries":1}),
    )
    .await;
    let output = world.cli(&["exec", "Retry and run"], "").await;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Final answer.\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Retry"), "{stderr}");
    assert!(stderr.contains("shell: printf tool-output"), "{stderr}");
    assert!(stderr.contains("Tool failed"), "{stderr}");
    world.stop().await;
}
