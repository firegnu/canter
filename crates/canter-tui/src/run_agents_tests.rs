//! Sub-agents on screen against a local HTTP server (P2-179): the row
//! under the info rows (P2-240), `/agents` and `s` to stop, a sub-agent's screen
//! with the main session's approvals, and the tool cards.

use super::retract_tests::Harness;
use super::*;
use axum::body::{Body, Bytes};
use axum::response::Response;
use canter_protocol::{AgentState, AgentStatus, EventPayload};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::{Value, json};

/// Keys come in well after the frames are drawn (at 0), past the guards.
const LATER: i64 = 10_000;

fn rows(test: &Harness) -> Vec<String> {
    let buffer = test.ui.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn shown(test: &Harness) -> String {
    rows(test).join("\n")
}

fn has(test: &Harness, parts: &[&str]) -> bool {
    rows(test)
        .iter()
        .any(|row| parts.iter().all(|part| row.contains(part)))
}

/// The key's action, left for the test to carry out.
fn press(test: &mut Harness, code: KeyCode) -> Option<Action> {
    let action = test
        .ui
        .app_mut()
        .handle_key(KeyEvent::new(code, KeyModifiers::NONE), LATER);
    test.ui.draw(0).unwrap();
    action
}

fn agent(id: &str, kind: &str, status: &str, more: Value) -> Value {
    let mut agent = json!({
        "id": id, "task": "Inspect", "status": status, "result": null,
        "createdAt": 0, "updatedAt": 0, "name": format!("agent-{id}"), "agentType": kind,
    });
    agent
        .as_object_mut()
        .unwrap()
        .extend(more.as_object().unwrap().clone());
    agent
}

fn payload(value: Value) -> EventPayload {
    serde_json::from_value(value).unwrap()
}

/// Types `/agents`, answers the list read with `list` and shows it.
async fn open_list(test: &mut Harness, main: &str, list: Value) {
    for c in "/agents".chars() {
        press(test, KeyCode::Char(c));
    }
    let action = press(test, KeyCode::Enter).unwrap();
    test.chat.act(test.ui.app_mut(), action);
    let request = test.request().await;
    assert_eq!(request.path, format!("/v1/sessions/{main}/agents"));
    request.json(json!({ "data": list }));
    test.message().await;
}

#[tokio::test]
async fn the_row_opens_the_list_where_s_stops_a_running_agent() {
    let mut test = Harness::new().await;
    assert!(!shown(&test).contains("sub-agent"), "{}", shown(&test));
    // The harness draws at 0.
    let started = json!({ "startedAt": -42_000 });
    let a = agent("a", "explorer", "running", started.clone());
    let b = agent("b", "general", "running", started.clone());
    let c = agent(
        "c",
        "general",
        "completed",
        json!({ "durationMs": 133_000 }),
    );
    for entry in [&a, &b, &c] {
        test.event(payload(json!({ "type": "agent.updated", "agent": entry })));
    }
    // Under the two info rows (P2-240), live while the session is idle.
    let screen = rows(&test);
    let bottom = screen.iter().rposition(|row| row.starts_with('╰')).unwrap();
    assert_eq!(
        screen[bottom + 3].trim(),
        "● 2 sub-agents running · 1 done · ↓ to view"
    );
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: (bottom + 3) as u16,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        test.ui.app_mut().handle_mouse(click),
        Some(Action::AgentList)
    );

    open_list(&mut test, "test", json!([a, b, c])).await;
    let text = shown(&test);
    assert!(
        has(&test, &["╭─ Sub-agents · 2 running · 1 done ─"]),
        "{text}"
    );
    assert!(!text.contains("sub-agents running"), "{text}");
    assert!(has(&test, &["│   Main session · test (here)"]), "{text}");
    assert!(
        has(&test, &["│ › ├ ● agent-a  explorer  running  42s"]),
        "{text}"
    );
    assert!(
        has(&test, &["│   ├ ● agent-b  general   running  42s"]),
        "{text}"
    );
    assert!(
        has(&test, &["│   └ ✓ agent-c  general   done     2m 13s"]),
        "{text}"
    );
    assert!(
        has(&test, &["↑↓ select · Enter open · s stop · Esc close"]),
        "{text}"
    );

    // `s` stops the running agent selected through the server.
    test.ui
        .app_mut()
        .handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE), LATER)
        .map(|action| test.chat.act(test.ui.app_mut(), action))
        .unwrap();
    let stop = test.request().await;
    assert_eq!(stop.path, "/v1/sessions/test/agents/a/stop");
    stop.json(agent(
        "a",
        "explorer",
        "interrupted",
        json!({ "stoppedByUser": true, "durationMs": 5_000 }),
    ));
    test.message().await;
    let text = shown(&test);
    assert!(
        has(&test, &["├ ■ agent-a  explorer  stopped by you  5s"]),
        "{text}"
    );
    assert!(
        has(&test, &["╭─ Sub-agents · 1 running · 2 done ─"]),
        "{text}"
    );
    // Not running any more: `s` does nothing.
    assert_eq!(press(&mut test, KeyCode::Char('s')), None);
    press(&mut test, KeyCode::Down);
    press(&mut test, KeyCode::Down);
    assert_eq!(press(&mut test, KeyCode::Char('s')), None);

    // The open list follows the events too.
    let b = agent("b", "general", "failed", json!({ "durationMs": 9_000 }));
    test.event(payload(json!({ "type": "agent.updated", "agent": b })));
    let text = shown(&test);
    assert!(
        rows(&test)
            .iter()
            .any(|row| row.starts_with("╭─ Sub-agents · 3 done ─")),
        "{text}"
    );
    assert!(
        has(&test, &["├ ✗ agent-b  general   failed          9s"]),
        "{text}"
    );
    assert_eq!(press(&mut test, KeyCode::Up), None);
    assert_eq!(
        press(&mut test, KeyCode::Enter),
        Some(Action::OpenAgent("b".into()))
    );
    let text = shown(&test);
    assert!(has(&test, &["○ 3 sub-agents · ↓ to view"]), "{text}");
    assert!(!text.contains("▾"), "{text}");
    test.stop().await;
}

