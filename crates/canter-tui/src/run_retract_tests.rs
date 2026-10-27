use super::*;
use axum::{Router, body::Body, extract::State, response::Response};
use canter_protocol::{EventPayload, Turn, TurnStatus};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use tokio::{net::UnixListener, sync::oneshot};

pub(super) struct Request {
    pub(super) path: String,
    pub(super) body: Value,
    pub(super) reply: oneshot::Sender<Response>,
}
impl Request {
    pub(super) fn json(self, value: Value) {
        use axum::response::IntoResponse;
        assert!(self.reply.send(axum::Json(value).into_response()).is_ok());
    }
}

// Exercise the terminal dispatcher against a real local HTTP boundary.
pub(super) struct Harness {
    pub(super) chat: Chat<SocketConnector>,
    pub(super) ui: Ui<TestBackend>,
    pub(super) requests: mpsc::UnboundedReceiver<Request>,
    messages: mpsc::UnboundedReceiver<Message>,
    pub(super) server: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
}
impl Harness {
    pub(super) async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let (sender, requests) = mpsc::unbounded_channel();
        let router = Router::new()
            .fallback(
                |State(sender): State<mpsc::UnboundedSender<Request>>,
                 request: axum::http::Request<Body>| async move {
                    let path = request.uri().path().to_owned();
                    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                        .await
                        .unwrap();
                    let body = if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).unwrap()
                    };
                    let (reply, receive) = oneshot::channel();
                    sender.send(Request { path, body, reply }).unwrap();
                    receive.await.unwrap()
                },
            )
            .with_state(sender);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let (sender, messages) = mpsc::unbounded_channel();
        let naming = AutoName::new(&SessionSnapshot::empty("test"), false, sender.clone());
        let chat = Chat {
            connector: Arc::new(SocketConnector(socket.clone())),
            client: Client::new(&socket).unwrap(),
            server: None,
            session: "test".into(),
            env: BTreeMap::new(),
            client_name: "test".into(),
            home: Some(temp.path().to_string_lossy().into_owned()),
            version: "test".into(),
            status_line: None,
            sender,
            pump: tokio::spawn(async {}),
            btw: None,
            naming,
        };
        let mut snapshot = SessionSnapshot::empty("test");
        snapshot.settings.cwd = temp.path().to_string_lossy().into_owned();
        snapshot.settings.model = "deepseek-flash".into();
        snapshot.status = SessionStatus::Idle;
        let mut ui = Ui::new(TestBackend::new(90, 28), App::new(snapshot, None, 0)).unwrap();
        ui.draw(0).unwrap();
        Self {
            chat,
            ui,
            requests,
            messages,
            server,
            _temp: temp,
        }
    }
    pub(super) async fn request(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    pub(super) async fn message(&mut self) {
        let message = tokio::time::timeout(Duration::from_secs(5), self.messages.recv())
            .await
            .unwrap()
            .unwrap();
        self.chat.message(&mut self.ui, message);
        self.ui.draw(0).unwrap();
    }
    pub(super) fn event(&mut self, payload: EventPayload) {
        let event = Event {
            seq: self.ui.app().snapshot().last_event_seq + 1,
            session_id: "test".into(),
            timestamp: 0,
            payload,
        };
        self.chat
            .message(&mut self.ui, Message::Server(Box::new(event)));
        self.ui.draw(0).unwrap();
    }
    pub(super) fn start(&mut self, id: &str, text: &str, review: Option<&str>) {
        let turn: Turn = serde_json::from_value(json!({
            "id":id,"input":[{"type":"text","text":text}],"items":[],"status":"inProgress",
            "clientName":"test","error":null,"usage":null,"review":review,
        }))
        .unwrap();
        self.event(EventPayload::TurnStarted {
            turn,
            input_id: None,
        });
        self.event(EventPayload::SessionStatus {
            status: SessionStatus::Working,
            close_reason: None,
        });
    }
    pub(super) fn key(&mut self, code: KeyCode) {
        if let Some(action) = self
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0)
        {
            self.chat.act(self.ui.app_mut(), action);
        }
    }
    pub(super) async fn stop(self) {
        self.server.abort();
        assert!(self.server.await.unwrap_err().is_cancelled());
        self.chat.pump.await.unwrap();
    }
}

