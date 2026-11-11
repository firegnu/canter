use super::*;
mod client_exit;
mod context;
mod subagents;

fn command(script: &str) -> Value {
    json!([{"hooks":[{"type":"command","command":script}]}])
}

#[tokio::test]
async fn hooks_use_session_environment_and_override_session_identity() {
    let fixture = Fixture::new(json!({"hooks":{"SessionStart":command(
        r#"cat >/dev/null; printf '["%s","%s","%s","%s","%s"]' "$CLIENT_HOOK_VALUE" "${CANTER_RELOAD_TEST-unset}" "$CANTER_SESSION_ID" "$CANTER_PROJECT_DIR" "$TMPDIR" > environment.json"#
    )}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture
        .create_with_env(json!({
            "CLIENT_HOOK_VALUE":"client-only-value",
            "CANTER_SESSION_ID":"fake-session-id",
            "CANTER_PROJECT_DIR":"/fake-project",
            "TMPDIR":"/client-temp"
        }))
        .await;
    let environment = fixture.hook_file("environment.json").await;
    let expected = json!([
        "client-only-value",
        "unset",
        url.rsplit('/').next().unwrap(),
        fixture.root.path(),
        "/client-temp"
    ]);
    fixture.shutdown().await;
    assert_eq!(environment, expected);
}

// The write/edit result on main for replacing "old\n" with "new\n".
const FILE_RESULT: &str = "Updated nested/sample.txt: 1 lines added, 1 lines removed.";

async fn file_tool_result(config: Value, tool: &str) -> String {
    let mut fixture = Fixture::new(config).await;
    fixture.trust(fixture.root.path()).await;
    fs::create_dir(fixture.root.path().join("nested")).unwrap();
    fs::write(fixture.root.path().join("nested/sample.txt"), "old\n").unwrap();
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    let prefix = first.body["messages"].as_array().unwrap().clone();
    let arguments = if tool == "write" {
        json!({"path":"nested/sample.txt","content":"new\n"})
    } else {
        json!({"path":"nested/sample.txt","edits":[{"oldText":"old","newText":"new"}]})
    };
    first.tool(tool, arguments);
    let next = fixture.request().await;
    let messages = next.body["messages"].as_array().unwrap();
    assert_eq!(&messages[..prefix.len()], prefix);
    let result = messages.iter().rev().find(|m| m["role"] == "tool").unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
    result
}

#[tokio::test]
async fn post_tool_file_changes_and_deletions_are_reported_regardless_of_hook_exit() {
    for tool in ["write", "edit"] {
        for (script, feedback) in [
            ("printf 'fmt\\n' > nested/sample.txt", ""),
            (
                "printf 'fmt\\n' > nested/sample.txt; printf 'formatter feedback' >&2; exit 2",
                "\n\nPostToolUse hook: formatter feedback",
            ),
            ("printf 'fmt\\n' > nested/sample.txt; exit 7", ""),
            ("rm nested/sample.txt", ""),
        ] {
            let result = file_tool_result(
                json!({"hooks":{"PostToolUse":command(&format!("cat >/dev/null; {script}"))}}),
                tool,
            )
            .await;
            assert_eq!(
                result,
                format!(
                    "{FILE_RESULT}{feedback}\nnested/sample.txt was changed by a PostToolUse hook after this call; read it again before editing it."
                ),
                "{tool}: {script}"
            );
        }
    }
}

#[tokio::test]
async fn post_tool_unchanged_files_and_unmatched_hooks_preserve_the_result() {
    for tool in ["write", "edit"] {
        for config in [
            json!({"hooks":{"PostToolUse":command("cat >/dev/null")}}),
            json!({"hooks":{"PostToolUse":command("cat >/dev/null; printf 'new\\n' > nested/sample.txt")}}),
            json!({"hooks":{"PostToolUse":command("cat >/dev/null; touch nested/sample.txt")}}),
            json!({"hooks":{"PostToolUse":command("cat >/dev/null; printf 'other\\n' > other.txt")}}),
            json!({"hooks":{"PostToolUse":[{"matcher":"read","hooks":[{"type":"command","command":"printf 'fmt\\n' > nested/sample.txt"}]}]}}),
            json!({"hooks":{"PostToolUse":[{"matcher":"write|edit","hooks":[]}]}}),
        ] {
            assert_eq!(
                file_tool_result(config.clone(), tool).await,
                FILE_RESULT,
                "{tool}: {config}"
            );
        }
    }
}

#[tokio::test]
async fn file_tool_results_without_hooks_match_main_byte_for_byte() {
    for tool in ["write", "edit"] {
        assert_eq!(
            file_tool_result(json!({}), tool).await,
            FILE_RESULT,
            "{tool}"
        );
    }
}

impl Fixture {
    async fn hook_file(&self, name: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = fs::read(self.root.path().join(name))
                    && let Ok(value) = serde_json::from_slice(&bytes)
                {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("hook must write complete JSON")
    }
    async fn set(&self, url: &str, params: Value) {
        self.client
            .patch(url)
            .json(&params)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn close(&self, url: &str) {
        self.client
            .delete(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn resume(&self, url: &str) {
        self.client.post(format!("{url}/resume")).json(&json!({"env":{"HOME":self.root.path().join("home"),"CANTER_HOME":self.root.path().join("state")}})).send().await.unwrap().error_for_status().unwrap();
    }
    async fn send(&self, url: &str, text: &str, mode: &str) {
        self.client
            .post(format!("{url}/input"))
            .json(&json!({"input":[{"type":"text","text":text}],"mode":mode,"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn interrupt_hook_turn(&self, url: &str) {
        let snapshot = self.snapshot(url).await;
        let turn = snapshot.session.turns.last().unwrap();
        self.client
            .post(format!("{url}/interrupt"))
            .json(&json!({"turnId":turn.id}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
}

#[tokio::test]
async fn prompt_block_never_reaches_the_model_even_after_resume() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":command("cat > prompt.json; printf 'please revise the prompt' >&2; exit 2")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    fixture.send(&url, "blocked original text", "queue").await;
    assert!(warnings.next().await.contains("please revise the prompt"));
    assert_eq!(
        fixture.hook_file("prompt.json").await["prompt"],
        "blocked original text"
    );
    assert!(fixture.requests.try_recv().is_err());
    let snapshot = fixture.snapshot(&url).await;
    assert!(snapshot.session.turns.iter().all(|t| {
        t.input.is_empty()
            && !t
                .items
                .iter()
                .any(|i| matches!(i, canter_protocol::Item::UserMessage { .. }))
    }));
    fixture.close(&url).await;
    fixture.config["hooks"] = json!({});
    fixture.applied("no-hooks").await;
    fixture.resume(&url).await;
    fixture.send(&url, "accepted later text", "queue").await;
    let request = fixture.request().await;
    assert!(!request.body.to_string().contains("blocked original text"));
    assert!(request.body.to_string().contains("accepted later text"));
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn pre_tool_denial_prevents_approval_execution_and_post_tool_notification() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "PreToolUse":command("cat > pre.json; printf '%s' '{\"hookSpecificOutput\":{\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"no file today\"}}'"),
        "PermissionRequest":command("cat > permission.json"),
        "PostToolUse":command("cat > post.json")
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture
        .set(&url, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"denied.txt","content":"denied"}));
    let next = fixture.request().await;
    assert!(next.body.to_string().contains("no file today"));
    assert!(!fixture.root.path().join("denied.txt").exists());
    assert!(!fixture.root.path().join("permission.json").exists());
    assert!(!fixture.root.path().join("post.json").exists());
    assert_eq!(fixture.hook_file("pre.json").await["tool_name"], "write");
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn lifecycle_notifications_and_permission_cannot_approve_or_continue_a_turn() {
    let mut hooks = json!({});
    for event in [
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "Stop",
        "StopFailure",
        "Interrupt",
    ] {
        hooks[event] = command(&format!(
            "cat > {event}.json; printf '%s' '{{\"hookSpecificOutput\":{{\"permissionDecision\":\"allow\"}}}}'"
        ));
    }
    hooks["Stop"] = command(
        "cat > Stop.json; printf '%s' '{\"decision\":\"block\",\"reason\":\"do not continue\"}'",
    );
    let mut fixture = Fixture::new(json!({"hooks":hooks,"model_max_retries":0})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    assert_eq!(
        fixture.hook_file("SessionStart.json").await["source"],
        "startup"
    );
    fixture
        .set(&url, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"allowed.txt","content":"ok"}));
    assert_eq!(
        fixture.hook_file("PermissionRequest.json").await["tool_name"],
        "write"
    );
    assert!(!fixture.root.path().join("allowed.txt").exists());
    let pending = fixture.snapshot(&url).await.session.pending_requests;
    assert_eq!(pending.len(), 1);
    fixture
        .client
        .post(format!("{url}/requests/{}", pending[0].id))
        .json(&json!({"decision":"accept","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fixture.hook_file("Stop.json").await["last_assistant_message"],
        "done"
    );
    assert!(fixture.requests.try_recv().is_err());
    assert!(!fixture.root.path().join("StopFailure.json").exists());
    assert!(!fixture.root.path().join("Interrupt.json").exists());
    fixture.input(&url).await;
    let waiting = fixture.request().await;
    fixture.interrupt_hook_turn(&url).await;
    drop(waiting);
    assert_eq!(
        fixture.hook_file("Interrupt.json").await["hook_event_name"],
        "Interrupt"
    );
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .reply
        .send("invalid stream".into())
        .unwrap();
    assert!(fixture.hook_file("StopFailure.json").await["error"].is_string());
    fixture.close(&url).await;
    assert_eq!(
        fixture.hook_file("SessionEnd.json").await["reason"],
        "client_request"
    );
    for event in [
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "Stop",
        "StopFailure",
        "Interrupt",
    ] {
        context::assert_transcript(
            &fixture,
            &url,
            &fixture.hook_file(&format!("{event}.json")).await,
        );
    }
    fs::remove_file(fixture.root.path().join("SessionStart.json")).unwrap();
    fixture.resume(&url).await;
    assert_eq!(
        fixture.hook_file("SessionStart.json").await["source"],
        "resume"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn queued_and_steered_prompts_are_checked_when_consumed() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":command(
        "input=$(cat); printf '%s\\n' \"$input\" >> prompts.jsonl; case \"$input\" in *reject*) printf '%s' '{\"decision\":\"block\",\"reason\":\"rejected input\"}';; esac"
    )}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    let mut warnings = fixture.warnings(&url).await;
    fixture.send(&url, "reject steer", "steer").await;
    fixture.send(&url, "reject queue", "queue").await;
    first.tool("read", json!({"path":"prompts.jsonl"}));
    let next = fixture.request().await;
    // The read result contains the captured first prompt only; rejected steering
    // must not appear as a user message or hidden model instruction.
    for message in next.body["messages"].as_array().unwrap() {
        if message["role"] == "user" {
            assert!(!message.to_string().contains("reject"));
        }
    }
    assert!(warnings.next().await.contains("rejected input"));
    next.stop();
    assert!(warnings.next().await.contains("rejected input"));
    fixture.idle(&url).await;
    assert!(fixture.requests.try_recv().is_err());
    let prompts = fs::read_to_string(fixture.root.path().join("prompts.jsonl")).unwrap();
    assert_eq!(prompts.lines().count(), 3);
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .queued_inputs
            .is_empty()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn hooks_reload_at_turn_boundaries_and_invalid_config_keeps_the_entire_old_config() {
    let mut fixture = Fixture::new(json!({"hooks":{"PreToolUse":command("cat > old.json")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    fixture.config["hooks"]["PreToolUse"] = command("cat > new.json");
    fixture.applied("new-hooks").await;
    first.tool("write", json!({"path":"test.txt","content":"ok"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.hook_file("old.json").await["tool_name"], "write");
    assert!(!fixture.root.path().join("new.json").exists());
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("read", json!({"path":"test.txt"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.hook_file("new.json").await["tool_name"], "read");
    for invalid in [
        json!({"NotAnEvent":[]}),
        json!({"PreToolUse":[{"matcher":"[","hooks":[]}]}),
        json!({"Stop":[{"hooks":[{"type":"http","command":"true"}]}]}),
        json!({"Stop":[{"hooks":[{"type":"command","command":"true","timeout":0}]}]}),
    ] {
        let mut warnings = fixture.warnings(&url).await;
        fixture.config["hooks"] = invalid;
        fixture.config["context_window"] = json!(524288);
        fixture.save();
        assert!(
            warnings
                .next()
                .await
                .contains("Keeping the previous configuration")
        );
        fixture.input(&url).await;
        fixture.request().await.stop();
        fixture.idle(&url).await;
        assert_eq!(fixture.context(&url).await.model_context_window, 1048576);
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn bad_hook_output_exit_and_timeout_fail_open_with_warnings() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":[{"hooks":[
        {"type":"command","command":"cat >/dev/null; printf garbage"},
        {"type":"command","command":"cat >/dev/null; exit 7"},
        {"type":"command","command":"cat >/dev/null; sleep 30 & echo $! > child.pid; wait","timeout":0.2}
    ]}]}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    fixture.input(&url).await;
    let request = fixture.request().await;
    let mut all = String::new();
    for _ in 0..3 {
        all.push_str(&warnings.next().await);
    }
    assert!(all.contains("invalid JSON output"));
    assert!(all.contains("unexpected exit status"));
    assert!(all.contains("timed out"));
    assert!(all.contains("UserPromptSubmit hook"));
    assert!(all.contains("printf garbage"));
    assert!(!request.body.to_string().contains("invalid JSON output"));
    assert_dead(&fixture.root.path().join("child.pid")).await;
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

async fn assert_dead(path: &std::path::Path) {
    let pid: i32 = fs::read_to_string(path).unwrap().trim().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("hook descendant must be killed");
}

#[tokio::test]
async fn interrupt_kills_hooks_without_locking_snapshots_or_other_sessions() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":command("cat > waiting.json; sleep 30 & echo $! > child.pid; wait")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.hook_file("waiting.json").await;
    fixture.config["hooks"] = json!({});
    fixture.applied("other-session").await;
    let other = fixture.create().await;
    fixture.input(&other).await;
    fixture.request().await.stop();
    fixture.idle(&other).await;
    fixture.interrupt_hook_turn(&url).await;
    assert_dead(&fixture.root.path().join("child.pid")).await;
    assert!(fixture.requests.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn session_start_waits_before_turns_and_matching_commands_run_in_parallel() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command("cat > start.json; while [ ! -f release ]; do sleep 0.02; done"),
        "PreToolUse":[{"matcher":"read|write","hooks":[
            {"type":"command","command":"cat >/dev/null; while [ ! -f second ]; do sleep 0.02; done; touch first","timeout":1},
            {"type":"command","command":"cat >/dev/null; touch second","timeout":1}
        ]}]
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.hook_file("start.json").await;
    fixture.input(&url).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.requests.recv())
            .await
            .is_err()
    );
    fs::write(fixture.root.path().join("release"), "").unwrap();
    fixture
        .request()
        .await
        .tool("write", json!({"path":"parallel.txt","content":"ok"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(fixture.root.path().join("first").exists());
    fixture.shutdown().await;
}

#[tokio::test]
async fn restricted_sessions_and_hard_denials_never_run_hooks() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command(r#"cat > start.json; printf '%s' '{"hookSpecificOutput":{"additionalContext":"restricted start context"}}'"#),
        "UserPromptSubmit":command(r#"cat > prompt.json; printf '%s' '{"hookSpecificOutput":{"additionalContext":"restricted input context"}}'"#),
        "PreToolUse":command("cat > pre.json"),
        "PostToolUse":command("cat > post.json"),
        "Stop":command("cat > stop.json")
    }}))
    .await;
    let restricted = fixture.create().await;
    fixture.input(&restricted).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"restricted.txt","content":"ok"}));
    fixture.request().await.stop();
    fixture.idle(&restricted).await;
    fixture.close(&restricted).await;
    assert!(
        !serde_json::to_string(&fixture.snapshot(&restricted).await)
            .unwrap()
            .contains("hookContext")
    );
    for name in [
        "start.json",
        "prompt.json",
        "pre.json",
        "post.json",
        "stop.json",
    ] {
        assert!(!fixture.root.path().join(name).exists(), "{name}");
    }
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.set(&url, json!({"planMode":true})).await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"plan-denied.txt","content":"no"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(!fixture.root.path().join("pre.json").exists());
    assert!(!fixture.root.path().join("post.json").exists());
    fixture.set(&url, json!({"planMode":false})).await;
    fs::write(
        fixture.root.path().join("state/rules.json"),
        json!({"fileRules":[{"pattern":"blocked.txt","decision":"forbidden"}]}).to_string(),
    )
    .unwrap();
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"blocked.txt","content":"no"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(!fixture.root.path().join("pre.json").exists());
    assert!(!fixture.root.path().join("post.json").exists());
    fixture.shutdown().await;
}

#[tokio::test]
async fn automatic_goal_and_background_turns_skip_prompt_hooks() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "UserPromptSubmit":command("cat >> prompts.jsonl; printf '\\n' >> prompts.jsonl"),
        "Stop":command("cat >> stops.jsonl; printf '\\n' >> stops.jsonl")
    }}))
    .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"test objective","budgetUsd":0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.request().await.stop();
    let next = fixture.request().await;
    assert!(!fixture.root.path().join("prompts.jsonl").exists());
    next.tool("update_goal", json!({"status":"complete"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    fixture.input(&url).await;
    fixture.request().await.tool("shell", json!({"command":"while [ ! -f finish-background ]; do sleep 0.02; done","background":true}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    fs::write(fixture.root.path().join("finish-background"), "").unwrap();
    let wake = fixture.request().await;
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("prompts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    wake.stop();
    fixture.idle(&url).await;
    fixture.close(&url).await;
    let stops = fs::read_to_string(fixture.root.path().join("stops.jsonl")).unwrap();
    assert_eq!(stops.lines().count(), 4);
    fixture.shutdown().await;
}

#[tokio::test]
async fn whole_tool_names_environment_failed_tools_and_output_limits() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "PreToolUse":[
            {"matcher":"ell","hooks":[{"type":"command","command":"touch partial-match"}]},
            {"matcher":"shell","hooks":[{"type":"command","command":"cat > pre.json; printf '%s\\n%s\\n%s' \"$CANTER_SESSION_ID\" \"$CANTER_PROJECT_DIR\" \"${TMPDIR-}\" > environment.txt"}]
            }],
        "PostToolUse":[{"matcher":"shell","hooks":[{"type":"command","command":"cat > failed.json; head -c 100000 /dev/zero | tr '\\000' x >&2; exit 2"}]}]
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("shell", json!({"command":"exit 1"}));
    let request = fixture.request().await;
    assert!(!fixture.root.path().join("partial-match").exists());
    assert_eq!(fixture.hook_file("failed.json").await["tool_name"], "shell");
    let result = request.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(result.contains("PostToolUse hook: xxxx"));
    assert!(result.len() < 34000);
    let env = fs::read_to_string(fixture.root.path().join("environment.txt")).unwrap();
    assert!(env.starts_with(&format!(
        "{}\n{}\n",
        url.rsplit('/').next().unwrap(),
        fixture.root.path().display()
    )));
    assert!(!env.contains("canter-shell-"));
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn permissions_are_rechecked_after_waiting_for_pre_tool_hooks() {
    let mut fixture = Fixture::new(json!({"hooks":{"PreToolUse":command("cat > waiting.json; while [ ! -f release ]; do sleep 0.02; done")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.request().await.tool(
        "write",
        json!({"path":"should-not-exist.txt","content":"no"}),
    );
    fixture.hook_file("waiting.json").await;
    fixture.set(&url, json!({"planMode":true})).await;
    fs::write(fixture.root.path().join("release"), "").unwrap();
    let request = fixture.request().await;
    assert!(!fixture.root.path().join("should-not-exist.txt").exists());
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .pending_requests
            .is_empty()
    );
    request.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn blocked_late_steering_does_not_restart_the_model() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":command("input=$(cat); case \"$input\" in *reject*) echo 'rejected late input' >&2; exit 2;; esac")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    let mut warnings = fixture.warnings(&url).await;
    fixture.send(&url, "reject late steering", "steer").await;
    first.stop();
    assert!(warnings.next().await.contains("rejected late input"));
    fixture.idle(&url).await;
    assert!(fixture.requests.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn missing_working_directory_fails_open_and_server_shutdown_notifies() {
    let mut fixture = Fixture::new(json!({"hooks":{"UserPromptSubmit":command("true")}})).await;
    let cwd = fixture.root.path().join("work");
    fs::create_dir(&cwd).unwrap();
    fixture.trust(&cwd).await;
    let created: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":cwd,"env":{"HOME":fixture.root.path().join("home")},"model":"fake","approvalPolicy":"allowAll","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let mut warnings = fixture.warnings(&url).await;
    fs::remove_dir(&cwd).unwrap();
    fixture.input(&url).await;
    let request = fixture.request().await;
    assert!(warnings.next().await.contains("could not start"));
    request.stop();
    fixture.idle(&url).await;
    fixture.close(&url).await;
    let output = tempfile::tempdir().unwrap();
    fixture.config["hooks"] = json!({"SessionEnd":command(&format!("cat > '{}'", output.path().join("end.json").display()))});
    fixture.applied("shutdown-hooks").await;
    fixture.trust(fixture.root.path()).await;
    fixture.create().await;
    fixture.shutdown().await;
    let event: Value =
        serde_json::from_slice(&fs::read(output.path().join("end.json")).unwrap()).unwrap();
    assert_eq!(event["reason"], "server_shutdown");
}

#[tokio::test]
async fn pre_tool_use_also_blocks_questions_without_opening_a_request() {
    let mut fixture = Fixture::new(json!({"hooks":{"PreToolUse":[{"matcher":"ask_user","hooks":[{"type":"command","command":"cat > question.json; echo 'no question needed' >&2; exit 2"}]}]}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.request().await.tool("ask_user", json!({"question":"Continue?","options":[{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]}));
    let next = fixture.request().await;
    assert!(next.body.to_string().contains("no question needed"));
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .pending_requests
            .is_empty()
    );
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn plan_approval_notifies_permission_request() {
    let mut fixture =
        Fixture::new(json!({"hooks":{"PermissionRequest":command("cat > plan-permission.json")}}))
            .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.set(&url, json!({"planMode":true})).await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("submit_plan", json!({"plan":"Investigate then implement."}));
    let input = fixture.hook_file("plan-permission.json").await;
    assert_eq!(input["tool_name"], "submit_plan");
    assert_eq!(input["tool_input"]["plan"], "Investigate then implement.");
    let pending = fixture.snapshot(&url).await.session.pending_requests;
    assert_eq!(pending.len(), 1);
    fixture.interrupt_hook_turn(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn review_keeps_question_dialogs_and_the_original_submit_plan_response() {
    let mut fixture = Fixture::new(json!({"hooks":{"PreToolUse":[
        {"matcher":"ask_user","hooks":[{"type":"command","command":"cat > review-question.json"}]},
        {"matcher":"submit_plan","hooks":[{"type":"command","command":"cat > review-plan.json"}]}
    ]}}))
    .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture
        .set(&url, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    fixture.client.post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"Review the changes."}],"review":"Review: changes","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap();
    fixture.request().await.tool("ask_user", json!({"question":"Which files?","options":[{"label":"All","description":"All files"},{"label":"Rust","description":"Rust files"}]}));
    let pending = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = fixture.snapshot(&url).await.session.pending_requests;
            if !pending.is_empty() {
                break pending;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("review ask_user must open a question dialog");
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].kind, canter_protocol::RequestKind::UserInput(question) if question.question == "Which files?")
    );
    assert_eq!(
        fixture.hook_file("review-question.json").await["tool_name"],
        "ask_user"
    );
    fixture
        .client
        .post(format!("{url}/requests/{}", pending[0].id))
        .json(&json!({"answer":{"type":"option","label":"Rust"},"clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .request()
        .await
        .tool("submit_plan", json!({"plan":"Inspect the Rust changes."}));
    let next = fixture.request().await;
    let result = next.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(
        result["content"],
        "This session is not in Plan mode. Only the user can enable Plan mode; continue the task under the current permission policy."
    );
    assert_eq!(
        fixture.hook_file("review-plan.json").await["tool_name"],
        "submit_plan"
    );
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .pending_requests
            .is_empty()
    );
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn close_bounds_notification_wait_and_kills_hook_process_groups() {
    stalled_notifications_shutdown(true).await;
}

#[tokio::test]
async fn shutdown_bounds_notification_wait_and_kills_hook_process_groups() {
    stalled_notifications_shutdown(false).await;
}

async fn stalled_notifications_shutdown(close_session: bool) {
    let output = tempfile::tempdir().unwrap();
    let events = ["Stop", "PermissionRequest", "Interrupt", "SessionEnd"];
    let mut hooks = json!({});
    for event in events {
        hooks[event] = command(&format!(
            "cat >/dev/null; sleep 30 & echo $! > '{0}/{event}-child.pid'; echo $$ > '{0}/{event}-shell.pid'; wait",
            output.path().display()
        ));
    }
    let mut fixture = Fixture::new(json!({"hooks":hooks})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    fixture
        .set(&url, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"pending.txt","content":"pending"}));
    fixture
        .hook_file(
            output
                .path()
                .join("PermissionRequest-shell.pid")
                .to_str()
                .unwrap(),
        )
        .await;
    fixture.interrupt_hook_turn(&url).await;
    for event in &events[..3] {
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
    let stop = async {
        if close_session {
            fixture.close(&url).await;
        }
        fixture.shutdown().await;
    };
    tokio::pin!(stop);
    // The collective budget is 10 seconds, with 2 seconds for HTTP/process cleanup.
    let started = tokio::time::Instant::now();
    let bounded = tokio::time::timeout(Duration::from_secs(12), &mut stop)
        .await
        .is_ok();
    if !bounded {
        // Clean up the deliberately stalled hooks before reporting a regression.
        for event in events {
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
    assert!(bounded, "notification hooks exceeded the shutdown budget");
    assert!(
        started.elapsed() >= Duration::from_secs(10),
        "notifications lost their ten-second budget"
    );
    for event in events {
        assert_dead(&output.path().join(format!("{event}-shell.pid"))).await;
        assert_dead(&output.path().join(format!("{event}-child.pid"))).await;
    }
}

#[tokio::test]
async fn accepted_prompt_retains_the_turn_input_in_snapshots_and_replay() {
    let mut fixture =
        Fixture::new(json!({"hooks":{"UserPromptSubmit":command("cat >/dev/null")}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    let expected = vec![canter_protocol::UserInput::Text {
        text: "hello".into(),
    }];
    assert_eq!(
        fixture.snapshot(&url).await.session.turns[0].input,
        expected
    );
    fixture.close(&url).await;
    fixture.resume(&url).await;
    assert_eq!(
        fixture.snapshot(&url).await.session.turns[0].input,
        expected
    );
    fixture.shutdown().await;
}