/// The cell at the start of `text` in the first row showing it.
fn cell(test: &Harness, text: &str) -> ratatui::buffer::Cell {
    let (y, row) = rows(test)
        .into_iter()
        .enumerate()
        .find(|(_, row)| row.contains(text))
        .unwrap_or_else(|| panic!("{text} not in\n{}", shown(test)));
    let x = crate::text::str_width(&row[..row.find(text).unwrap()]);
    test.ui.backend().buffer()[(x as u16, y as u16)].clone()
}

fn row_of(test: &Harness, text: &str) -> u16 {
    rows(test)
        .iter()
        .position(|row| row.contains(text))
        .unwrap() as u16
}

#[tokio::test]
async fn the_list_hangs_the_sub_agents_under_the_main_session() {
    use ratatui::style::{Color, Modifier};
    let mut test = Harness::new().await;
    let mut settings = test.ui.app().snapshot().settings.clone();
    settings.name = Some(
        "Use the reviewer sub-agent to review the code in src that parses dates and durations, then report back"
            .into(),
    );
    test.event(EventPayload::SessionUpdated { settings });
    let a = agent("a", "reviewer", "running", json!({ "startedAt": -125_000 }));
    let b = agent(
        "b",
        "explorer",
        "completed",
        json!({ "durationMs": 41_000 }),
    );
    let c = agent("c", "general", "failed", json!({ "durationMs": 12_000 }));
    for entry in [&a, &b, &c] {
        test.event(payload(json!({ "type": "agent.updated", "agent": entry })));
    }
    // The row under the info rows: a coloured mark, a dim way to open.
    let row = "● 1 sub-agent running · 2 done · ↓ to view";
    assert!(has(&test, &[row]), "{}", shown(&test));
    assert_eq!(cell(&test, row).fg, Color::Cyan);
    assert!(cell(&test, "↓ to view").modifier.contains(Modifier::DIM));

    open_list(&mut test, "test", json!([a, b, c])).await;
    let text = shown(&test);
    assert!(
        has(&test, &["╭─ Sub-agents · 1 running · 2 done ─"]),
        "{text}"
    );
    // The main session on top, dim and cut short, here; not selected.
    assert!(
        has(
            &test,
            &["│   Main session · Use the reviewer sub-agent", "… (here)"]
        ),
        "{text}"
    );
    let main = cell(&test, "Main session");
    assert!(main.modifier.contains(Modifier::DIM), "{main:?}");
    assert!(!main.modifier.contains(Modifier::BOLD), "{main:?}");
    // The sub-agents hang under it, in four columns.
    assert!(
        has(&test, &["│ › ├ ● agent-a  reviewer  running  2m 05s"]),
        "{text}"
    );
    assert!(
        has(&test, &["│   ├ ✓ agent-b  explorer  done     41s"]),
        "{text}"
    );
    assert!(
        has(&test, &["│   └ ✗ agent-c  general   failed   12s"]),
        "{text}"
    );
    assert!(cell(&test, "├ ✓").modifier.contains(Modifier::DIM));
    assert!(cell(&test, "reviewer").modifier.contains(Modifier::DIM));
    assert_eq!(cell(&test, "running  2m").fg, Color::Cyan);
    assert_eq!(cell(&test, "2m 05s").fg, Color::Cyan);
    assert_eq!(cell(&test, "done     41s").fg, Color::Green);
    assert_eq!(cell(&test, "41s").fg, Color::Green);
    assert_eq!(cell(&test, "failed   12s").fg, Color::Red);
    assert!(!text.contains("completed"), "{text}");

    // Enter on the main session closes the list: it is the one on screen.
    assert_eq!(press(&mut test, KeyCode::Up), None);
    assert!(has(&test, &["│ › Main session · "]), "{}", shown(&test));
    assert_eq!(press(&mut test, KeyCode::Char('s')), None);
    assert_eq!(press(&mut test, KeyCode::Enter), None);
    assert!(has(&test, &[row]), "{}", shown(&test));
    assert!(!shown(&test).contains("Main session"));

    // So does a click on it.
    open_list(&mut test, "test", json!([a, b, c])).await;
    assert!(has(&test, &["│ › ├ ● agent-a"]), "{}", shown(&test));
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 6,
        row: row_of(&test, "Main session"),
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(test.ui.app_mut().handle_mouse(click), None);
    test.ui.draw(0).unwrap();
    assert!(has(&test, &[row]), "{}", shown(&test));
    test.stop().await;
}