async fn failed_text_send(pastes: &[&str], typing: &str) -> Harness {
    use axum::response::IntoResponse;
    let mut test = Harness::new().await;
    for paste in pastes {
        test.ui.app_mut().handle_paste(paste);
    }
    test.key(KeyCode::Enter);
    let request = test.request().await;
    assert_eq!(request.path, "/v1/sessions/test/input");
    assert_eq!(
        request.body["input"],
        json!([{"type":"text","text":pastes.concat()}])
    );
    test.ui.app_mut().handle_paste(typing);
    let response = (
        axum::http::StatusCode::CONFLICT,
        axum::Json(json!({"code":"conflict","message":"offline"})),
    )
        .into_response();
    assert!(request.reply.send(response).is_ok());
    test.message().await;
    let shown: String = test
        .ui
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(shown.contains("✗ Not sent:"), "{shown}");
    assert!(shown.contains("offline"), "{shown}");
    test
}

#[tokio::test]
async fn user_shell_dispatches_command_and_restores_draft_on_busy_response() {
    use axum::response::IntoResponse;
    for busy in [false, true] {
        let mut test = Harness::new().await;
        test.ui.app_mut().handle_paste("! git status ");
        test.key(KeyCode::Enter);
        let request = test.request().await;
        assert_eq!(request.path, "/v1/sessions/test/shellCommand");
        assert_eq!(request.body, json!({"command":"git status"}));
        if busy {
            let response = (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({
                    "code":"conflict", "message":"wait for this turn to finish"
                })),
            )
                .into_response();
            assert!(request.reply.send(response).is_ok());
        } else {
            request.json(json!({"eventSeq":1,"turnId":"shell-turn"}));
        }
        test.message().await;
        let retry = test
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
        assert_eq!(
            retry,
            busy.then(|| Action::ShellCommand("git status".into()))
        );
        assert!(
            test.requests.try_recv().is_err(),
            "never send /input for a shell command"
        );
        test.stop().await;
    }
}

#[tokio::test]
async fn failed_text_send_restores_the_draft() {
    let mut test = failed_text_send(&["first line\nsecond line"], "").await;
    assert_eq!(
        test.ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("first line\nsecond line".into()))
    );
    test.stop().await;
}

#[tokio::test]
async fn failed_text_send_keeps_newly_typed_text() {
    let mut test = failed_text_send(&["original draft"], "new typing").await;
    assert_eq!(
        test.ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("original draftnew typing".into()))
    );
    test.stop().await;
}

#[tokio::test]
async fn failed_text_send_keeps_pastes_folded() {
    let paste = (1..=11)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut test = failed_text_send(&["before ", &paste, " after"], "").await;
    let shown: String = test
        .ui
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        shown.contains("before [Pasted #1 · 11 lines] after"),
        "{shown}"
    );
    assert!(!shown.contains("line 5"), "{shown}");
    assert_eq!(
        test.ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send(format!("before {paste} after")))
    );
    test.stop().await;
}

#[tokio::test]
async fn review_escape_sends_an_ordinary_interrupt() {
    let mut test = Harness::new().await;
    test.start(
        "review",
        "generated instructions and diff",
        Some("Review changes"),
    );
    test.key(KeyCode::Esc);
    let request = test.request().await;
    assert_eq!(request.path, "/v1/sessions/test/interrupt");
    assert_eq!(request.body, json!({"turnId":"review"}));
    request.json(json!({"eventSeq":3}));
    test.message().await;
    test.stop().await;
}

