use canter_protocol::SessionSnapshot;
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

fn screen() -> Ui<TestBackend> {
    Ui::new(
        TestBackend::new(100, 28),
        App::new_remote(
            SessionSnapshot::empty("test"),
            Some("/server/home".into()),
            0,
            "/client".into(),
            Some("/client/home".into()),
        ),
    )
    .unwrap()
}
fn text(ui: &mut Ui<TestBackend>) -> String {
    ui.draw(0).unwrap();
    ui.backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn enter(ui: &mut Ui<TestBackend>, command: &str) -> Option<canter_tui::Action> {
    ui.app_mut().handle_paste(command);
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0)
}

#[test]
fn remote_workspace_commands_explain_the_limit_without_actions() {
    for command in ["/diff", "/review", "/files"] {
        let mut ui = screen();
        assert_eq!(enter(&mut ui, command), None, "{command}");
        assert!(
            text(&mut ui).contains("Not available on remote servers yet"),
            "{command}"
        );
    }
}

#[test]
fn remote_memory_lists_and_deletes_but_does_not_open_an_editor() {
    use canter_protocol::{MemoryDeleteParams, MemoryEntry, MemoryListResponse, MemoryScope};
    use canter_tui::Action;
    let mut ui = screen();
    assert_eq!(enter(&mut ui, "/memory"), Some(Action::Memory));
    let list = || MemoryListResponse {
        data: vec![MemoryEntry {
            scope: MemoryScope::Project,
            file: "convention.md".into(),
            name: "Project convention".into(),
            description: "Keep the existing style.".into(),
            path: "/server/project/memory/convention.md".into(),
        }],
    };
    ui.app_mut().show_memories(Ok(list()));
    assert!(text(&mut ui).contains("Keep the existing style."));
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        None
    );
    assert!(text(&mut ui).contains("Edit memory on the server machine"));
    ui.app_mut().show_memories(Ok(list()));
    text(&mut ui);
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE), 0),
        None
    );
    assert!(text(&mut ui).contains("Delete memory"));
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), 0),
        Some(Action::DeleteMemory(MemoryDeleteParams {
            scope: MemoryScope::Project,
            file: "convention.md".into()
        }))
    );
}

#[test]
fn remote_clipboard_images_and_file_mentions_do_not_read_client_files() {
    let mut ui = screen();
    *ui.app_mut() = App::new_remote(
        SessionSnapshot::empty("test"),
        None,
        0,
        "/client".into(),
        None,
    )
    .with_clipboard_reader(|_| panic!("remote clipboard images are unavailable"));
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
    assert!(text(&mut ui).contains("Not available on remote servers yet"));
    let mut ui = screen();
    ui.app_mut().handle_paste("@src/");
    assert!(text(&mut ui).contains("Not available on remote servers yet"));
}

#[test]
fn remote_exports_resolve_relative_paths_and_tilde_on_the_client() {
    let client = tempfile::tempdir().unwrap();
    let home = client.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let mut snapshot = SessionSnapshot::empty("test");
    snapshot.settings.cwd = "/server/project".into();
    let turn = serde_json::from_value(serde_json::json!({"id":"t", "status":"completed", "items":[{"type":"agentMessage","id":"a","text":"Remote answer"}],"input":[{"type":"text","text":"Hello"}],"clientName":"test","usage":null,"error":null})).unwrap();
    snapshot.turns.push(turn);
    let mut app = App::new_remote(
        snapshot,
        Some("/server/home".into()),
        0,
        client.path().to_str().unwrap().into(),
        Some(home.to_str().unwrap().into()),
    );
    for name in ["answer.md", "~/answer.md"] {
        app.export(canter_tui::Export::File(Some(name.into())), 0);
    }
    for path in [client.path().join("answer.md"), home.join("answer.md")] {
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("Remote answer")
        );
    }
}

#[test]
fn remote_image_drop_is_a_notice_and_path_text_still_sends() {
    let client = tempfile::tempdir().unwrap();
    let image = client.path().join("a picture.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    for value in [format!("'{}'", image.display()), "a picture.png".into()] {
        let mut ui = Ui::new(
            TestBackend::new(100, 28),
            App::new_remote(
                SessionSnapshot::empty("test"),
                None,
                0,
                client.path().to_str().unwrap().into(),
                None,
            ),
        )
        .unwrap();
        ui.app_mut().handle_paste(&value);
        assert!(text(&mut ui).contains("Not available on remote servers yet"));
        assert_eq!(
            ui.app_mut()
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            None
        );
    }
    for value in [
        "/srv/x.rs",
        "./x",
        "~/x",
        "/srv/p/src/lib.rs:10:5: error[E0308]: mismatched types",
        "Hello remote server",
    ] {
        let mut ui = screen();
        assert_eq!(
            enter(&mut ui, value),
            Some(canter_tui::Action::Send(value.into()))
        );
        assert!(!text(&mut ui).contains("Not available on remote servers yet"));
    }
}

#[test]
fn remote_addresses_require_tls_or_literal_loopback_and_credentials_stay_out_of_debug() {
    use canter_tui::client::{Client, remote_url};
    for address in [
        "https://example.com",
        "http://127.0.0.1:1234",
        "http://[::1]:1234",
    ] {
        assert!(remote_url(address).is_ok(), "{address}");
    }
    for address in [
        "http://203.0.113.10",
        "http://example.com",
        "file:///tmp/socket",
        "https://user:secret@example.com",
        "https://example.com?token=secret",
        "https://example.com#secret",
    ] {
        let error = remote_url(address).unwrap_err().to_string();
        assert!(error.contains("HTTPS"));
        assert!(!error.contains("secret"));
    }
    let client = Client::remote("test", "https://example.com", "private-test-token").unwrap();
    assert!(!format!("{client:?}").contains("private-test-token"));
}

