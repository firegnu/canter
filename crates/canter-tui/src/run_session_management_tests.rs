use super::retract_tests::Harness;
use super::*;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

#[tokio::test]
async fn shared_resume_picker_deletes_or_displays_the_server_reason() {
    use axum::response::IntoResponse;
    for outcome in ["deleted", "attached", "old server"] {
        let mut test = Harness::new().await;
        let client = test.chat.client.clone();
        let (keys, mut input) = input();
        let mut terminal = Terminal::new(TestBackend::new(160, 20)).unwrap();
        let mut shutdown = Shutdown::listen().unwrap();
        let mut tab = TabTitle::new(&BTreeMap::new());
        let picker = Picker::new(
            vec![canter_protocol::SessionSummary {
                id: "test".into(),
                cwd: "/work".into(),
                name: Some("Test session".into()),
                status: SessionStatus::Idle,
                updated_at: 0,
                last_event_seq: 1,
            }],
            "/work".into(),
            None,
        );
        let drive = async {
            while keys.frames() == 0 {
                tokio::task::yield_now().await;
            }
            for code in [
                KeyCode::Char('d'),
                KeyCode::Char('n'),
                KeyCode::Char('d'),
                KeyCode::Char('y'),
            ] {
                keys.send(TermEvent::Key(KeyEvent::from(code)));
            }
            let info = test.request().await;
            assert_eq!(info.path, "/v1/info");
            info.json(json!({"serverVersion":"test", "protocolVersion":1, "sessions":[], "features":if outcome == "old server" { vec![] } else { vec!["sessionTrash"] }}));
            if outcome != "old server" {
                let request = test.request().await;
                assert_eq!(request.path, "/v1/sessions/test/trash");
                let response = if outcome == "deleted" {
                    axum::http::StatusCode::NO_CONTENT.into_response()
                } else {
                    (axum::http::StatusCode::CONFLICT, axum::Json(json!({"code":"conflict", "message":"A terminal is attached; leave it first."}))).into_response()
                };
                request.reply.send(response).unwrap();
            }
            // Initial frame + d + n + d + the completed request.
            while keys.frames() < 5 {
                tokio::task::yield_now().await;
            }
            keys.send(TermEvent::Key(KeyEvent::from(KeyCode::Esc)));
        };
        let picked = pick(
            &mut terminal,
            &mut input,
            picker,
            &client,
            &mut shutdown,
            (&mut tab, "work"),
        );
        let ((), picked) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(drive, picked)
        })
        .await
        .unwrap();
        assert_eq!(picked.unwrap(), Pick::Cancel);
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        match outcome {
            "deleted" => assert!(
                screen.contains("No sessions in this directory yet")
                    && !screen.contains("Test session"),
                "{screen}"
            ),
            "attached" => assert!(
                screen.contains("A terminal is attached; leave it first.")
                    && screen.contains("Test session"),
                "{screen}"
            ),
            _ => assert!(
                screen.contains("canter serve --replace") && screen.contains("Test session"),
                "{screen}"
            ),
        }
        assert!(
            test.requests.try_recv().is_err(),
            "no extra delete or resume requests"
        );
        test.stop().await;
    }
}

#[tokio::test]
async fn manual_rename_uses_patch_and_updates_the_visible_session() {
    for name in ["A useful name", ""] {
        let mut test = Harness::new().await;
        // Enable the existing session item in the status bar through its UI.
        test.ui.app_mut().handle_paste("/statusline");
        test.key(KeyCode::Enter);
        test.ui.draw(0).unwrap();
        for _ in 0..12 {
            test.key(KeyCode::Down);
            test.ui.draw(0).unwrap();
        }
        test.key(KeyCode::Char(' '));
        test.ui.draw(0).unwrap();
        test.key(KeyCode::Enter);
        test.ui.draw(0).unwrap();
        test.ui.app_mut().handle_paste(&format!("/rename {name}"));
        test.key(KeyCode::Enter);
        let request = test.request().await;
        assert_eq!(request.path, "/v1/sessions/test");
        assert_eq!(request.body["name"], name);
        assert!(request.body.get("expectedName").is_none());
        request.json(json!({"eventSeq":1}));
        let mut settings = test.ui.app().snapshot().settings.clone();
        settings.name = (!name.is_empty()).then(|| name.to_owned());
        test.event(canter_protocol::EventPayload::SessionUpdated { settings });
        assert_eq!(
            test.ui
                .app()
                .snapshot()
                .settings
                .name
                .as_deref()
                .unwrap_or(""),
            name
        );
        assert!(!test.chat.naming.eligible);
        let screen: String = test
            .ui
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(
            screen.contains("A useful name"),
            !name.is_empty(),
            "{screen}"
        );
        test.stop().await;
    }
}