#[tokio::test]
async fn agents_without_sub_agents_says_so() {
    let mut test = Harness::new().await;
    open_list(&mut test, "test", json!([])).await;
    let text = shown(&test);
    assert!(has(&test, &[" No sub-agents in this session."]), "{text}");
    assert!(!text.contains("▸"), "{text}");
    test.stop().await;
}

/// An event of `session` as one SSE frame.
fn frame(session: &str, seq: u64, payload: Value) -> Bytes {
    let mut event = json!({ "seq": seq, "sessionId": session, "timestamp": 0 });
    event
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().unwrap().clone());
    Bytes::from(format!("data: {event}\n\n"))
}

fn approval(id: &str, turn: &str, command: &str) -> Value {
    json!({
        "id": id, "turnId": turn, "createdAt": 0,
        "kind": { "type": "approval", "itemId": format!("item-{id}"), "tool": "shell",
                  "arguments": { "command": command },
                  "availableDecisions": ["accept", "decline", "cancel"] },
    })
}

#[tokio::test]
async fn a_sub_agents_screen_shows_it_with_the_main_sessions_approvals() {
    let mut test = Harness::new().await;
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("binding.json");
    let mut file =
        crate::session_file::SessionFile::new(path.clone(), files.path().join("state")).unwrap();
    file.publish(test.ui.app().snapshot()).unwrap();
    let original_binding = std::fs::read(&path).unwrap();
    test.chat.session_file = Some(file);
    let cwd = test.ui.app().snapshot().settings.cwd.clone();
    let mut child = SessionSnapshot::empty("child");
    child.status = SessionStatus::Working;
    child.settings.cwd = cwd.clone();
    child.settings.name = Some("agent-child".into());
    child.parent = Some(
        serde_json::from_value(json!({
            "agentType": "explorer", "sessionId": "test", "turnId": "t0", "task": "Trace locks",
        }))
        .unwrap(),
    );
    child.agent = Some(AgentState {
        status: AgentStatus::Running,
        result: None,
        stopped_by_user: false,
    });
    child.turns.push(
        serde_json::from_value(json!({
            "id": "ct1", "input": [{ "type": "text", "text": "Trace locks" }], "items": [],
            "status": "inProgress", "clientName": "agent", "error": null, "usage": null,
            "review": null,
        }))
        .unwrap(),
    );
    child.last_event_seq = 5;
    let mut main = SessionSnapshot::empty("test");
    main.status = SessionStatus::WaitingApproval;
    main.settings.cwd = cwd;
    main.settings.name = Some("Refactor locks".into());
    main.pending_requests =
        vec![serde_json::from_value(approval("r1", "t0", "cargo test")).unwrap()];
    let question = json!({
        "id": "question", "turnId": "t0", "createdAt": 0,
        "kind": { "type": "userInput", "itemId": "ask", "question": "Which parser?",
            "options": [ { "label": "Current", "description": "Use current parser" },
                         { "label": "New", "description": "Use new parser" } ] },
    });
    main.pending_requests
        .push(serde_json::from_value(question.clone()).unwrap());
    // This sub-agent's own request reaches the main session forwarded; it
    // is not shown again as someone else's.
    main.agent_requests = vec![
        serde_json::from_value(json!({
            "agentId": "child", "agentName": "agent-child", "cwd": "/w", "item": null,
            "request": approval("r0", "ct1", "rm -rf build"),
        }))
        .unwrap(),
    ];
    main.last_event_seq = 9;
    let (events, receive) = mpsc::unbounded_channel::<Bytes>();
    let Harness {
        chat, ui, requests, ..
    } = &mut test;
    let (receiver, ()) = tokio::join!(
        chat.open(ui, "child", "Could not open the session", false),
        async {
            let resume = next(requests).await;
            assert_eq!(resume.path, "/v1/sessions/child/resume");
            resume.json(json!({ "session": child, "eventSeq": 5 }));
            let stream = next(requests).await;
            assert_eq!(stream.path, "/v1/events");
            let open = futures_util::stream::pending::<Result<Bytes, io::Error>>();
            assert!(
                stream
                    .reply
                    .send(Response::new(Body::from_stream(open)))
                    .is_ok()
            );
            next(requests)
                .await
                .json(json!({ "data": [], "nextCursor": null }));
            let skills = next(requests).await;
            assert_eq!(skills.path, "/v1/sessions/child/skills");
            skills.json(json!({ "data": [] }));
            let read = next(requests).await;
            assert_eq!(read.path, "/v1/sessions/test");
            read.json(json!({ "session": main, "eventSeq": 9 }));
            let stream = next(requests).await;
            assert_eq!(stream.path, "/v1/events");
            let main_events = futures_util::stream::unfold(receive, |mut receive| async move {
                let bytes = receive.recv().await?;
                Some((Ok::<_, io::Error>(bytes), receive))
            });
            assert!(
                stream
                    .reply
                    .send(Response::new(Body::from_stream(main_events)))
                    .is_ok()
            );
            let list = next(requests).await;
            assert_eq!(list.path, "/v1/sessions/test/agents");
            list.json(json!({ "data": [
                agent("child", "explorer", "running", json!({})),
                agent("sib", "general", "running", json!({})),
            ] }));
        }
    );
    test.messages = receiver.unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original_binding,
        "viewing a child keeps the main session binding"
    );
    test.chat.message(
        &mut test.ui,
        Message::Reconnected(Connected {
            client: test.chat.client.clone(),
            warning: None,
            server: None,
        }),
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original_binding,
        "reconnecting on a child screen still publishes the main session"
    );
    let refreshed = next(&mut test.requests).await;
    assert_eq!(refreshed.path, "/v1/sessions/test/agents");
    refreshed.json(json!({ "data": [
        agent("child", "explorer", "running", json!({})),
        agent("sib", "general", "running", json!({})),
    ] }));
    test.message().await;
    test.ui.draw(0).unwrap();
    let screen = rows(&test);
    let text = screen.join("\n");
    assert_eq!(screen[0], " Sub-agent · agent-child · explorer · running");
    assert_eq!(screen[1], " from: Refactor locks · /agents to go back");
    const WAITING: &str = "Main session is waiting for you · /agents to go back";
    assert_eq!(screen[2].trim(), WAITING);
    assert_eq!(
        test.ui.backend().buffer()[(1, 2)].fg,
        ratatui::style::Color::Yellow
    );
    assert!(!text.contains("Which parser?"), "{text}");
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        test.ui.app_mut().handle_mouse(click),
        Some(Action::OpenAgent("test".into()))
    );
    // The main session's own approval, not from a sub-agent.
    assert!(text.contains("cargo test"), "{text}");
    assert!(!text.contains("Requested by sub-agent"), "{text}");

    // Another sub-agent asks once the main session's is answered.
    events
        .send(frame(
            "test",
            10,
            json!({ "type": "request.resolved", "requestId": "r1",
                    "resolution": { "type": "answered", "decision": "accept" }, "clientName": "other" }),
        ))
        .unwrap();
    events
        .send(frame(
            "test",
            11,
            json!({ "type": "agent.request.opened", "agentId": "sib", "agentName": "agent-sib",
                    "cwd": "/w", "item": null, "request": approval("r2", "st1", "cargo build") }),
        ))
        .unwrap();
    test.message().await;
    test.message().await;
    let text = shown(&test);
    assert!(text.contains("Requested by sub-agent agent-sib"), "{text}");
    assert!(text.contains("cargo build"), "{text}");
    assert!(!text.contains("rm -rf build"), "{text}");
    // Its answer goes to that sub-agent.
    let answer = press(&mut test, KeyCode::Enter).unwrap();
    test.chat.act(test.ui.app_mut(), answer);
    let resolve = test.request().await;
    assert_eq!(resolve.path, "/v1/sessions/sib/requests/r2");
    resolve.json(json!({ "eventSeq": 12, "alreadyResolved": false }));
    events
        .send(frame(
            "test",
            12,
            json!({ "type": "agent.request.resolved", "agentId": "sib", "requestId": "r2",
                    "resolution": { "type": "answered", "decision": "accept" }, "clientName": "test" }),
        ))
        .unwrap();
    test.message().await;
    let text = shown(&test);
    assert!(!text.contains("cargo build"), "{text}");
    assert!(has(&test, &["› Message this sub-agent"]), "{text}");

    // `/agents` here goes back to the main session first.
    open_list(
        &mut test,
        "test",
        json!([
            agent("child", "explorer", "running", json!({})),
            agent("sib", "general", "running", json!({})),
        ]),
    )
    .await;
    let text = shown(&test);
    assert!(has(&test, &["╭─ Sub-agents · 2 running ─"]), "{text}");
    assert!(has(&test, &["│ › Main session · Refactor locks"]), "{text}");
    assert!(!text.contains("←"), "{text}");
    assert!(
        has(&test, &["│   ├ ● agent-child (here)  explorer  running"]),
        "{text}"
    );
    assert!(
        has(&test, &["│   └ ● agent-sib           general   running"]),
        "{text}"
    );
    assert_eq!(
        press(&mut test, KeyCode::Enter),
        Some(Action::OpenAgent("test".into()))
    );

    // Esc stops this sub-agent as `s` does, not a plain interrupt.
    let stop = press(&mut test, KeyCode::Esc).unwrap();
    assert_eq!(
        stop,
        Action::Interrupt {
            turn_id: "ct1".into()
        }
    );
    test.chat.act(test.ui.app_mut(), stop);
    let request = test.request().await;
    assert_eq!(request.path, "/v1/sessions/test/agents/child/stop");
    request.json(agent(
        "child",
        "explorer",
        "interrupted",
        json!({ "stoppedByUser": true }),
    ));
    test.message().await;
    test.message().await;
    let mut done = test.ui.app().snapshot().turns[0].clone();
    done.status = canter_protocol::TurnStatus::Interrupted;
    for payload in [
        EventPayload::TurnCompleted { turn: done },
        EventPayload::AgentStopped {
            turn_id: "ct1".into(),
        },
    ] {
        let event = Event {
            seq: test.ui.app().snapshot().last_event_seq + 1,
            session_id: "child".into(),
            timestamp: 0,
            payload,
        };
        test.chat
            .message(&mut test.ui, Message::Server(Box::new(event)));
    }
    test.ui.draw(0).unwrap();
    assert_eq!(
        rows(&test)[0],
        " Sub-agent · agent-child · explorer · stopped by you",
        "{}",
        shown(&test)
    );
    // Answer the question loaded in the main snapshot, then open/resolve
    // questions and plans through the already subscribed main SSE stream.
    let plan = json!({
        "id": "plan", "turnId": "t0", "createdAt": 0,
        "kind": { "type": "planApproval", "itemId": "submit", "plan": "Replace the parser",
            "approvalPolicy": "alwaysAsk" },
    });
    let mut seq = 13;
    for (request, resolution) in [
        (
            question.clone(),
            json!({"type":"userInput", "answer":{"type":"option","label":"Current"}}),
        ),
        (question, json!({"type":"cancelled"})),
        (
            plan.clone(),
            json!({"type":"plan","answer":{"decision":"implement","approvalPolicy":"alwaysAsk"}}),
        ),
        (
            plan.clone(),
            json!({"type":"plan","answer":{"decision":"keepPlanning","approvalPolicy":"alwaysAsk"}}),
        ),
        (plan, json!({"type":"cancelled"})),
    ] {
        if seq > 13 {
            events
                .send(frame(
                    "test",
                    seq,
                    json!({"type":"request.opened", "request":request}),
                ))
                .unwrap();
            seq += 1;
            test.message().await;
        }
        assert_eq!(rows(&test)[2].trim(), WAITING);
        assert!(!shown(&test).contains("Which parser?"));
        assert!(!shown(&test).contains("Replace the parser"));
        assert_eq!(
            test.ui.app_mut().handle_mouse(click),
            Some(Action::OpenAgent("test".into()))
        );
        events
            .send(frame(
                "test",
                seq,
                json!({"type":"request.resolved", "requestId":request["id"],
            "resolution":resolution, "clientName":"other"}),
            ))
            .unwrap();
        seq += 1;
        test.message().await;
        assert!(!shown(&test).contains(WAITING), "{}", shown(&test));
        assert_eq!(test.ui.app_mut().handle_mouse(click), None);
    }
    events.send(frame("test", seq, json!({"type":"request.opened", "request":approval("only-approval", "t0", "cargo check")}))).unwrap();
    test.message().await;
    assert!(shown(&test).contains("cargo check"));
    assert!(!shown(&test).contains(WAITING));
    if let Some(pump) = test.chat.main_pump.take() {
        pump.abort();
    }
    test.chat.pump.abort();
    test.server.abort();
}

