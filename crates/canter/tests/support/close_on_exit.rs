use super::*;

async fn wait_closed(world: &World, id: &str) {
    tokio::time::timeout(WAIT, async {
        while world.snapshot(id).await.status != SessionStatus::Closed {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("session was not closed");
}

#[tokio::test]
async fn managed_terminal_quit_closes_switched_sessions_and_background_commands() {
    let mut world = World::new(vec![]).await;
    world.start_server().await;
    let terminal = world.open(&["--close-on-exit", "--yolo"], "managed").await;
    terminal.wait_idle().await;
    let first = world.only_session().await;
    terminal
        .type_line("!echo $$ > managed.pid; printf managed-ready; exec /bin/sleep 60 &")
        .await;
    terminal.wait_for("managed-ready").await;
    terminal.wait_for("Background #1 · running").await;
    assert!(
        world
            .snapshot(&first)
            .await
            .background_commands
            .iter()
            .any(|command| command.running)
    );
    terminal.type_line("/new").await;
    terminal.wait_idle().await;
    let second = tokio::time::timeout(Duration::from_secs(5), terminal.quit())
        .await
        .expect("exit within five seconds");
    assert_ne!(first, second);
    wait_closed(&world, &first).await;
    wait_closed(&world, &second).await;
    assert!(
        world
            .snapshot(&first)
            .await
            .background_commands
            .iter()
            .all(|command| !command.running)
    );
    let pid = Pid::from_raw(
        fs::read_to_string(world.path("work/managed.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    assert_eq!(
        kill(pid, None),
        Err(Errno::ESRCH),
        "the actual background process must stop"
    );
    world.stop_server(Signal::SIGTERM).await;
}

fn published(path: &Path) -> String {
    serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn managed_terminal_claims_resume_fork_handoff_and_return() {
    let mut world = World::new(vec![
        text_reply("Original answer"),
        text_reply("Continue the task"),
    ])
    .await;
    world.start_server().await;
    let original = world.open(&["Original task"], "ordinary").await;
    original.wait_for("Original answer").await;
    original.wait_idle().await;
    let id = original.quit().await;
    assert_eq!(world.snapshot(&id).await.status, SessionStatus::Idle);
    let path = world.path("managed.json");
    let terminal = world
        .open(
            &[
                "--close-on-exit",
                "--session-file",
                path.to_str().unwrap(),
                "--resume",
                &id,
            ],
            "managed",
        )
        .await;
    terminal.wait_for("Original answer").await;
    terminal.type_line("/fork").await;
    terminal.wait_for("Session forked.").await;
    let fork = published(&path);
    terminal.type_line("/handoff").await;
    terminal
        .wait_for("Edit the note and press Enter to send.")
        .await;
    let handoff = published(&path);
    terminal.press(KeyCode::Esc).await;
    terminal
        .wait_for("Esc again to return to the original session")
        .await;
    terminal.press(KeyCode::Esc).await;
    terminal
        .wait_until("returned to fork", |_| published(&path) == fork)
        .await;
    terminal.type_line("/resume").await;
    terminal.wait_for("Which session to resume?").await;
    terminal.press(KeyCode::Down).await;
    terminal.press(KeyCode::Enter).await;
    terminal
        .wait_until("picked a session", |terminal| {
            !terminal.screen().contains("Which session to resume?")
        })
        .await;
    terminal.quit().await;
    for session in [&id, &fork, &handoff] {
        wait_closed(&world, session).await;
    }
    assert_ne!(fork, id);
    assert_ne!(handoff, fork);
    world.stop_server(Signal::SIGTERM).await;
}

// Runs the real CLI preparation and terminal event loop in a separate process,
// with TestBackend in place of a physical terminal so signals are isolated.
#[tokio::test]
async fn managed_terminal_process() {
    let Some(root) = std::env::var_os("CANTER_MANAGED_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let managed = std::env::var_os("CANTER_MANAGED_TEST_ENABLED").is_some();
    let binding = root.join("binding.json");
    let mut args = vec!["--yolo", "--session-file", binding.to_str().unwrap()];
    if managed {
        args.push("--close-on-exit");
    }
    let terminal = Terminal::start(
        &args,
        Context {
            state_dir: root.join("state"),
            executable: env!("CARGO_BIN_EXE_canter").into(),
            cwd: root.join("work"),
            env: BTreeMap::from([("HOME".into(), root.join("home").display().to_string())]),
            client_name: "signal-test".into(),
        },
    )
    .await;
    terminal.wait_idle().await;
    let first = published(&binding);
    terminal
        .type_line("!printf process-ready; exec /bin/sleep 60 &")
        .await;
    terminal.wait_for("Background #1 · running").await;
    terminal.type_line("/new").await;
    terminal
        .wait_until("second session", |_| published(&binding) != first)
        .await;
    fs::write(
        root.join("ready.json"),
        serde_json::to_vec(&json!([first, published(&binding)])).unwrap(),
    )
    .unwrap();
    let _ = terminal.task.await.unwrap().unwrap();
}

async fn signal_process(signal: Signal, managed: bool) {
    let mut world = World::new(vec![]).await;
    // Slow close demonstrates that the terminal does not wait for SessionEnd.
    let config = world.path("state/config.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    value["hooks"] = json!({"SessionEnd":[{"hooks":[{"type":"command","command":"cat >/dev/null; sleep 6", "timeout":10}]}]});
    fs::write(config, value.to_string()).unwrap();
    world.start_server().await;
    let log = fs::File::create(world.path("terminal.log")).unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "close_on_exit::managed_terminal_process"])
        .env_clear()
        .env("HOME", world.path("home"))
        .env("CANTER_HOME", world.path("state"))
        .env("CANTER_MANAGED_TEST_ROOT", world.root.path())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true);
    if managed {
        command.env("CANTER_MANAGED_TEST_ENABLED", "1");
    }
    let mut child = command.spawn().unwrap();
    tokio::time::timeout(WAIT, async {
        while !world.path("ready.json").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(world.path("terminal.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let ids: Vec<String> =
        serde_json::from_slice(&fs::read(world.path("ready.json")).unwrap()).unwrap();
    let ordinary = world.open(&["--resume", &ids[0]], "ordinary watcher").await;
    ordinary.wait_for("Background #1 · running").await;
    let observer = reqwest::Client::builder()
        .unix_socket(world.path("state/server.sock"))
        .build()
        .unwrap()
        .get(format!(
            "http://localhost/v1/events?session={}&observe=true",
            ids[0]
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    kill(Pid::from_raw(child.id().unwrap() as i32), signal).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("terminal exits within five seconds")
        .unwrap();
    assert_eq!(status.success(), signal != Signal::SIGKILL);
    if managed {
        if signal == Signal::SIGKILL {
            tokio::time::sleep(Duration::from_secs(9)).await;
            assert!(
                world
                    .snapshot(&ids[0])
                    .await
                    .background_commands
                    .iter()
                    .any(|command| command.running)
            );
        }
        for id in &ids {
            wait_closed(&world, id).await;
        }
        assert!(
            world
                .snapshot(&ids[0])
                .await
                .background_commands
                .iter()
                .all(|command| !command.running)
        );
        ordinary.wait_for("Session closed").await;
    } else {
        tokio::time::sleep(Duration::from_secs(11)).await;
        for id in &ids {
            assert_ne!(world.snapshot(id).await.status, SessionStatus::Closed);
        }
        assert!(
            world
                .snapshot(&ids[0])
                .await
                .background_commands
                .iter()
                .any(|command| command.running)
        );
    }
    drop(observer);
    ordinary.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

#[tokio::test]
async fn managed_term_exits_within_five_seconds_while_server_finishes_cleanup() {
    signal_process(Signal::SIGTERM, true).await;
}

#[tokio::test]
async fn managed_hup_exits_within_five_seconds_while_server_finishes_cleanup() {
    signal_process(Signal::SIGHUP, true).await;
}

#[tokio::test]
async fn managed_sigkill_closes_after_grace_despite_other_subscribers() {
    signal_process(Signal::SIGKILL, true).await;
}

#[tokio::test]
async fn ordinary_term_keeps_sessions_and_background_work_running() {
    signal_process(Signal::SIGTERM, false).await;
}

#[tokio::test]
async fn managed_terminal_reconnects_after_replace_and_still_closes_on_exit() {
    let mut world = World::new(vec![
        text_reply("Before replacement"),
        text_reply("After replacement"),
    ])
    .await;
    world.start_server().await;
    let path = world.path("binding.json");
    let terminal = world
        .open(
            &[
                "--close-on-exit",
                "--session-file",
                path.to_str().unwrap(),
                "Hello",
            ],
            "managed upgrade",
        )
        .await;
    terminal.wait_for("Before replacement").await;
    terminal.wait_idle().await;
    let id = published(&path);
    fs::remove_file(&path).unwrap();
    let output = tokio::time::timeout(
        WAIT,
        tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"))
            .args(["serve", "--replace"])
            .env_clear()
            .env("HOME", world.path("home"))
            .env("CANTER_HOME", world.path("state"))
            .current_dir(world.path("work"))
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    world.server.take().unwrap().wait().await.unwrap();
    let registration: Value =
        serde_json::from_slice(&fs::read(world.path("state/server.json")).unwrap()).unwrap();
    struct Replacement(Pid);
    impl Drop for Replacement {
        fn drop(&mut self) {
            let _ = kill(self.0, Signal::SIGKILL);
        }
    }
    let replacement = Replacement(Pid::from_raw(registration["pid"].as_i64().unwrap() as i32));
    terminal
        .wait_until("reconnected binding", |_| path.exists())
        .await;
    assert_eq!(published(&path), id);
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!(world.snapshot(&id).await.status, SessionStatus::Idle);
    terminal.type_line("Continue").await;
    terminal.wait_for("After replacement").await;
    terminal.wait_idle().await;
    assert_eq!(terminal.quit().await, id);
    wait_closed(&world, &id).await;
    kill(replacement.0, Signal::SIGTERM).unwrap();
    tokio::time::timeout(WAIT, async {
        while kill(replacement.0, None) != Err(Errno::ESRCH) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    std::mem::forget(replacement);
}
