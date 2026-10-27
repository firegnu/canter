//! ↑ and ↓ bring back what was sent from the input box in this directory,
//! also in a new session (P2-25).

use std::collections::BTreeMap;
use std::path::Path;

use canter_protocol::{SessionSnapshot, SessionStatus, UserInput};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

/// `CANTER_HOME` and `HOME` both in `home`: nothing of the user's is read.
fn env(home: &Path) -> BTreeMap<String, String> {
    let home = home.to_string_lossy().into_owned();
    BTreeMap::from([
        ("CANTER_HOME".to_owned(), home.clone()),
        ("HOME".to_owned(), home),
    ])
}

/// A screen in `cwd` reading the history in `home`.
fn ui(home: &Path, cwd: &str) -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("input-history-test");
    snapshot.settings.model = "deepseek-flash".into();
    snapshot.settings.cwd = cwd.into();
    let app = App::new(snapshot, None, 0)
        .with_clipboard_reader(|path| {
            std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
            Ok(true)
        })
        .with_input_history(&env(home));
    let mut ui = Ui::new(TestBackend::new(80, 20), app).unwrap();
    ui.draw(0).unwrap();
    ui
}

fn press(ui: &mut Ui<TestBackend>, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    let action = ui.app_mut().handle_key(KeyEvent::new(code, modifiers), 0);
    ui.draw(0).unwrap();
    action
}

fn key(ui: &mut Ui<TestBackend>, code: KeyCode) {
    assert_eq!(press(ui, code, KeyModifiers::NONE), None);
}

fn up(ui: &mut Ui<TestBackend>) {
    key(ui, KeyCode::Up);
}

fn down(ui: &mut Ui<TestBackend>) {
    key(ui, KeyCode::Down);
}

fn enter(ui: &mut Ui<TestBackend>) -> Option<Action> {
    press(ui, KeyCode::Enter, KeyModifiers::NONE)
}

/// Types `text` and sends it.
fn send(ui: &mut Ui<TestBackend>, text: &str) {
    ui.app_mut().handle_paste(text);
    assert_eq!(enter(ui), Some(Action::Send(text.into())));
}

/// The draft with `|` where the cursor is; sends it.
fn with_cursor(ui: &mut Ui<TestBackend>) -> String {
    key(ui, KeyCode::Char('|'));
    match enter(ui) {
        Some(Action::Send(text)) => text,
        other => panic!("expected the draft to be sent: {other:?}"),
    }
}

fn screen(ui: &mut Ui<TestBackend>) -> String {
    ui.draw(0).unwrap();
    let buffer = ui.backend().buffer();
    let width = usize::from(buffer.area.width);
    buffer
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The input box's first row, after `› `; empty for the placeholder.
fn input_row(ui: &mut Ui<TestBackend>) -> String {
    const PLACEHOLDER: &str = "Type a message (Enter to send, Ctrl+J for a new line)";
    let shown = screen(ui);
    let rows: Vec<&str> = shown.lines().collect();
    // Below the box's top border, not in a list above it.
    let top = rows
        .iter()
        .rposition(|row| row.trim_start().starts_with('╭'));
    let row = rows[top.map_or(0, |top| top + 1)..]
        .iter()
        .find_map(|row| row.split_once("› ").map(|(_, rest)| rest.to_owned()))
        .unwrap_or_else(|| panic!("no input box: {shown}"));
    let row = row.trim_end_matches([' ', '│']);
    if row == PLACEHOLDER { "" } else { row }.to_owned()
}

#[test]
fn up_in_the_empty_box_goes_back_and_down_comes_forward_to_an_empty_box() {
    let home = tempfile::tempdir().unwrap();
    let mut ui = ui(home.path(), "/work");
    send(&mut ui, "first");
    send(&mut ui, "second");
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "second");
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "first");
    // At the oldest it stays.
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "first");
    down(&mut ui);
    assert_eq!(input_row(&mut ui), "second");
    down(&mut ui);
    assert_eq!(input_row(&mut ui), "");
    // Up again starts from the newest.
    up(&mut ui);
    assert_eq!(with_cursor(&mut ui), "second|");
}