#[tokio::test]
async fn first_message_after_user_shell_becomes_the_session_name() {
    for event_first in [false, true] {
        let mut test = Harness::new().await;
        test.ui.app_mut().handle_paste("!git status");
        test.key(KeyCode::Enter);
        let command = test.request().await;
        assert_eq!(command.path, "/v1/sessions/test/shellCommand");
        command.json(json!({"eventSeq":1,"turnId":"shell"}));
        test.message().await;
        let mut turn: Turn = serde_json::from_value(json!({
            "id":"shell","input":[],"items":[],"status":"inProgress",
            "clientName":"userShell","error":null,"usage":null,
        }))
        .unwrap();
        test.event(EventPayload::TurnStarted {
            turn: turn.clone(),
            input_id: None,
        });
        test.event(EventPayload::ItemCompleted {
            turn_id: turn.id.clone(),
            item: serde_json::from_value(json!({
                "type":"commandExecution","id":"shell-item","source":"userShell",
                "command":"git status","cwd":"/work","status":"completed",
                "aggregatedOutput":"clean","exitCode":0,"durationMs":1,
            }))
            .unwrap(),
        });
        turn.status = TurnStatus::Completed;
        test.event(EventPayload::TurnCompleted { turn });
        assert!(
            test.requests.try_recv().is_err(),
            "a command does not name the session"
        );

        let text = "修复登录页面";
        test.ui.app_mut().handle_paste(text);
        test.key(KeyCode::Enter);
        let input = test.request().await;
        assert_eq!(input.path, "/v1/sessions/test/input");
        let seq = test.ui.app().snapshot().last_event_seq + 1;
        if event_first {
            test.start("first-message", text, None);
        }
        input.json(json!({"eventSeq":seq,"turnId":"first-message"}));
        test.message().await;
        if !event_first {
            test.start("first-message", text, None);
        }
        let naming = test.request().await;
        assert_eq!(naming.path, "/v1/sessions/test");
        assert_eq!(naming.body["name"], text);
        naming.json(json!({"eventSeq":test.ui.app().snapshot().last_event_seq + 1}));
        test.stop().await;
    }
}

#[tokio::test]
async fn retract_clears_its_auto_title_even_if_naming_finishes_late_and_names_the_next_input() {
    let mut test = Harness::new().await;
    test.ui.app_mut().handle_paste("mistyped title");
    test.key(KeyCode::Enter);
    let input = test.request().await;
    assert_eq!(input.path, "/v1/sessions/test/input");
    input.json(json!({"eventSeq":1,"turnId":"first"}));
    test.message().await;
    test.start("first", "mistyped title", None);
    let naming = test.request().await;
    assert_eq!(naming.body["name"], "mistyped title");
    // Keep the naming reply in flight while Esc retracts this input.
    test.key(KeyCode::Esc);
    let interrupt = test.request().await;
    test.event(EventPayload::TurnRetracted {
        turn_id: "first".into(),
    });
    test.event(EventPayload::SessionStatus {
        status: SessionStatus::Idle,
        close_reason: None,
    });
    interrupt.json(json!({"eventSeq":3,"retracted":true}));
    test.message().await;
    let mut settings = test.ui.app().snapshot().settings.clone();
    settings.name = Some("mistyped title".into());
    test.event(EventPayload::SessionUpdated {
        settings: settings.clone(),
    });
    naming.json(json!({"eventSeq":5}));
    let clear = test.request().await;
    assert_eq!(clear.path, "/v1/sessions/test");
    assert_eq!(clear.body["name"], "");
    settings.name = None;
    test.event(EventPayload::SessionUpdated { settings });
    clear.json(json!({"eventSeq":6}));
    test.chat
        .act(test.ui.app_mut(), Action::Send("corrected title".into()));
    let input = test.request().await;
    input.json(json!({"eventSeq":7,"turnId":"next"}));
    test.message().await;
    test.start("next", "corrected title", None);
    let naming = test.request().await;
    assert_eq!(naming.body["name"], "corrected title");
    naming.json(json!({"eventSeq":9}));
    test.stop().await;
}

#[tokio::test]
async fn lost_interrupt_reply_still_restores_when_the_retraction_event_arrives() {
    use axum::response::IntoResponse;
    for (event_first, connection_error) in [(false, true), (true, true), (false, false)] {
        let mut test = Harness::new().await;
        test.start("first", "recover this input", None);
        test.ui.app_mut().handle_paste("existing draft");
        test.key(KeyCode::Esc);
        let request = test.request().await;
        if event_first {
            test.event(EventPayload::TurnRetracted {
                turn_id: "first".into(),
            });
        }
        let response = if connection_error {
            Response::new(Body::from_stream(futures_util::stream::once(async {
                Err::<axum::body::Bytes, _>(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "test dropped reply",
                ))
            })))
        } else {
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"code":"conflict","message":"definitive rejection"})),
            )
                .into_response()
        };
        assert!(request.reply.send(response).is_ok());
        test.message().await;
        if !event_first {
            test.event(EventPayload::TurnRetracted {
                turn_id: "first".into(),
            });
        }
        test.event(EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        });
        assert_eq!(
            test.ui
                .app_mut()
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            Some(Action::Send(
                if connection_error {
                    "recover this input\nexisting draft"
                } else {
                    "existing draft"
                }
                .into()
            ))
        );
        test.stop().await;
    }
}
