use canter_protocol::{CommandRuleListResponse, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::json;

struct Screen(Ui<TestBackend>);

impl Screen {
    fn new() -> Self {
        Self(
            Ui::new(
                TestBackend::new(120, 40),
                App::new(SessionSnapshot::empty("test"), None, 0),
            )
            .unwrap(),
        )
    }

    fn show(&mut self, result: Result<CommandRuleListResponse, String>) {
        self.0.app_mut().show_command_rules(result);
        self.draw();
    }

    fn draw(&mut self) {
        self.0.draw(1000).unwrap();
    }

    fn key(&mut self, key: KeyCode) -> Option<Action> {
        let action = self
            .0
            .app_mut()
            .handle_key(KeyEvent::new(key, KeyModifiers::NONE), 1000);
        self.draw();
        action
    }

    fn text(&self) -> String {
        let buffer = self.0.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn rules() -> CommandRuleListResponse {
    serde_json::from_value(json!({"path":"/server/state/rules.json","data":[
        {"pattern":["cargo","publish"],"decision":"forbidden"},
        {"pattern":["cargo","test"],"decision":"allow"},
        {"pattern":["git","push"],"decision":"prompt"}
    ]}))
    .unwrap()
}

#[test]
fn permissions_groups_rules_and_shows_the_server_file() {
    let mut screen = Screen::new();
    screen.show(Ok(rules()));
    let text = screen.text();
    let positions: Vec<_> = [
        "Allow",
        "cargo test",
        "Ask",
        "git push",
        "Deny",
        "cargo publish",
    ]
    .into_iter()
    .map(|word| text.find(word).unwrap())
    .collect();
    assert!(positions.is_sorted(), "{text}");
    assert!(text.contains("/server/state/rules.json"));
    assert!(text.contains("Add or edit rules in this file."));
}

#[test]
fn permissions_requires_confirmation_and_escape_cancels_without_deleting() {
    for start in [KeyCode::Enter, KeyCode::Delete] {
        let mut screen = Screen::new();
        screen.show(Ok(rules()));
        assert_eq!(screen.key(start), None);
        assert!(
            screen
                .text()
                .contains("Delete rule \"cargo test\" (Allow)?"),
            "{}",
            screen.text()
        );
        assert!(screen.text().contains("Enter to confirm"));
        assert_eq!(screen.key(KeyCode::Esc), None);
        assert!(screen.text().contains("› cargo test"));
        assert_eq!(screen.key(KeyCode::Down), None);
        assert_eq!(screen.key(KeyCode::Delete), None);
        assert!(screen.text().contains("Delete rule \"git push\" (Ask)?"));
        assert_eq!(
            screen.key(KeyCode::Enter),
            Some(Action::DeleteCommandRule(rules().data[2].clone()))
        );
    }
    let mut screen = Screen::new();
    screen.show(Ok(rules()));
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert!(!screen.text().contains("Permissions"));
}

#[test]
fn permissions_empty_and_broken_files_do_not_offer_deletion() {
    let mut screen = Screen::new();
    screen.show(Ok(CommandRuleListResponse {
        path: "/server/state/rules.json".into(),
        data: vec![],
    }));
    let text = screen.text();
    assert!(text.contains("No command rules."));
    assert!(text.contains("Always allow") && text.contains("edit the rules file"));
    assert!(text.contains("/server/state/rules.json"));
    assert_eq!(screen.key(KeyCode::Delete), None);
    assert_eq!(screen.key(KeyCode::Enter), None);
    screen.show(Err(
        "invalid command rules JSON at /server/state/rules.json".into(),
    ));
    assert!(screen.text().contains("invalid command rules JSON"));
    assert_eq!(screen.key(KeyCode::Delete), None);
    assert_eq!(screen.key(KeyCode::Enter), None);
}

#[test]
fn permissions_does_not_confirm_from_an_unseen_frame_or_key_repeat() {
    let mut screen = Screen::new();
    screen.show(Ok(rules()));
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(screen.0.app_mut().handle_key(enter, 1000), None);
    assert_eq!(screen.0.app_mut().handle_key(enter, 1000), None);
    screen.draw();
    let mut repeat = enter;
    repeat.kind = KeyEventKind::Repeat;
    assert_eq!(screen.0.app_mut().handle_key(repeat, 1000), None);
    assert!(screen.key(KeyCode::Enter).is_some());
}

#[test]
fn permissions_keeps_the_selected_rule_visible_in_a_long_list() {
    let mut screen = Screen::new();
    let mut list = rules();
    list.data = (0..60)
        .map(|index| {
            serde_json::from_value(
                json!({"pattern":["command",index.to_string()],"decision":"allow"}),
            )
            .unwrap()
        })
        .collect();
    screen.show(Ok(list));
    for _ in 0..59 {
        assert_eq!(screen.key(KeyCode::Down), None);
    }
    assert!(screen.text().contains("› command 59"));
    assert_eq!(screen.key(KeyCode::Up), None);
    assert!(screen.text().contains("› command 58"));
}

#[tokio::test]
async fn permissions_client_deletes_the_confirmed_content_and_reloads_the_list() {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use canter_protocol::CommandRule;
    use canter_tui::client::Client;
    use std::sync::{Arc, Mutex};

    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut initial = rules();
    initial.data = vec![initial.data[1].clone()];
    let shared = Arc::new(Mutex::new(initial));
    let (listed, deleted) = (shared.clone(), shared.clone());
    let router = Router::new()
        .route(
            "/v1/commandRules",
            get(move || {
                let listed = listed.clone();
                async move { Json(listed.lock().unwrap().clone()) }
            }),
        )
        .route(
            "/v1/commandRules/delete",
            post(move |Json(rule): Json<CommandRule>| {
                let deleted = deleted.clone();
                async move {
                    assert_eq!(
                        serde_json::to_value(rule).unwrap(),
                        json!({"pattern":["cargo","test"],"decision":"allow"})
                    );
                    deleted.lock().unwrap().data.clear();
                    Json(json!({"removed":1,"message":"Deleted 1 command rule."}))
                }
            }),
        );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = Client::new(&socket).unwrap();
    let mut screen = Screen::new();
    screen.show(Ok(client.command_rules().await.unwrap()));
    assert_eq!(screen.key(KeyCode::Delete), None);
    assert_eq!(shared.lock().unwrap().data.len(), 1);
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert_eq!(shared.lock().unwrap().data.len(), 1);
    assert_eq!(screen.key(KeyCode::Enter), None);
    let Some(Action::DeleteCommandRule(rule)) = screen.key(KeyCode::Enter) else {
        panic!("confirmed deletion")
    };
    let result = client.delete_command_rule(&rule).await.unwrap();
    assert_eq!(result.removed, 1);
    assert_eq!(result.message, "Deleted 1 command rule.");
    screen.show(Ok(client.command_rules().await.unwrap()));
    assert!(screen.text().contains("No command rules."));
    assert!(!screen.text().contains("cargo test"));
    stop.send(()).unwrap();
    server.await.unwrap();
}
