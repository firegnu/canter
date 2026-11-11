use super::*;

async fn fixture() -> (Fixture, String) {
    let fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command("input=$(cat); printf '%s\\n' \"$input\" >> starts.jsonl"),
        "SessionEnd":command("input=$(cat); printf '%s\\n' \"$input\" >> ends.jsonl")
    }}))
    .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    wait_lines(&fixture, "starts.jsonl", 1).await;
    (fixture, url)
}

fn lines(fixture: &Fixture, name: &str) -> Vec<Value> {
    fs::read_to_string(fixture.root.path().join(name))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn wait_lines(fixture: &Fixture, name: &str, count: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let values = lines(fixture, name);
            if values.len() >= count {
                return values;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lifecycle hook must run")
}

async fn connect(fixture: &Fixture, url: &str) -> reqwest::Response {
    connect_query(fixture, url, "").await
}

async fn connect_query(fixture: &Fixture, url: &str, query: &str) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .unix_socket(fixture.root.path().join("state/server.sock"))
        .build()
        .unwrap()
        .get(format!(
            "http://localhost/v1/events?session={}{query}",
            url.rsplit('/').next().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
}

#[tokio::test]
async fn observer_receives_events_without_delaying_end_or_resuming_client_lifecycle() {
    let (mut fixture, url) = fixture().await;
    let mut client = connect(&fixture, &url).await.bytes_stream().eventsource();
    let mut observer = connect_query(&fixture, &url, "&observe=true")
        .await
        .bytes_stream()
        .eventsource();
    let first = client.next().await.unwrap().unwrap();
    let observed = observer.next().await.unwrap().unwrap();
    assert_eq!(observed.data, first.data);
    assert_eq!(observed.id, first.id);
    fixture
        .client
        .patch(&url)
        .json(&json!({"name":"Observed change"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let observed = tokio::time::timeout(Duration::from_secs(5), observer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(observed.data, event.data);
        assert_eq!(observed.id, event.id);
        if event.data.contains("Observed change") {
            break;
        }
    }
    drop(client);
    let ended = wait_lines(&fixture, "ends.jsonl", 1).await;
    assert_eq!(ended[0]["reason"], "client_exit");
    let another = connect_query(&fixture, &url, "&observe=true").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(lines(&fixture, "starts.jsonl").len(), 1);
    let ordinary = connect(&fixture, &url).await;
    assert_eq!(
        wait_lines(&fixture, "starts.jsonl", 2).await[1]["source"],
        "resume"
    );
    drop((ordinary, observer, another));
    stop(&mut fixture).await;
}

async fn stop(fixture: &mut Fixture) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(fixture.child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), fixture.child.wait())
        .await
        .unwrap()
        .unwrap();
    fixture.model.abort();
    assert!(
        status.success(),
        "{}",
        fs::read_to_string(fixture.root.path().join("server.log")).unwrap()
    );
}

#[tokio::test]
async fn last_client_leaves_without_closing_the_session_and_shutdown_does_not_repeat_end() {
    let (mut fixture, url) = fixture().await;
    let client = connect(&fixture, &url).await;
    let before = fixture.snapshot(&url).await;
    drop(client);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(lines(&fixture, "ends.jsonl").is_empty());
    let ended = wait_lines(&fixture, "ends.jsonl", 1).await;
    assert_eq!(ended[0]["reason"], "client_exit");
    assert_eq!(
        serde_json::to_value(fixture.snapshot(&url).await).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    stop(&mut fixture).await;
    assert_eq!(lines(&fixture, "ends.jsonl"), ended);
}

#[tokio::test]
async fn clients_share_one_start_and_end_pair_and_a_later_connection_resumes() {
    let (mut fixture, url) = fixture().await;
    let first = connect(&fixture, &url).await;
    fixture.resume(&url).await;
    let second = connect(&fixture, &url).await;
    drop(first);
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert!(
        lines(&fixture, "ends.jsonl").is_empty(),
        "another client is still attached"
    );
    assert_eq!(
        lines(&fixture, "starts.jsonl").len(),
        1,
        "resume must not duplicate the attached period's start"
    );
    drop(second);
    wait_lines(&fixture, "ends.jsonl", 1).await;
    // One-shot reads/resume requests are not attached clients.
    fixture.resume(&url).await;
    fixture.snapshot(&url).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(lines(&fixture, "starts.jsonl").len(), 1);
    let third = connect(&fixture, &url).await;
    let starts = wait_lines(&fixture, "starts.jsonl", 2).await;
    assert_eq!(starts[1]["source"], "resume");
    let fourth = connect(&fixture, &url).await;
    drop((third, fourth));
    wait_lines(&fixture, "ends.jsonl", 2).await;
    fixture.close(&url).await;
    stop(&mut fixture).await;
    assert_eq!(lines(&fixture, "starts.jsonl").len(), 2);
    assert_eq!(lines(&fixture, "ends.jsonl").len(), 2);
}

#[tokio::test]
async fn reconnect_within_grace_keeps_the_same_pair() {
    let (mut fixture, url) = fixture().await;
    drop(connect(&fixture, &url).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let reconnected = connect(&fixture, &url).await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(lines(&fixture, "ends.jsonl").is_empty());
    assert_eq!(lines(&fixture, "starts.jsonl").len(), 1);
    drop(reconnected);
    wait_lines(&fixture, "ends.jsonl", 1).await;
    stop(&mut fixture).await;
}

#[tokio::test]
async fn repeated_client_reconnections_notify_without_reinjecting_context() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command(r#"input=$(cat); printf '%s\n' "$input" >> starts.jsonl; printf '%s' '{"hookSpecificOutput":{"additionalContext":"opening once"}}'"#),
        "SessionEnd":command(r#"input=$(cat); printf '%s\n' "$input" >> ends.jsonl"#)
    }})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    let mut client = connect(&fixture, &url).await;
    for count in 1..=3 {
        fixture.input(&url).await;
        let request = fixture.request().await;
        if count == 1 {
            let start = &wait_lines(&fixture, "starts.jsonl", 1).await[0];
            assert_eq!(start["source"], "startup");
            assert_eq!(start["accepts_additional_context"], true);
        }
        let occurrences = request.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("opening once"))
            })
            .count();
        assert_eq!(occurrences, 1);
        request.stop();
        fixture.idle(&url).await;
        if count < 3 {
            drop(client);
            wait_lines(&fixture, "ends.jsonl", count).await;
            client = connect(&fixture, &url).await;
            let start = &wait_lines(&fixture, "starts.jsonl", count + 1).await[count];
            assert_eq!(start["source"], "resume");
            // The reconnect only notifies, so hooks must not record an injection.
            assert_eq!(start["accepts_additional_context"], false);
        }
    }
    drop(client);
    fixture.shutdown().await;
}

#[tokio::test]
async fn client_exit_waits_for_the_running_turn() {
    let (mut fixture, url) = fixture().await;
    let client = connect(&fixture, &url).await;
    fixture.input(&url).await;
    let request = fixture.request().await;
    drop(client);
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert!(lines(&fixture, "ends.jsonl").is_empty());
    assert_eq!(
        fixture.snapshot(&url).await.session.status,
        SessionStatus::Working
    );
    request.stop();
    fixture.idle(&url).await;
    wait_lines(&fixture, "ends.jsonl", 1).await;
    stop(&mut fixture).await;
}

#[tokio::test]
async fn client_exit_waits_through_goal_continuation() {
    let (mut fixture, url) = fixture().await;
    let client = connect(&fixture, &url).await;
    fixture
        .client
        .post(format!("{url}/goal"))
        .json(&json!({"objective":"Finish both steps", "budgetUsd":0}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let first = fixture.request().await;
    drop(client);
    tokio::time::sleep(Duration::from_secs(11)).await;
    first.stop();
    let second = fixture.request().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(lines(&fixture, "ends.jsonl").is_empty());
    second.tool("update_goal", json!({"status":"complete"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    wait_lines(&fixture, "ends.jsonl", 1).await;
    stop(&mut fixture).await;
}

#[tokio::test]
async fn client_exit_waits_through_a_pending_background_wake() {
    let (mut fixture, url) = fixture().await;
    let client = connect(&fixture, &url).await;
    fixture.input(&url).await;
    fixture.request().await.tool(
        "shell",
        json!({"command":"while [ ! -f release ]; do sleep 0.02; done", "background":true}),
    );
    let next = fixture.request().await;
    drop(client);
    tokio::time::sleep(Duration::from_secs(11)).await;
    fs::write(fixture.root.path().join("release"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.snapshot(&url).await.session.background_commands[0].running {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    next.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        lines(&fixture, "ends.jsonl").is_empty(),
        "the two-second wake delay is still work"
    );
    fixture.request().await.stop();
    fixture.idle(&url).await;
    wait_lines(&fixture, "ends.jsonl", 1).await;
    stop(&mut fixture).await;
}

#[tokio::test]
async fn restricted_client_exit_never_runs_hooks() {
    let mut fixture = Fixture::new(json!({"hooks":{
        "SessionStart":command("touch forbidden"), "SessionEnd":command("touch forbidden")
    }}))
    .await;
    let url = fixture.create().await;
    drop(connect(&fixture, &url).await);
    tokio::time::sleep(Duration::from_secs(11)).await;
    let client = connect(&fixture, &url).await;
    stop(&mut fixture).await;
    drop(client);
    assert!(!fixture.root.path().join("forbidden").exists());
}

#[tokio::test]
async fn client_exit_hook_uses_its_own_timeout() {
    let mut fixture = Fixture::new(json!({"hooks":{"SessionEnd":command(
        "input=$(cat); printf '%s\\n' \"$input\" >> ends.jsonl; sleep 12; touch finished"
    )}}))
    .await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    drop(connect(&fixture, &url).await);
    wait_lines(&fixture, "ends.jsonl", 1).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !fixture.root.path().join("finished").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("client_exit must not use the shutdown budget");
    stop(&mut fixture).await;
    assert_eq!(lines(&fixture, "ends.jsonl").len(), 1);
}

#[tokio::test]
async fn client_exit_notifies_while_waiting_for_approval_without_resolving_it() {
    let (mut fixture, url) = fixture().await;
    let client = connect(&fixture, &url).await;
    fixture
        .set(&url, json!({"approvalPolicy":"alwaysAsk"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"pending.txt","content":"pending"}));
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .snapshot(&url)
            .await
            .session
            .pending_requests
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = fixture.snapshot(&url).await;
    drop(client);
    wait_lines(&fixture, "ends.jsonl", 1).await;
    assert_eq!(
        serde_json::to_value(fixture.snapshot(&url).await).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    stop(&mut fixture).await;
    assert_eq!(lines(&fixture, "ends.jsonl").len(), 1);
}

#[tokio::test]
async fn event_client_process() {
    let Some(socket) = std::env::var_os("CLIENT_SOCKET") else {
        return;
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(PathBuf::from(socket))
        .build()
        .unwrap();
    let mut response = client
        .get(format!(
            "http://localhost/v1/events?session={}",
            std::env::var("CLIENT_SESSION").unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fs::write(std::env::var_os("CLIENT_READY").unwrap(), "ready").unwrap();
    let exit = PathBuf::from(std::env::var_os("CLIENT_EXIT").unwrap());
    loop {
        tokio::select! {
            chunk = response.chunk() => if chunk.unwrap().is_none() { return; },
            _ = tokio::time::sleep(Duration::from_millis(20)) => if exit.exists() { return; },
        }
    }
}

#[tokio::test]
async fn client_process_normal_exit_and_sigkill_both_notify() {
    for killed in [false, true] {
        let (mut fixture, url) = fixture().await;
        let ready = fixture.root.path().join("client.ready");
        let exit = fixture.root.path().join("client.exit");
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "reload_tests::hooks::client_exit::event_client_process",
            ])
            .env_clear()
            .env("HOME", fixture.root.path().join("home"))
            .env("CANTER_HOME", fixture.root.path().join("state"))
            .env(
                "CLIENT_SOCKET",
                fixture.root.path().join("state/server.sock"),
            )
            .env("CLIENT_SESSION", url.rsplit('/').next().unwrap())
            .env("CLIENT_READY", &ready)
            .env("CLIENT_EXIT", &exit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        until(|| ready.exists()).await;
        if killed {
            child.start_kill().unwrap();
        } else {
            fs::write(exit, "exit").unwrap();
        }
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.success(), !killed);
        assert_eq!(
            wait_lines(&fixture, "ends.jsonl", 1).await[0]["reason"],
            "client_exit"
        );
        stop(&mut fixture).await;
    }
}

#[tokio::test]
async fn five_second_session_end_finishes_on_shutdown() {
    let mut fixture = Fixture::new(
        json!({"hooks":{"SessionEnd":command("cat > end.json; sleep 5; touch finished")}}),
    )
    .await;
    fixture.trust(fixture.root.path()).await;
    fixture.create().await;
    stop(&mut fixture).await;
    assert!(
        fixture.root.path().join("finished").exists(),
        "SessionEnd was killed before its five-second cleanup finished"
    );
}
