use super::retract_tests::Harness;
use super::*;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::json;

#[tokio::test]
async fn fork_command_is_available_while_a_turn_runs() {
    let mut test = Harness::new().await;
    test.start("running", "Original keeps running", None);
    test.ui.app_mut().handle_paste("/fork");
    let action = test
        .ui
        .app_mut()
        .handle_key(KeyEvent::from(KeyCode::Enter), 0);
    assert!(
        matches!(action, Some(Action::ForkSession)),
        "/fork must be a built-in command, including while working"
    );
    test.stop().await;
}

#[tokio::test]
async fn fork_command_in_approval_boxes_does_not_answer_or_interrupt() {
    for paste in [false, true] {
        for kind in [
            json!({"type":"approval","itemId":"tool","tool":"write","arguments":"{}","availableDecisions":["accept","declineWithFeedback","cancel"]}),
            json!({"type":"userInput","itemId":"tool","question":"Choose","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}),
            json!({"type":"planApproval","itemId":"tool","plan":"Proposed plan","approvalPolicy":"alwaysAsk"}),
        ] {
            let mut test = Harness::new().await;
            test.start("waiting", "Original waits", None);
            test.event(serde_json::from_value(json!({"type":"request.opened","request":{"id":"pending","turnId":"waiting","createdAt":0,"kind":kind}})).unwrap());
            test.ui.draw(1000).unwrap();
            let before = test.ui.app().snapshot().clone();
            if paste {
                test.ui.app_mut().handle_paste("/fork");
                test.ui.draw(2000).unwrap();
            } else {
                for c in "/fork".chars() {
                    assert!(
                        test.ui
                            .app_mut()
                            .handle_key(KeyEvent::from(KeyCode::Char(c)), 2000)
                            .is_none()
                    );
                    test.ui.draw(2000).unwrap();
                }
            }
            let action = test
                .ui
                .app_mut()
                .handle_key(KeyEvent::from(KeyCode::Enter), 2000);
            assert!(
                matches!(action, Some(Action::ForkSession)),
                "{kind}: {action:?}"
            );
            assert_eq!(test.ui.app().snapshot(), &before);
            assert!(test.requests.try_recv().is_err());
            test.stop().await;
        }
    }
}

#[tokio::test]
async fn fork_switches_with_history_or_keeps_original_on_failure() {
    use axum::{
        body::Body,
        response::{IntoResponse, Response},
    };
    for outcome in ["success", "failed", "old server", "subscription failed"] {
        let mut test = Harness::new().await;
        test.start("old-turn", "Inherited conversation", None);
        let mut fork = test.ui.app().snapshot().clone();
        fork.id = "forked".into();
        fork.status = SessionStatus::Idle;
        fork.turns[0].status = canter_protocol::TurnStatus::Completed;
        let before = test.ui.app().snapshot().clone();
        let Harness {
            chat, ui, requests, ..
        } = &mut test;
        let drive = async {
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/info");
            request.json(json!({"serverVersion":"test","protocolVersion":1,"sessions":[],"features":if outcome == "old server" { vec![] } else { vec!["sessionFork"] }}));
            if outcome == "old server" {
                return;
            }
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/sessions/test/fork");
            assert_eq!(request.body["env"], json!({}));
            if outcome == "failed" {
                request
                    .reply
                    .send(
                        (
                            axum::http::StatusCode::CONFLICT,
                            axum::Json(
                                json!({"code":"conflict","message":"Saved history unavailable"}),
                            ),
                        )
                            .into_response(),
                    )
                    .unwrap();
                return;
            }
            request.json(json!({"session":fork,"eventSeq":fork.last_event_seq}));
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/events");
            if outcome == "subscription failed" {
                request
                    .reply
                    .send(
                        (
                            axum::http::StatusCode::CONFLICT,
                            axum::Json(
                                json!({"code":"conflict","message":"Subscription unavailable"}),
                            ),
                        )
                            .into_response(),
                    )
                    .unwrap();
                return;
            }
            request
                .reply
                .send(Response::new(Body::from_stream(
                    futures_util::stream::pending::<Result<axum::body::Bytes, io::Error>>(),
                )))
                .unwrap();
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/models");
            request.json(json!({"data":[],"nextCursor":null}));
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/sessions/forked/skills");
            request.json(json!({"data":[]}));
            let request = requests.recv().await.unwrap();
            assert_eq!(request.path, "/v1/sessions/forked/agents");
            request.json(json!({"data":[]}));
        };
        let (receiver, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(chat.fork_session(ui), drive)
        })
        .await
        .unwrap();
        assert_eq!(receiver.is_some(), outcome == "success");
        test.ui.draw(0).unwrap();
        let screen: String = test
            .ui
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        if outcome == "success" {
            assert_eq!(test.chat.session, "forked");
            assert!(screen.contains("Inherited conversation"), "{screen}");
            assert!(
                screen.contains("Session forked.") && screen.contains("/resume"),
                "{screen}"
            );
        } else {
            assert_eq!(test.chat.session, "test");
            assert_eq!(test.ui.app().snapshot(), &before);
            assert!(
                screen.contains(match outcome {
                    "failed" => "Saved history unavailable",
                    "old server" => "canter serve --replace",
                    _ => "Subscription unavailable",
                }),
                "{screen}"
            );
        }
        assert!(test.requests.try_recv().is_err());
        test.chat.pump.abort();
        let _ = test.chat.pump.await;
        test.server.abort();
        assert!(test.server.await.unwrap_err().is_cancelled());
    }
}

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
                agent_status: None,
                parent: None,
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