#[tokio::test]
async fn returning_from_a_child_handoff_restores_its_main_session_file() {
    let mut test = Harness::new().await;
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("binding.json");
    let mut file =
        crate::session_file::SessionFile::new(path.clone(), files.path().join("state")).unwrap();
    file.publish(test.ui.app().snapshot()).unwrap();
    test.chat.session_file = Some(file);
    let mut parent = SessionSnapshot::empty("parent");
    parent.settings.cwd = "/original/project".into();
    let mut child = SessionSnapshot::empty("child");
    child.parent = Some(
        serde_json::from_value(json!({
            "sessionId":"parent", "turnId":"t0", "task":"Inspect code"
        }))
        .unwrap(),
    );
    let Harness {
        chat, ui, requests, ..
    } = &mut test;
    let (receiver, ()) = tokio::join!(chat.return_from_handoff(ui, "child"), async {
        loop {
            let request = next(requests).await;
            match request.path.as_str() {
                "/v1/sessions/child/resume" => request.json(json!({"session":child,"eventSeq":0})),
                "/v1/sessions/parent" => request.json(json!({"session":parent,"eventSeq":0})),
                "/v1/events" => {
                    let stream = futures_util::stream::pending::<Result<Bytes, io::Error>>();
                    assert!(
                        request
                            .reply
                            .send(Response::new(Body::from_stream(stream)))
                            .is_ok()
                    );
                }
                "/v1/models" => request.json(json!({"data":[],"nextCursor":null})),
                "/v1/sessions/child/skills" => request.json(json!({"data":[]})),
                "/v1/sessions/parent/agents" => {
                    request.json(json!({"data":[]}));
                    break;
                }
                path => panic!("unexpected request: {path}"),
            }
        }
    });
    test.messages = receiver.unwrap();
    let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(binding["sessionId"], "parent");
    assert_eq!(binding["cwd"], "/original/project");
    if let Some(pump) = test.chat.main_pump.take() {
        pump.abort();
    }
    test.chat.pump.abort();
    test.server.abort();
}