#[test]
fn multi_line_recalled_moves_through_history_at_its_ends_only() {
    let home = tempfile::tempdir().unwrap();
    let mut ui = ui(home.path(), "/work");
    send(&mut ui, "older");
    send(&mut ui, "one\ntwo");
    up(&mut ui);
    // Recalled with the cursor at the end: ↑ goes on to the one before.
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "older");
    down(&mut ui);
    // The cursor moved inside: ↑ moves it between the lines.
    key(&mut ui, KeyCode::Left);
    up(&mut ui);
    assert_eq!(with_cursor(&mut ui), "on|e\ntwo");
}

#[test]
fn a_draft_is_not_replaced_and_a_changed_recall_moves_the_cursor() {
    let home = tempfile::tempdir().unwrap();
    let mut ui = ui(home.path(), "/work");
    send(&mut ui, "first");
    send(&mut ui, "a\nb");
    ui.app_mut().handle_paste("draft\nline");
    up(&mut ui);
    assert_eq!(with_cursor(&mut ui), "draf|t\nline");

    // Changed after recalling, ↑ and ↓ move the cursor.
    up(&mut ui);
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "a");
    key(&mut ui, KeyCode::Char('!'));
    up(&mut ui);
    assert_eq!(with_cursor(&mut ui), "a|\nb!");
}

#[test]
fn lists_search_and_boxes_keep_their_own_keys() {
    let home = tempfile::tempdir().unwrap();
    let mut ui = ui(home.path(), "/work");
    send(&mut ui, "hello");

    // `?` opens the help: ↑ scrolls it, the box stays empty.
    key(&mut ui, KeyCode::Char('?'));
    up(&mut ui);
    key(&mut ui, KeyCode::Esc);
    assert_eq!(input_row(&mut ui), "");

    // Find: ↑ goes to an older match.
    press(&mut ui, KeyCode::Char('f'), KeyModifiers::CONTROL);
    up(&mut ui);
    key(&mut ui, KeyCode::Esc);
    assert_eq!(input_row(&mut ui), "");

    // A recalled command, once changed, opens its list; ↑ selects in it.
    ui.app_mut().handle_paste("/help");
    assert_eq!(enter(&mut ui), None);
    key(&mut ui, KeyCode::Esc);
    up(&mut ui);
    key(&mut ui, KeyCode::Backspace);
    let shown = screen(&mut ui);
    assert!(shown.contains("Show available commands"), "{shown}");
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "/hel");
}

#[test]
fn up_goes_on_past_recalled_commands_without_their_list() {
    let home = tempfile::tempdir().unwrap();
    let mut ui = ui(home.path(), "/work");
    let command = |ui: &mut Ui<TestBackend>, text: &str| {
        ui.app_mut().handle_paste(text);
        assert_eq!(enter(ui), None);
        key(ui, KeyCode::Esc);
    };
    send(&mut ui, "first");
    command(&mut ui, "/help");
    send(&mut ui, "second");
    command(&mut ui, "/statusline");

    let mut seen = Vec::new();
    for _ in 0..4 {
        up(&mut ui);
        let shown = screen(&mut ui);
        assert!(!shown.contains("Show available commands"), "{shown}");
        assert!(
            !shown.contains("Choose and order what the status line shows"),
            "{shown}"
        );
        seen.push(input_row(&mut ui));
    }
    assert_eq!(seen, ["/statusline", "second", "/help", "first"]);
    down(&mut ui);
    assert_eq!(input_row(&mut ui), "/help");
    down(&mut ui);
    assert_eq!(input_row(&mut ui), "second");

    // Enter on a recalled command runs it.
    up(&mut ui);
    assert_eq!(enter(&mut ui), None);
    let shown = screen(&mut ui);
    assert!(shown.contains("Send, or run the command"), "{shown}");
    assert_eq!(input_row(&mut ui), "");
}

