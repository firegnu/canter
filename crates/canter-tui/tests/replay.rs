//! Replays the demo session from the fake server, through the client, onto
//! ratatui's test backend, and compares the screens with
//! `tests/expected/replay.txt`. Set `CANTER_UPDATE_EXPECTED=1` to rewrite that
//! file after an intended change, then read the diff.

use std::time::Duration;

use canter_protocol::{InputMode, SessionInputParams, SessionStatus, TurnStatus, UserInput};
use canter_tui::client::{Client, EventStream};
use canter_tui::fake::{DEMO_SCRIPT, FakeServer, Pace, parse_script};
use canter_tui::{Action, App, CLIENT_NAME, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;
/// A fixed clock keeps the spinner and elapsed time out of the comparison.
const NOW: i64 = 0;
/// Mid-stream moments worth checking: thinking, a command starting to print
/// (with colour codes), its output cut to the last lines, and the last message
/// half written.
const SNAPSHOT_AT: [u64; 4] = [9, 21, 23, 29];

#[tokio::test]
async fn replayed_session_matches_expected_screens() {
    tokio::time::timeout(Duration::from_secs(30), replay())
        .await
        .expect("replay finished in time");
}

async fn replay() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("canter.sock");
    let script = parse_script(DEMO_SCRIPT).expect("script parses");
    let session = script[0].session_id.clone();
    let last = script.len() as u64;
    let _server = FakeServer::start(&socket, script, 2, Pace::Instant)
        .await
        .expect("fake server starts");

    let client = Client::new(&socket).expect("client");
    let snapshot = client
        .read_session(&session)
        .await
        .expect("snapshot")
        .session;
    assert_eq!(
        snapshot.last_event_seq, 2,
        "the snapshot holds the preloaded events"
    );
    let mut events = client
        .subscribe(&session, snapshot.last_event_seq)
        .await
        .expect("subscribe");
    let app = App::new(snapshot, Some("/home/dev".into()), NOW);
    let mut ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).expect("ui");
    ui.draw(NOW).unwrap();

    let mut report = String::new();
    while ui.app().snapshot().last_event_seq < last {
        step(&mut ui, &mut events).await;
        let seq = ui.app().snapshot().last_event_seq;
        if SNAPSHOT_AT.contains(&seq) {
            report += &format!("== 第 {seq} 条事件后，屏幕 ==\n");
            report += &rows(ui.backend().buffer());
        }
    }

    // Type a message and send it; after the replay the fake server answers.
    for c in "你好".chars() {
        assert_eq!(ui.app_mut().handle_key(key(KeyCode::Char(c))), None);
    }
    let Some(Action::Send(text)) = ui.app_mut().handle_key(key(KeyCode::Enter)) else {
        panic!("Enter sends the input");
    };
    let params = SessionInputParams {
        input: vec![UserInput::Text { text }],
        mode: InputMode::Queue,
        client_name: CLIENT_NAME.into(),
    };
    client.send_input(&session, &params).await.expect("send");
    loop {
        step(&mut ui, &mut events).await;
        let snapshot = ui.app().snapshot();
        let answered = snapshot
            .turns
            .get(1)
            .is_some_and(|turn| turn.status == TurnStatus::Completed);
        if answered && snapshot.status == SessionStatus::Idle {
            break;
        }
    }
    report += "== 发了「你好」之后，滚动区 ==\n";
    report += &rows(ui.backend().scrollback());
    report += "== 发了「你好」之后，屏幕 ==\n";
    report += &rows(ui.backend().buffer());

    let expected_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/expected/replay.txt");
    if std::env::var_os("CANTER_UPDATE_EXPECTED").is_some() {
        std::fs::write(expected_path, &report).expect("write expected screens");
    }
    let expected = std::fs::read_to_string(expected_path).expect("expected screens");
    assert!(
        report == expected,
        "screens differ from {expected_path}\n--- actual ---\n{report}"
    );

    // Colors the text above cannot show: diff lines, and grey thinking.
    let scrollback = ui.backend().scrollback();
    let added = find(scrollback, "+ cargo install --path crates/canter");
    assert_eq!(scrollback[added].fg, Color::Green, "added lines are green");
    let removed = find(scrollback, "- git clone https://github.com/firegnu/canter");
    assert_eq!(scrollback[removed].fg, Color::Red, "removed lines are red");
    let thinking = find(scrollback, "思考：");
    assert!(
        scrollback[thinking].modifier.contains(Modifier::DIM),
        "thinking is grey"
    );
}

async fn step(ui: &mut Ui<TestBackend>, events: &mut EventStream) {
    let event = events
        .next()
        .await
        .expect("stream stays open")
        .expect("event decodes");
    ui.app_mut().apply_event(&event, NOW);
    ui.draw(NOW).unwrap();
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The buffer as text, one row per line, trailing spaces trimmed. Cells
/// hidden under a wide character are skipped.
fn rows(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        out += row(buffer, y).trim_end();
        out.push('\n');
    }
    out
}

/// Position of the first cell of `text` in the buffer.
fn find(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            return (row[..byte].width() as u16, y);
        }
    }
    panic!("{text:?} is not in the buffer");
}

fn row(buffer: &Buffer, y: u16) -> String {
    let mut row = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += symbol.width().max(1) as u16;
    }
    row
}