async fn next(
    requests: &mut mpsc::UnboundedReceiver<super::retract_tests::Request>,
) -> super::retract_tests::Request {
    tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap()
}

fn card(id: &str, tool: &str, status: &str, more: Value) -> Value {
    let mut item = json!({
        "type": "collabAgentToolCall", "id": id, "tool": tool, "status": status,
        "prompt": null, "agents": [], "error": null, "startedAt": 0, "durationMs": 5,
    });
    item.as_object_mut()
        .unwrap()
        .extend(more.as_object().unwrap().clone());
    item
}

fn collab(id: &str, kind: &str, status: Option<&str>, result: Option<&str>) -> Value {
    json!({ "id": id, "name": format!("agent-{id}"), "agentType": kind,
            "status": status, "result": result })
}

/// Whether `text` on the row that has it is drawn bold.
fn bold(test: &Harness, row_has: &str, text: &str) -> bool {
    let screen = rows(test);
    let Some(y) = screen.iter().position(|row| row.contains(row_has)) else {
        return false;
    };
    let x = screen[y]
        .find(text)
        .map(|at| screen[y][..at].chars().count());
    let buffer = test.ui.backend().buffer();
    x.is_some_and(|x| {
        buffer[(x as u16, y as u16)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    })
}

#[tokio::test]
async fn the_four_sub_agent_tools_have_their_own_cards() {
    let mut test = Harness::new().await;
    test.ui = Ui::new(
        TestBackend::new(90, 50),
        App::new(test.ui.app().snapshot().clone(), None, 0),
    )
    .unwrap();
    test.start("t1", "Use agents", None);
    let reply = "Locks are taken parent first\nline 2\nline 3\nline 4\nline 5";
    let items = [
        card(
            "s1",
            "spawnAgent",
            "completed",
            json!({
                "prompt": "Trace how locks are taken\nand report",
                "agents": [collab("a", "explorer", Some("running"), None)],
            }),
        ),
        card(
            "w1",
            "wait",
            "completed",
            json!({
                "durationMs": 31_000,
                "agents": [
                    collab("a", "explorer", Some("completed"), Some(reply)),
                    collab("b", "general", Some("running"), None),
                    collab("c", "general", Some("interrupted"), None),
                ],
            }),
        ),
        card(
            "i1",
            "sendInput",
            "completed",
            json!({
                "prompt": "Also check the lexer\nthanks",
                "agents": [collab("b", "general", Some("running"), None)],
            }),
        ),
        card(
            "c1",
            "closeAgent",
            "completed",
            json!({
                "agents": [collab("c", "general", Some("closed"), None)],
            }),
        ),
        card(
            "s2",
            "spawnAgent",
            "failed",
            json!({
                "error": "At most four agents may run at once",
                "agents": [{ "id": "", "name": "", "agentType": "general" }],
            }),
        ),
        json!({ "type": "backgroundEvent", "id": "agent-result-a-t2", "commandId": null,
                "text": "[Sub-agent a (agent-a) completed]\nLocks are taken parent first",
                "agent": collab("a", "explorer", Some("completed"), None) }),
        json!({ "type": "backgroundEvent", "id": "agent-result-b-t3", "commandId": null,
                "text": "[Sub-agent b (agent-b) interrupted]\nNo final reply.",
                "agent": collab("b", "general", Some("interrupted"), None) }),
    ];
    for item in items {
        test.event(payload(
            json!({ "type": "item.started", "turnId": "t1", "item": item }),
        ));
        test.event(payload(
            json!({ "type": "item.completed", "turnId": "t1", "item": item }),
        ));
    }
    let text = shown(&test);
    assert!(has(&test, &["• Spawned explorer · agent-a"]), "{text}");
    assert!(bold(&test, "Spawned explorer", "agent-a"), "{text}");
    assert!(has(&test, &["    └ Trace how locks are taken …"]), "{text}");
    assert!(
        has(&test, &["• Waiting for agent-a, agent-b, agent-c · 31s"]),
        "{text}"
    );
    assert!(has(&test, &["└ ✓ agent-a completed"]), "{text}");
    assert!(has(&test, &["┃ Locks are taken parent first"]), "{text}");
    assert!(has(&test, &["… 2 more lines · Ctrl+O"]), "{text}");
    assert!(!text.contains("line 5"), "{text}");
    assert!(has(&test, &["└ ● agent-b still running"]), "{text}");
    assert!(has(&test, &["└ ■ agent-c interrupted"]), "{text}");
    assert!(has(&test, &["• Sent to agent-b"]), "{text}");
    assert!(has(&test, &["    └ Also check the lexer …"]), "{text}");
    assert!(has(&test, &["• Closed agent-c"]), "{text}");
    assert!(has(&test, &["• Spawn general", "failed"]), "{text}");
    assert!(
        has(&test, &["┃ At most four agents may run at once"]),
        "{text}"
    );
    assert!(
        has(
            &test,
            &["• Sub-agent agent-a completed — result sent to the model"]
        ),
        "{text}"
    );
    assert!(
        has(
            &test,
            &["• Sub-agent agent-b interrupted — result sent to the model"]
        ),
        "{text}"
    );
    // Ctrl+O shows the whole reply.
    test.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        LATER,
    );
    test.ui.draw(0).unwrap();
    let text = shown(&test);
    assert!(has(&test, &["┃ line 5"]), "{text}");
    test.stop().await;
}
