use super::*;

fn binding(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("session file published")).unwrap()
}

fn assert_binding(world: &World, path: &Path, id: &str, cwd: &Path) {
    assert_eq!(
        binding(path),
        json!({"sessionId":id,"cwd":cwd,"stateDir":world.path("state")})
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn session_file_tracks_new_resume_fork_handoff_and_reconnect() {
    let mut world = World::new(vec![
        text_reply("Original answer"),
        text_reply("Continue the task"),
    ])
    .await;
    world.start_server().await;
    let path = world.path("binding.json");
    let option = path.to_str().unwrap();
    let terminal = world
        .open(&["--session-file", option, "Original task"], "binding")
        .await;
    terminal.wait_for("Original answer").await;
    terminal.wait_idle().await;
    let original = world.only_session().await;
    assert_binding(&world, &path, &original, &world.path("work"));
    // A reader holding the previous file keeps a complete, unchanged version.
    let mut old_file = fs::File::open(&path).unwrap();
    terminal.type_line("/new").await;
    terminal
        .wait_until("new session binding", |_| {
            binding(&path)["sessionId"] != original
        })
        .await;
    let new = binding(&path)["sessionId"].as_str().unwrap().to_owned();
    assert_binding(&world, &path, &new, &world.path("work"));
    let old: Value = serde_json::from_reader(&mut old_file).unwrap();
    assert_eq!(old["sessionId"], original);
    terminal.type_line("/resume").await;
    terminal.wait_for("Which session to resume?").await;
    terminal.press(KeyCode::Down).await;
    terminal.press(KeyCode::Enter).await;
    terminal.wait_for("Original answer").await;
    assert_binding(&world, &path, &original, &world.path("work"));
    terminal.type_line("/fork").await;
    terminal.wait_for("Session forked.").await;
    let fork = binding(&path)["sessionId"].as_str().unwrap().to_owned();
    assert_ne!(fork, original);
    assert_ne!(fork, new);
    assert_binding(&world, &path, &fork, &world.path("work"));
    terminal.type_line("/handoff").await;
    terminal
        .wait_for("Edit the note and press Enter to send.")
        .await;
    let handoff = binding(&path)["sessionId"].as_str().unwrap().to_owned();
    assert_ne!(handoff, fork);
    assert_binding(&world, &path, &handoff, &world.path("work"));
    terminal.press(KeyCode::Esc).await;
    terminal
        .wait_for("Esc again to return to the original session")
        .await;
    terminal.press(KeyCode::Esc).await;
    terminal
        .wait_until("returned session binding", |_| {
            binding(&path)["sessionId"] == fork
        })
        .await;
    assert_binding(&world, &path, &fork, &world.path("work"));
    world.stop_server(Signal::SIGTERM).await;
    terminal.wait_for("Reconnecting").await;
    fs::remove_file(&path).unwrap();
    world.start_server().await;
    terminal
        .wait_until("reconnected session binding", |_| path.exists())
        .await;
    assert_binding(&world, &path, &fork, &world.path("work"));
    assert_eq!(terminal.quit().await, fork);
    assert_binding(&world, &path, &fork, &world.path("work"));
    for args in [vec!["--resume", &original], vec!["-c"]] {
        let terminal = world
            .open(
                &[&["--session-file", option][..], &args].concat(),
                "resumed binding",
            )
            .await;
        terminal.wait_idle().await;
        let id = terminal.quit().await;
        assert_binding(&world, &path, &id, &world.path("work"));
    }
    world.stop_server(Signal::SIGTERM).await;
}

#[tokio::test]
async fn session_file_startup_failure_exits_before_opening_terminal() {
    let mut world = World::new(vec![]).await;
    world.start_server().await;
    let locked = world.path("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    for path in [
        world.path("missing/binding.json"),
        locked.join("binding.json"),
        world.path("work"),
        world.path(&"x".repeat(256)),
    ] {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"))
            .arg("--session-file")
            .arg(&path)
            .env_clear()
            .env("HOME", world.path("home"))
            .env("CANTER_HOME", world.path("state"))
            .current_dir(world.path("work"))
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("Could not write session file"), "{error}");
        assert!(
            !output
                .stdout
                .windows(8)
                .any(|bytes| bytes == b"\x1b[?1049h")
        );
        assert!(!path.is_file());
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    world.stop_server(Signal::SIGTERM).await;
}

#[tokio::test]
async fn session_file_waits_for_trust_and_retries_failed_switches() {
    let mut world = World::new(vec![]).await;
    world.start_server().await;
    let directory = world.path("binding");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("current.json");
    let terminal = Terminal::start_with_trust(
        &["--session-file", path.to_str().unwrap()],
        world.context("binding", &world.path("work")),
        false,
    )
    .await;
    terminal.wait_for("Trust this folder?").await;
    assert!(!path.exists(), "no file until the session exists");
    read_the_box().await;
    terminal.press(KeyCode::Enter).await;
    terminal.wait_idle().await;
    let original = world.only_session().await;
    assert_binding(&world, &path, &original, &world.path("work"));
    fs::rename(&directory, world.path("saved-binding")).unwrap();
    terminal.type_line("/new").await;
    terminal.wait_for("Could not write session file").await;
    assert!(
        !terminal.task.is_finished(),
        "later write errors keep the terminal running"
    );
    fs::rename(world.path("saved-binding"), &directory).unwrap();
    terminal.type_line("/new").await;
    terminal
        .wait_until("retried session binding", |_| {
            binding(&path)["sessionId"] != original
        })
        .await;
    let id = terminal.quit().await;
    assert_binding(&world, &path, &id, &world.path("work"));
    assert_eq!(
        fs::read_dir(&directory).unwrap().count(),
        1,
        "temporary files are cleaned up"
    );
    // A relative destination is relative to the terminal's starting directory,
    // while cwd in the record comes from the resumed session.
    let resumed = Terminal::start(
        &["--session-file", "resumed.json", "--resume", &id],
        world.context("other directory", &world.path("home")),
    )
    .await;
    resumed.wait_idle().await;
    assert_eq!(resumed.quit().await, id);
    assert_binding(
        &world,
        &world.path("home/resumed.json"),
        &id,
        &world.path("work"),
    );
    world.stop_server(Signal::SIGTERM).await;
}
