use canter_protocol::SessionSnapshot;
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

#[tokio::test]
async fn remote_workspace_data_drives_the_branch_counts_and_file_completion() {
    use axum::{Json, Router, response::Sse, routing::get};
    use serde_json::json;
    // Wire responses keep this rendering test independent of the client's
    // filesystem. Real collectors and session ignores run in canter's remote tests.
    let router = Router::new()
        .route("/v1/info", get(|| async {
            Json(json!({"serverVersion":"test","protocolVersion":1,"sessions":[],"features":["workspaceQueries"]}))
        }))
        .route("/v1/sessions/test/workspace/summary", get(|| async {
            Json(json!({"branch":"server-branch","changedLines":[2,0]}))
        }))
        .route("/v1/sessions/test/workspace/entries", get(|| async {
            Sse::new(futures_util::stream::iter([Ok::<_, std::convert::Infallible>(
                axum::response::sse::Event::default().json_data(json!({
                    "entries":[{"label":"server-file.txt","dir":false}],"truncated":false,"done":true
                })).unwrap()
            )]))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = canter_tui::client::Client::remote(
        "fixture",
        &format!("http://{}", listener.local_addr().unwrap()),
        "test-only",
    )
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut snapshot = SessionSnapshot::empty("test");
    snapshot.settings.cwd = "/server/path-unavailable-on-client".into();
    let mut app = App::new_remote(snapshot, None, 0, "/client".into(), None).with_workspace(client);
    app.load_workspace().await;
    let mut ui = Ui::new(TestBackend::new(100, 28), app).unwrap();
    ui.app_mut().handle_paste("@server-");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let shown = text(&mut ui);
            if shown.contains("server-branch")
                && shown.contains("server-file.txt")
                && shown.contains("+2")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 0);
    assert!(text(&mut ui).contains("@server-file.txt"));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn old_servers_prompt_for_replacement_without_local_workspace_fallback() {
    use axum::{Json, Router, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/v1/info",
        get(|| async {
            Json(serde_json::json!({"serverVersion":"old","protocolVersion":1,"sessions":[]}))
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client =
        canter_tui::client::Client::remote("old", &format!("http://{address}"), "test-only")
            .unwrap();
    let error = client.workspace_diff("test").await.unwrap_err().to_string();
    assert!(
        error.contains("Server 'old'") && error.contains("upgrade it on that machine"),
        "{error}"
    );
    // Local clients retain the existing explicit serve --replace wording.
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let router = Router::new().route(
        "/v1/info",
        get(|| async {
            Json(serde_json::json!({"serverVersion":"old","protocolVersion":1,"sessions":[]}))
        }),
    );
    let local_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let local = canter_tui::client::Client::new(&socket).unwrap();
    let error = local
        .workspace_status("test")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("serve --replace"), "{error}");
    task.abort();
    local_task.abort();
    let _ = task.await;
    let _ = local_task.await;
}

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
fn remote_workspace_commands_use_the_same_actions_as_local_commands() {
    for (command, action) in [
        ("/diff", canter_tui::Action::Diff),
        ("/review", canter_tui::Action::Review(String::new())),
        ("/files", canter_tui::Action::Files),
    ] {
        let mut ui = screen();
        assert_eq!(enter(&mut ui, command), Some(action), "{command}");
        assert!(
            !text(&mut ui).contains("Not available on remote servers yet"),
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
fn remote_clipboard_images_read_client_bytes_and_mentions_are_not_blocked() {
    let mut ui = screen();
    let mut snapshot = SessionSnapshot::empty("test");
    snapshot.settings.model = "deepseek-flash".into();
    *ui.app_mut() = App::new_remote(snapshot, None, 0, "/client".into(), None)
        .with_clipboard_reader(|path| {
            std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
            Ok(true)
        });
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
    assert!(text(&mut ui).contains("[Image #1 screenshot.png]"));
    assert!(matches!(
        enter(&mut ui, ""),
        Some(canter_tui::Action::SendImages { .. })
    ));
    let mut ui = screen();
    ui.app_mut().handle_paste("@src/");
    assert!(!text(&mut ui).contains("Not available on remote servers yet"));
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
fn remote_image_drop_uses_client_directory_and_path_text_still_sends() {
    let client = tempfile::tempdir().unwrap();
    let image = client.path().join("a picture.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    for value in [format!("'{}'", image.display()), "a picture.png".into()] {
        let mut snapshot = SessionSnapshot::empty("test");
        snapshot.settings.model = "deepseek-flash".into();
        snapshot.settings.cwd = "/server/project".into();
        let mut ui = Ui::new(
            TestBackend::new(100, 28),
            App::new_remote(
                snapshot,
                None,
                0,
                client.path().to_str().unwrap().into(),
                None,
            ),
        )
        .unwrap();
        ui.app_mut().handle_paste(&value);
        assert!(text(&mut ui).contains("[Image #1 a picture.png]"));
        assert!(matches!(
            ui.app_mut()
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            Some(canter_tui::Action::SendImages { .. })
        ));
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
    assert!(!text(&mut local).contains("client-only-branch"));
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
