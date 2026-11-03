use super::*;

#[tokio::test]
async fn yolo_reconnect_restores_the_terminals_sandbox_choice() {
    let mut world = World::new(vec![
        shell_reply("printf before > ../before-restart"),
        text_reply("Before restart."),
        shell_reply("printf after > ../after-restart"),
        text_reply("After restart."),
    ])
    .await;
    world.start_server().await;
    let terminal = world
        .open(&["--yolo", "Write before restart"], "reconnect-yolo")
        .await;
    terminal.wait_for("Before restart.").await;
    terminal.wait_idle().await;
    assert_eq!(
        fs::read_to_string(world.path("before-restart")).unwrap(),
        "before"
    );
    let id = world.only_session().await;
    world.stop_server(Signal::SIGTERM).await;
    terminal
        .wait_until("Reconnecting", |a| a.status().contains("Reconnecting"))
        .await;
    world.start_server().await;
    terminal
        .wait_until("reconnected", |a| !a.status().contains("Reconnecting"))
        .await;
    assert_eq!(
        world.snapshot(&id).await.settings.sandbox_enabled,
        Some(false)
    );
    terminal.wait_for("Sandbox off").await;
    terminal.type_line("Write after restart").await;
    terminal.wait_for("After restart.").await;
    terminal.wait_idle().await;
    assert_eq!(
        fs::read_to_string(world.path("after-restart")).unwrap(),
        "after"
    );
    terminal.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

#[tokio::test]
async fn yolo_slash_resume_and_fork_use_the_terminal_choice() {
    let mut world = World::new(Vec::new()).await;
    world.start_server().await;
    let ordinary = world.open(&[], "ordinary").await;
    ordinary.wait_idle().await;
    let original = ordinary.quit().await;
    let yolo = world.open(&["--yolo"], "yolo").await;
    yolo.wait_idle().await;
    let client = Client::new(&world.path("state/server.sock")).unwrap();
    // Give the target a stable picker label without making a model request.
    client
        .update(
            &original,
            &serde_json::from_value(json!({"name":"Original sandboxed session"})).unwrap(),
        )
        .await
        .unwrap();
    yolo.type_line("/resume").await;
    yolo.wait_for("Which session to resume?").await;
    yolo.press(KeyCode::Enter).await;
    yolo.wait_until("resumed", |a| {
        !a.screen().contains("Which session to resume?")
    })
    .await;
    assert_eq!(
        world.snapshot(&original).await.settings.sandbox_enabled,
        Some(false)
    );
    yolo.wait_for("Sandbox off").await;
    yolo.type_line("/fork").await;
    yolo.wait_for("Session forked.").await;
    yolo.wait_for("Sandbox off").await;
    let forked = yolo.quit().await;
    assert_ne!(forked, original);
    assert_eq!(
        world.snapshot(&forked).await.settings.sandbox_enabled,
        Some(false)
    );
    // Forking from a yolo source in an ordinary terminal follows configuration.
    let ordinary = world.open(&["-r", &original], "ordinary-fork").await;
    ordinary.wait_for("Sandbox on").await;
    ordinary.type_line("/fork").await;
    ordinary.wait_for("Session forked.").await;
    ordinary.wait_for("Sandbox on").await;
    let forked = ordinary.quit().await;
    assert_eq!(
        world.snapshot(&forked).await.settings.sandbox_enabled,
        Some(true)
    );
    world.stop_server(Signal::SIGTERM).await;
}

#[tokio::test]
async fn yolo_resume_changes_only_that_session_and_updates_both_terminals() {
    let mut world = World::new(Vec::new()).await;
    world.start_server().await;
    let yolo = world.open(&["--yolo"], "yolo").await;
    yolo.wait_for("Sandbox off").await;
    let id = world.only_session().await;
    let ordinary = world.open(&[], "ordinary").await;
    ordinary.wait_for("Sandbox on").await;
    assert_eq!(
        world.snapshot(&id).await.settings.sandbox_enabled,
        Some(false)
    );
    let other = ordinary.quit().await;

    let resumed = world.open(&["-r", &id], "resume-ordinary").await;
    resumed.wait_idle().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(snapshot.settings.sandbox_enabled, Some(true));
    assert_eq!(snapshot.settings.approval_policy, PermissionMode::AllowAll);
    yolo.wait_for("Sandbox on").await;
    resumed.wait_for("Sandbox on").await;

    let latest = world.open(&["--yolo", "-r", &id], "resume-yolo").await;
    latest.wait_for("Sandbox off").await;
    yolo.wait_for("Sandbox off").await;
    resumed.wait_for("Sandbox off").await;
    assert_eq!(
        world.snapshot(&other).await.settings.sandbox_enabled,
        Some(true)
    );
    latest.quit().await;
    resumed.quit().await;
    yolo.quit().await;
    world.stop_server(Signal::SIGTERM).await;

    // The saved Allow all permission survives; the opening's exemption does not.
    world.start_server().await;
    let resumed = world.open(&["-r", &id], "from-disk").await;
    resumed.wait_for("Sandbox on").await;
    assert_eq!(
        world.snapshot(&id).await.settings.approval_policy,
        PermissionMode::AllowAll
    );
    resumed.quit().await;
    let continued = world.open(&["--yolo", "-c"], "continue-yolo").await;
    continued.wait_for("Sandbox off").await;
    assert_eq!(continued.quit().await, id);
    let continued = world.open(&["-c"], "continue-ordinary").await;
    continued.wait_for("Sandbox on").await;
    assert_eq!(continued.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}
