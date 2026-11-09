use super::*;

const ORDINARY: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "Interrupt",
];

async fn fixture(events: &[&str]) -> Fixture {
    let mut hooks = json!({});
    for event in events {
        hooks[*event] = command(&format!("/bin/sh \"$HOME/hooks.sh\" {event}"));
    }
    let fixture = Fixture::new(json!({"hooks":hooks})).await;
    fs::write(
        fixture.root.path().join("home/hooks.sh"),
        "payload=$(cat)\nprintf '%s\\n' \"$payload\" >> \"$HOME/$CANTER_SESSION_ID-$1.jsonl\"\n",
    )
    .unwrap();
    fixture.trust(fixture.root.path()).await;
    fixture
}

fn records(fixture: &Fixture, id: &str, event: &str) -> Vec<Value> {
    fs::read_to_string(fixture.root.path().join(format!("home/{id}-{event}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn wait_records(fixture: &Fixture, id: &str, event: &str, count: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = records(fixture, id, event);
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn spawn(fixture: &mut Fixture, parent: &str) -> (String, Request, Request) {
    spawn_with(fixture, parent, false, "general").await
}

async fn spawn_with(
    fixture: &mut Fixture,
    parent: &str,
    non_interactive: bool,
    kind: &str,
) -> (String, Request, Request) {
    fixture.client.post(format!("{parent}/input"))
        .json(&json!({"input":[{"type":"text","text":"hello"}],"clientName":"test","nonInteractive":non_interactive}))
        .send().await.unwrap().error_for_status().unwrap();
    fixture.request().await.tool(
        "spawn_agent",
        json!({"task":"Inspect the parser", "type":kind, "name":"Trace parser errors", "depth":"medium"}),
    );
    let first = fixture.request().await;
    let second = fixture.request().await;
    let (child, parent_request) = if first.body["messages"]
        .to_string()
        .contains("You are a sub-agent")
    {
        (first, second)
    } else {
        (second, first)
    };
    let agents: Value = fixture
        .client
        .get(format!("{parent}/agents"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = agents["data"][0]["id"].as_str().unwrap();
    (
        format!("http://localhost/v1/sessions/{id}"),
        parent_request,
        child,
    )
}

#[tokio::test]
async fn child_hooks_identify_tools_and_only_check_human_prompts() {
    let mut fixture = fixture(ORDINARY).await;
    let parent = fixture.create().await;
    let parent_id = parent.rsplit('/').next().unwrap();
    let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
    let child_id = child.rsplit('/').next().unwrap();
    request.tool("write", json!({"path":"child.txt","content":"first"}));
    fixture.request().await.stop();
    fixture.idle(&child).await;
    // A client name is not an input-origin flag.
    fixture
        .client
        .post(format!("{child}/input"))
        .json(&json!({"input":[{"type":"text","text":"hello"}],"clientName":"agent"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.request().await.stop();
    fixture.idle(&child).await;
    fixture.close(&child).await;
    for event in [
        "SessionStart",
        "SessionEnd",
        "Stop",
        "StopFailure",
        "Interrupt",
    ] {
        assert!(
            records(&fixture, child_id, event).is_empty(),
            "child fired {event}"
        );
    }
    let prompts = records(&fixture, child_id, "UserPromptSubmit");
    assert_eq!(
        prompts.len(),
        1,
        "only direct human input runs prompt hooks"
    );
    assert_eq!(prompts[0]["prompt"], "hello");
    for event in ["PreToolUse", "PostToolUse", "UserPromptSubmit"] {
        let rows = records(&fixture, child_id, event);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_id"], child_id);
        assert_eq!(rows[0]["agent_name"], "Trace parser errors");
        assert_eq!(rows[0]["session_id"], child_id);
        assert_eq!(rows[0]["agent_type"], "general");
        assert_eq!(rows[0]["parent_session_id"], parent_id);
        assert_eq!(rows[0]["permission_mode"], "allowAll");
    }
    parent_request.stop();
    fixture.idle(&parent).await;
    fixture.close(&parent).await;
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        let rows = records(&fixture, parent_id, event);
        assert_eq!(rows.len(), 1, "parent {event}");
        for key in ["agent_id", "agent_name", "agent_type", "parent_session_id"] {
            assert!(rows[0].get(key).is_none());
        }
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn subagent_notifications_are_async_and_agent_inputs_skip_prompt_hooks() {
    let mut fixture = fixture(&["SubagentStart", "SubagentStop", "UserPromptSubmit"]).await;
    // Non-tool events ignore even an invalid regular expression.
    for event in ["SubagentStart", "SubagentStop"] {
        fixture.config["hooks"][event][0]["matcher"] = json!("[");
    }
    fixture.applied("subagent-matchers").await;
    let script = fixture.root.path().join("home/hooks.sh");
    let mut contents = fs::read_to_string(&script).unwrap();
    contents.push_str("case \"$1\" in Subagent*) while [ ! -f \"$HOME/release\" ]; do sleep 0.02; done; printf '%s' '{\"decision\":\"block\",\"reason\":\"hook-must-not-reach-model\",\"hookSpecificOutput\":{\"additionalContext\":\"hook-must-not-reach-model\"}}';; esac\n");
    fs::write(&script, contents).unwrap();
    let parent = fixture.create().await;
    let parent_id = parent.rsplit('/').next().unwrap();
    let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
    let id = child.rsplit('/').next().unwrap();
    let start = wait_records(&fixture, id, "SubagentStart", 1).await;
    assert_eq!(start[0]["task"], "Inspect the parser");
    assert_eq!(start[0]["parent_session_id"], parent_id);
    assert_eq!(start[0]["agent_id"], id);
    assert_eq!(start[0]["agent_name"], "Trace parser errors");
    assert_eq!(start[0]["session_id"], id);
    assert_eq!(start[0]["agent_type"], "general");
    parent_request.tool(
        "send_input",
        json!({"id":id,"message":"Queued automatic task"}),
    );
    let parent_request = fixture.request().await;
    fixture.send(&child, "Queued human task", "queue").await;
    request.stop();
    let automatic = fixture.request().await;
    assert!(
        automatic.body["messages"]
            .to_string()
            .contains("Queued automatic task")
    );
    assert!(records(&fixture, id, "UserPromptSubmit").is_empty());
    automatic.stop();
    let human = fixture.request().await;
    assert_eq!(
        records(&fixture, id, "UserPromptSubmit")[0]["prompt"],
        "Queued human task"
    );
    human.stop();
    fixture.idle(&child).await;
    let stops = wait_records(&fixture, id, "SubagentStop", 3).await;
    assert!(!fixture.root.path().join("home/release").exists());
    for stop in stops {
        assert_eq!(stop["status"], "completed");
        assert_eq!(stop["last_assistant_message"], "done");
        assert_eq!(stop["agent_id"], id);
        assert_eq!(stop["agent_name"], "Trace parser errors");
    }
    fs::write(fixture.root.path().join("home/release"), "").unwrap();
    parent_request.tool(
        "send_input",
        json!({"id":id,"message":"Idle automatic task"}),
    );
    let first = fixture.request().await;
    let second = fixture.request().await;
    assert!(!first.body.to_string().contains("hook-must-not-reach-model"));
    assert!(
        !second
            .body
            .to_string()
            .contains("hook-must-not-reach-model")
    );
    first.stop();
    second.stop();
    fixture.idle(&child).await;
    fixture.idle(&parent).await;
    fixture.close(&child).await;
    fixture.resume(&child).await;
    fixture.close(&child).await;
    assert_eq!(records(&fixture, id, "SubagentStart").len(), 1);
    assert_eq!(records(&fixture, id, "SubagentStop").len(), 4);
    assert_eq!(records(&fixture, id, "UserPromptSubmit").len(), 1);
    fixture.shutdown().await;
}

#[tokio::test]
// Esc in the parent no longer stops sub-agents (DESIGN, 11-09), so it is not an ending here.
async fn subagent_stop_reports_failure_interrupt_user_stop_and_close() {
    for ending in ["failed", "interrupt", "user", "close"] {
        let mut events = ORDINARY.to_vec();
        events.extend(["SubagentStart", "SubagentStop"]);
        let mut fixture = fixture(&events).await;
        fixture.config["model_max_retries"] = json!(0);
        fixture.applied("no-retries").await;
        let parent = fixture.create().await;
        let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
        let id = child.rsplit('/').next().unwrap();
        // A prior completed round must never supply the next round's last message.
        request.stop();
        fixture.idle(&child).await;
        fixture.input(&child).await;
        let waiting = fixture.request().await;
        let mut parent_request = Some(parent_request);
        match ending {
            "failed" => {
                waiting.reply.send("invalid stream".into()).unwrap();
            }
            "interrupt" => {
                fixture.interrupt_hook_turn(&child).await;
                drop(waiting);
            }
            "user" => {
                fixture
                    .client
                    .post(format!("{parent}/agents/{id}/stop"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                drop(waiting);
            }
            "close" => {
                parent_request
                    .take()
                    .unwrap()
                    .tool("close_agent", json!({"id":id}));
                parent_request = Some(fixture.request().await);
                drop(waiting);
            }
            _ => unreachable!(),
        }
        let stops = wait_records(&fixture, id, "SubagentStop", 2).await;
        assert_eq!(stops[0]["status"], "completed");
        assert_eq!(
            stops[1]["status"],
            match ending {
                "failed" => "failed",
                "close" => "closed",
                _ => "interrupted",
            }
        );
        assert_eq!(stops[1]["last_assistant_message"], "");
        if ending != "close" {
            fixture.close(&child).await;
        }
        for event in [
            "SessionStart",
            "SessionEnd",
            "Stop",
            "StopFailure",
            "Interrupt",
        ] {
            assert!(records(&fixture, id, event).is_empty(), "{ending}: {event}");
        }
        assert_eq!(records(&fixture, id, "SubagentStop").len(), 2);
        if let Some(request) = parent_request {
            request.stop();
            fixture.idle(&parent).await;
        }
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn child_tool_hooks_keep_denials_feedback_and_approval_notifications() {
    let mut fixture = fixture(ORDINARY).await;
    let script = fixture.root.path().join("home/hooks.sh");
    let mut contents = fs::read_to_string(&script).unwrap();
    contents.push_str("case \"$1:$payload\" in PreToolUse:*blocked.txt*) echo 'hook-denied-write' >&2; exit 2;; PostToolUse:*allowed.txt*) echo 'hook-feedback' >&2; exit 2;; esac\n");
    fs::write(script, contents).unwrap();
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
    let id = child.rsplit('/').next().unwrap();
    fixture
        .set(&parent, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    request.tool("write", json!({"path":"blocked.txt","content":"no"}));
    let next = fixture.request().await;
    assert!(next.body.to_string().contains("hook-denied-write"));
    assert!(!fixture.root.path().join("blocked.txt").exists());
    assert!(records(&fixture, id, "PermissionRequest").is_empty());
    assert!(records(&fixture, id, "PostToolUse").is_empty());
    next.tool("write", json!({"path":"allowed.txt","content":"yes"}));
    let permission = wait_records(&fixture, id, "PermissionRequest", 1).await;
    assert_eq!(permission[0]["agent_id"], id);
    assert_eq!(permission[0]["agent_name"], "Trace parser errors");
    assert_eq!(permission[0]["permission_mode"], "alwaysAsk");
    assert_eq!(permission[0]["tool_name"], "write");
    let pending = fixture.snapshot(&child).await.session.pending_requests;
    fixture
        .client
        .post(format!("{child}/requests/{}", pending[0].id))
        .json(&json!({"decision":"accept","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let next = fixture.request().await;
    assert!(next.body.to_string().contains("hook-feedback"));
    next.stop();
    fixture.idle(&child).await;
    fixture.close(&child).await;
    parent_request.stop();
    fixture.idle(&parent).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn exec_subagents_notify_and_restricted_children_keep_their_own_hook_policy() {
    for restricted in [false, true] {
        let mut events = ORDINARY.to_vec();
        events.extend(["SubagentStart", "SubagentStop"]);
        let mut fixture = fixture(&events).await;
        let created: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
            .json(&json!({"cwd":fixture.root.path(),"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"model":"fake","approvalPolicy":"allowAll","clientName":"test","restrictedMode":restricted}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let parent = format!("http://localhost/v1/sessions/{}", created.session.id);
        let (child, parent_request, request) =
            spawn_with(&mut fixture, &parent, true, "explorer").await;
        let id = child.rsplit('/').next().unwrap();
        if restricted {
            fixture.resume(&parent).await;
            assert_eq!(
                fixture
                    .snapshot(&parent)
                    .await
                    .session
                    .settings
                    .restricted_mode,
                Some(false)
            );
            assert_eq!(
                fixture
                    .snapshot(&child)
                    .await
                    .session
                    .settings
                    .restricted_mode,
                Some(true)
            );
        }
        fs::write(fixture.root.path().join("sample.txt"), "sample").unwrap();
        request.tool("read", json!({"path":"sample.txt"}));
        fixture.request().await.stop();
        fixture.idle(&child).await;
        fixture.close(&child).await;
        if restricted {
            for event in &events {
                assert!(
                    records(&fixture, id, event).is_empty(),
                    "restricted child: {event}"
                );
            }
        } else {
            assert_eq!(
                records(&fixture, id, "SubagentStart")[0]["agent_type"],
                "explorer"
            );
            assert_eq!(
                records(&fixture, id, "SubagentStop")[0]["status"],
                "completed"
            );
            assert_eq!(
                records(&fixture, id, "PreToolUse")[0]["agent_type"],
                "explorer"
            );
            assert_eq!(records(&fixture, id, "PostToolUse").len(), 1);
            assert!(records(&fixture, id, "UserPromptSubmit").is_empty());
        }
        parent_request.stop();
        fixture.idle(&parent).await;
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn child_client_exit_and_reconnect_do_not_emit_session_hooks() {
    let mut fixture = fixture(&[
        "SessionStart",
        "SessionEnd",
        "SubagentStart",
        "SubagentStop",
    ])
    .await;
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn_with(&mut fixture, &parent, true, "general").await;
    let id = child.rsplit('/').next().unwrap();
    let parent_id = parent.rsplit('/').next().unwrap();
    let connect = |id: &str| {
        fixture
            .client
            .get(format!("http://localhost/v1/events?session={id}"))
            .send()
    };
    let parent_client = connect(parent_id)
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let child_client = connect(id).await.unwrap().error_for_status().unwrap();
    request.stop();
    parent_request.stop();
    fixture.idle(&child).await;
    fixture.idle(&parent).await;
    drop(child_client);
    drop(parent_client);
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!(
        records(&fixture, parent_id, "SessionEnd")[0]["reason"],
        "client_exit"
    );
    let child_client = connect(id).await.unwrap().error_for_status().unwrap();
    let parent_client = connect(parent_id)
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_records(&fixture, parent_id, "SessionStart", 2).await;
    assert!(records(&fixture, id, "SessionStart").is_empty());
    assert!(records(&fixture, id, "SessionEnd").is_empty());
    assert_eq!(records(&fixture, id, "SubagentStart").len(), 1);
    assert_eq!(records(&fixture, id, "SubagentStop").len(), 1);
    drop(child_client);
    drop(parent_client);
    fixture.shutdown().await;
}

async fn notification_budget(close: bool) {
    let mut fixture = fixture(&["SubagentStart", "SubagentStop"]).await;
    let output = tempfile::tempdir().unwrap();
    let script = fixture.root.path().join("home/hooks.sh");
    let mut contents = fs::read_to_string(&script).unwrap();
    contents.push_str(&format!(
        "sleep 30 & echo $! > '{0}/'$1-child.pid; echo $$ > '{0}/'$1-shell.pid; wait\n",
        output.path().display()
    ));
    fs::write(script, contents).unwrap();
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn_with(&mut fixture, &parent, true, "general").await;
    request.stop();
    parent_request.stop();
    fixture.idle(&child).await;
    fixture.idle(&parent).await;
    for event in ["SubagentStart", "SubagentStop"] {
        fixture
            .hook_file(
                output
                    .path()
                    .join(format!("{event}-shell.pid"))
                    .to_str()
                    .unwrap(),
            )
            .await;
    }
    let started = tokio::time::Instant::now();
    let stop = async {
        if close {
            fixture.close(&child).await;
        }
        fixture.shutdown().await;
    };
    tokio::pin!(stop);
    let bounded = tokio::time::timeout(Duration::from_secs(12), &mut stop)
        .await
        .is_ok();
    if !bounded {
        for event in ["SubagentStart", "SubagentStop"] {
            let pid = fs::read_to_string(output.path().join(format!("{event}-shell.pid")))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        stop.await;
    }
    assert!(
        bounded,
        "sub-agent hooks share the ten-second notification budget"
    );
    assert!(started.elapsed() >= Duration::from_secs(10));
    for event in ["SubagentStart", "SubagentStop"] {
        assert_dead(&output.path().join(format!("{event}-shell.pid"))).await;
        assert_dead(&output.path().join(format!("{event}-child.pid"))).await;
    }
}

#[tokio::test]
async fn subagent_notifications_share_close_budget() {
    notification_budget(true).await;
}

#[tokio::test]
async fn subagent_notifications_share_server_shutdown_budget() {
    notification_budget(false).await;
}

fn charged(request: Request) {
    let chunk = json!({"choices":[{"index":0,"delta":{"content":"charged result"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":100,"prompt_cache_hit_tokens":40,"completion_tokens":10,"total_tokens":110}});
    request
        .reply
        .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        .unwrap();
}

#[tokio::test]
async fn stats_endpoint_exposes_child_usage_without_mixing_request_or_tool_rows() {
    let mut fixture = fixture(&[]).await;
    fixture.config["prices"] =
        json!({"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":100000}}});
    fixture.applied("stats-prices").await;
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn_with(&mut fixture, &parent, true, "general").await;
    let id = child.rsplit('/').next().unwrap();
    request.tool("write", json!({"path":"child.txt","content":"child work"}));
    charged(fixture.request().await);
    fixture.idle(&child).await;
    charged(parent_request);
    fixture.idle(&parent).await;
    let before = fixture.snapshot(&parent).await.event_seq;
    let report: Value = fixture
        .client
        .get(format!("{parent}/stats"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["overview"]["usage"]["total"]["outputTokens"], 10);
    assert_eq!(report["overview"]["usageWithSubAgents"]["outputTokens"], 20);
    assert_eq!(report["overview"]["usageWithSubAgents"]["cost"]["usd"], 2.0);
    assert_eq!(report["requests"].as_array().unwrap().len(), 2);
    assert_eq!(report["tools"].as_array().unwrap().len(), 1);
    assert_eq!(report["tools"][0]["name"], "spawn_agent");
    let row = &report["subAgents"][0];
    assert_eq!(row["id"], id);
    assert_eq!(row["name"], "Trace parser errors");
    assert_eq!(row["requestCount"], 2);
    assert_eq!(row["toolCount"], 1);
    assert_eq!(row["usage"]["cost"]["usd"], 1.0);
    assert!(row["durationMs"].is_number());
    assert_eq!(fixture.snapshot(&parent).await.event_seq, before);
    let child_report: Value = fixture
        .client
        .get(format!("{child}/stats"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        child_report["parent"]["sessionId"],
        parent.rsplit('/').next().unwrap()
    );
    assert_eq!(child_report["parent"]["agentType"], "general");
    assert_eq!(child_report["tools"][0]["name"], "write");
    let list: Value = fixture
        .client
        .get("http://localhost/v1/stats?all=true")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    fixture.close(&child).await;
    fixture
        .client
        .post(format!("{child}/trash"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let report: Value = fixture
        .client
        .get(format!("{parent}/stats"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(report.get("subAgents").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn goal_budget_interrupts_child_and_still_emits_subagent_stop() {
    let mut fixture = fixture(&["SubagentStart", "SubagentStop"]).await;
    fixture.config["prices"] =
        json!({"fake":{"peak":{"cache_hit":0,"cache_miss":0,"output":100000}}});
    fixture.applied("budget-prices").await;
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
    let id = child.rsplit('/').next().unwrap();
    fixture
        .client
        .post(format!("{parent}/goal"))
        .json(&json!({"objective":"Finish the work","budgetUsd":1}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Start another child request after the goal exists, so its charge belongs to it.
    request.tool("read", json!({"path":"missing.txt"}));
    charged(fixture.request().await);
    let stops = wait_records(&fixture, id, "SubagentStop", 1).await;
    assert_eq!(stops[0]["status"], "interrupted");
    assert_eq!(stops[0]["last_assistant_message"], "charged result");
    drop(parent_request);
    fixture.close(&child).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn child_background_wake_skips_prompt_hooks_and_notifies_each_turn() {
    let mut fixture = fixture(&["SubagentStart", "SubagentStop", "UserPromptSubmit"]).await;
    let parent = fixture.create().await;
    let (child, parent_request, request) = spawn(&mut fixture, &parent).await;
    let id = child.rsplit('/').next().unwrap();
    request.tool("shell", json!({"command":"while [ ! -f finish-background ]; do sleep 0.02; done", "background":true}));
    fixture.request().await.stop();
    fixture.idle(&child).await;
    fs::write(fixture.root.path().join("finish-background"), "").unwrap();
    fixture.request().await.stop();
    fixture.idle(&child).await;
    fixture.close(&child).await;
    assert!(records(&fixture, id, "UserPromptSubmit").is_empty());
    assert_eq!(records(&fixture, id, "SubagentStart").len(), 1);
    assert_eq!(records(&fixture, id, "SubagentStop").len(), 2);
    parent_request.stop();
    fixture.idle(&parent).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn server_shutdown_reports_running_child_as_interrupted() {
    let mut fixture = fixture(&["SubagentStop"]).await;
    let output = tempfile::tempdir().unwrap();
    fs::write(
        fixture.root.path().join("home/hooks.sh"),
        format!("cat > '{}'\n", output.path().join("stop.json").display()),
    )
    .unwrap();
    let parent = fixture.create().await;
    let (_, parent_request, child_request) = spawn(&mut fixture, &parent).await;
    fixture.shutdown().await;
    drop(parent_request);
    drop(child_request);
    let stop: Value =
        serde_json::from_slice(&fs::read(output.path().join("stop.json")).unwrap()).unwrap();
    assert_eq!(stop["status"], "interrupted");
    assert_eq!(stop["last_assistant_message"], "");
}