#[test]
fn queued_messages_and_commands_are_kept() {
    let home = tempfile::tempdir().unwrap();
    let mut snapshot = SessionSnapshot::empty("input-history-test");
    snapshot.settings.model = "deepseek-flash".into();
    snapshot.settings.cwd = "/work".into();
    snapshot.status = SessionStatus::Working;
    let app = App::new(snapshot, None, 0).with_input_history(&env(home.path()));
    let mut ui = Ui::new(TestBackend::new(80, 20), app).unwrap();
    ui.draw(0).unwrap();
    send(&mut ui, "while working");
    ui.app_mut().handle_paste("/help");
    assert_eq!(enter(&mut ui), None);
    key(&mut ui, KeyCode::Esc);

    let mut ui = self::ui(home.path(), "/work");
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "/help");
    key(&mut ui, KeyCode::Esc);
    up(&mut ui);
    assert_eq!(input_row(&mut ui), "while working");
}

#[test]
fn a_new_session_recalls_only_what_was_sent_in_its_directory() {
    let home = tempfile::tempdir().unwrap();
    let mut a = ui(home.path(), "/work/a");
    send(&mut a, "in a");
    let mut b = ui(home.path(), "/work/b");
    send(&mut b, "in b");

    let mut again = ui(home.path(), "/work/a");
    up(&mut again);
    assert_eq!(input_row(&mut again), "in a");
    up(&mut again);
    assert_eq!(with_cursor(&mut again), "in a|");

    let mut elsewhere = ui(home.path(), "/work/c");
    up(&mut elsewhere);
    assert_eq!(input_row(&mut elsewhere), "");
}

#[test]
fn pastes_and_images_come_back_in_this_terminal_and_as_text_from_the_file() {
    let home = tempfile::tempdir().unwrap();
    let long = (1..=11)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ui = ui(home.path(), "/work");
    press(&mut ui, KeyCode::Char('v'), KeyModifiers::CONTROL);
    ui.app_mut().handle_paste(" look ");
    ui.app_mut().handle_paste(&long);
    let Some(Action::SendImages { text, input }) = enter(&mut ui) else {
        panic!("expected an image send");
    };
    assert_eq!(text, format!("[Image #1 screenshot.png] look {long}"));
    ui.app_mut().sent();

    // In this terminal, as it was.
    up(&mut ui);
    assert_eq!(
        input_row(&mut ui),
        "[Image #1 screenshot.png] look [Pasted #1 · 11 lines]"
    );
    let Some(Action::SendImages {
        text: again,
        input: parts,
    }) = enter(&mut ui)
    else {
        panic!("the image must come back");
    };
    assert_eq!(again, text);
    assert_eq!(parts, input);
    ui.app_mut().sent();

    // Also on the screen that replaces this one, such as after /new.
    let mut snapshot = SessionSnapshot::empty("input-history-next");
    snapshot.settings.model = "deepseek-flash".into();
    snapshot.settings.cwd = "/work".into();
    let next = App::new(snapshot, None, 0).with_input_history_from(ui.app_mut());
    let mut next = Ui::new(TestBackend::new(80, 20), next).unwrap();
    next.draw(0).unwrap();
    up(&mut next);
    assert!(matches!(
        enter(&mut next),
        Some(Action::SendImages { input: parts, .. })
            if parts.iter().any(|part| matches!(part, UserInput::LocalImage { .. }))
    ));

    // From the file: the paste in full, no image.
    let mut later = self::ui(home.path(), "/work");
    up(&mut later);
    let shown = screen(&mut later);
    assert!(
        !shown.contains("[Image") && !shown.contains("[Pasted"),
        "{shown}"
    );
    assert!(shown.contains("line 11"), "{shown}");
    assert_eq!(
        enter(&mut later),
        Some(Action::Send(format!(" look {long}")))
    );
}

#[test]
fn a_damaged_file_is_said_once_and_the_rest_still_comes_back() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("input_history.jsonl"),
        "{\"cwd\":\"/work\",\"text\":\"kept\",\"ts\":1}\n{oops\n",
    )
    .unwrap();
    let mut ui = ui(home.path(), "/work");
    let shown = screen(&mut ui);
    assert!(
        shown.contains("Skipped damaged lines in the input history"),
        "{shown}"
    );
    up(&mut ui);
    assert_eq!(with_cursor(&mut ui), "kept|");
}