#[tokio::test]
async fn remote_openings_omit_env_use_prefix_and_never_follow_redirects() {
    use canter_tui::client::Client;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    let records = Arc::new(Mutex::new(Vec::new()));
    let received = records.clone();
    let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let received = received.clone();
        async move {
            let (parts, body) = request.into_parts();
            assert_eq!(parts.headers["authorization"], "Bearer test-only-token");
            let bytes = axum::body::to_bytes(body, 4096).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            received
                .lock()
                .unwrap()
                .push((parts.uri.path().to_owned(), value));
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", "/must-not-follow")],
            )
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/prefix", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = Client::remote("test", &url, "test-only-token").unwrap();
    let env = [("HOME".to_owned(), "/client/home".to_owned())].into();
    let params = serde_json::from_value(json!({"cwd":"/server/project","env":env,"model":"fake","approvalPolicy":"alwaysAsk","clientName":"test"})).unwrap();
    assert!(client.create_session(&params).await.is_err());
    assert!(client.resume("session", &env).await.is_err());
    assert!(
        client
            .fork_session(
                "session",
                &canter_protocol::SessionResumeParams {
                    env: Some(env),
                    sandbox_disabled: false,
                    restricted_mode: false
                }
            )
            .await
            .is_err()
    );
    let captured = records.lock().unwrap().clone();
    assert_eq!(captured.len(), 3);
    assert_eq!(
        captured.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        [
            "/prefix/v1/sessions",
            "/prefix/v1/sessions/session/resume",
            "/prefix/v1/sessions/session/fork"
        ]
    );
    for (_, value) in captured {
        assert!(value.get("env").is_none(), "{value}");
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn remote_sse_times_out_after_three_missing_heartbeats() {
    use canter_tui::client::Client;
    use std::time::Duration;
    let router = axum::Router::new().route(
        "/v1/events",
        axum::routing::get(|| async {
            let stream =
                futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>();
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(stream),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = Client::remote("test", &url, "test-only-token").unwrap();
    let mut stream = client.subscribe("s", 0).await.unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(50), stream.next())
        .await
        .expect("three missing heartbeats must disconnect");
    assert!(result.unwrap().is_err());
    assert!(started.elapsed() >= Duration::from_secs(44));
    task.abort();
    let _ = task.await;
}

#[test]
fn remote_screen_does_not_display_a_matching_client_repository_branch() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(
        root.path().join(".git/HEAD"),
        "ref: refs/heads/client-only-branch\n",
    )
    .unwrap();
    let mut snapshot = SessionSnapshot::empty("test");
    snapshot.settings.cwd = root.path().to_str().unwrap().into();
    let mut local = Ui::new(
        TestBackend::new(100, 28),
        App::new(snapshot.clone(), None, 0),
    )
    .unwrap();
    assert!(text(&mut local).contains("client-only-branch"));
    let mut remote = Ui::new(
        TestBackend::new(100, 28),
        App::new_remote(snapshot, None, 0, "/client".into(), None),
    )
    .unwrap();
    assert!(!text(&mut remote).contains("client-only-branch"));
}

#[test]
fn remote_pasting_commands_with_paths_and_multiline_source_preserves_text() {
    let mut ui = screen();
    assert_eq!(
        enter(&mut ui, "/export answer.md"),
        Some(canter_tui::Action::Export(canter_tui::Export::File(Some(
            "answer.md".into()
        ))))
    );
    let mut ui = screen();
    let code = "// source comment\nfn main() {}";
    assert_eq!(
        enter(&mut ui, code),
        Some(canter_tui::Action::Send(code.into()))
    );
}

#[tokio::test]
async fn remote_statusline_runs_outside_client_repositories_with_server_context() {
    let client = tempfile::tempdir().unwrap();
    std::fs::create_dir(client.path().join(".git")).unwrap();
    std::fs::write(
        client.path().join(".git/HEAD"),
        "ref: refs/heads/client-only\n",
    )
    .unwrap();
    let mut snapshot = SessionSnapshot::empty("test");
    let server_cwd = client
        .path()
        .join("absent-server-project")
        .to_str()
        .unwrap()
        .to_owned();
    snapshot.settings.cwd = server_cwd.clone();
    let home = client.path().to_str().unwrap().to_owned();
    let app = App::new_remote(snapshot, None, 0, home.clone(), Some(home.clone()))
        .with_status_line(
            Some("printf '%s\\n' \"$PWD\" > \"$HOME/pwd\"; /bin/cat > \"$HOME/input.json\"; printf 'remote status ok\\n'".into()),
            [("HOME".into(), home.clone()), ("CANTER_HOME".into(), home)].into(),
        );
    let mut ui = Ui::new(TestBackend::new(100, 28), app).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let shown = text(&mut ui);
            assert!(!shown.contains("statusline error"), "{shown}");
            if shown.contains("remote status ok") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("remote status script should finish");
    assert_eq!(
        std::fs::read_to_string(client.path().join("pwd"))
            .unwrap()
            .trim(),
        "/"
    );
    let input: serde_json::Value =
        serde_json::from_slice(&std::fs::read(client.path().join("input.json")).unwrap()).unwrap();
    assert_eq!(input["cwd"], server_cwd);
    assert!(input["gitBranch"].is_null());
}
