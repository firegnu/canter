//! A large paste folds into one `[Pasted #N …]` marker (P2-50).

use canter_protocol::{SessionSnapshot, UserInput};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

fn ui() -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("paste-fold-test");
    snapshot.settings.model = "deepseek-flash".into();
    let app = App::new(snapshot, None, 0).with_clipboard_reader(|path| {
        std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
        Ok(true)
    });
    let mut ui = Ui::new(TestBackend::new(60, 20), app).unwrap();
    ui.draw(0).unwrap();
    ui
}

fn lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
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

fn press(ui: &mut Ui<TestBackend>, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    ui.app_mut().handle_key(KeyEvent::new(code, modifiers), 0)
}

fn key(ui: &mut Ui<TestBackend>, code: KeyCode) {
    assert_eq!(press(ui, code, KeyModifiers::NONE), None);
}

fn enter(ui: &mut Ui<TestBackend>) -> Option<Action> {
    press(ui, KeyCode::Enter, KeyModifiers::NONE)
}

/// Types `|` where the cursor is and sends the draft.
fn with_cursor(ui: &mut Ui<TestBackend>) -> String {
    key(ui, KeyCode::Char('|'));
    match enter(ui) {
        Some(Action::Send(text)) => text,
        other => panic!("expected the draft to be sent: {other:?}"),
    }
}

/// `a`, a folded 11-line paste and `b`, the cursor at the end.
fn folded_between() -> Ui<TestBackend> {
    let mut ui = ui();
    ui.app_mut().handle_paste("a");
    ui.app_mut().handle_paste(&lines(11));
    ui.app_mut().handle_paste("b");
    ui
}

#[test]
fn eleven_lines_fold_and_ten_do_not() {
    let mut ui = ui();
    ui.app_mut().handle_paste(&lines(11));
    let shown = screen(&mut ui);
    assert!(shown.contains("[Pasted #1 · 11 lines]"), "{shown}");
    assert!(!shown.contains("line 5"), "{shown}");
    assert_eq!(enter(&mut ui), Some(Action::Send(lines(11))));

    let mut ui = self::ui();
    ui.app_mut().handle_paste(&lines(10));
    let shown = screen(&mut ui);
    assert!(!shown.contains("[Pasted"), "{shown}");
    assert!(shown.contains("line 5"), "{shown}");
    assert_eq!(enter(&mut ui), Some(Action::Send(lines(10))));
}

#[test]
fn one_line_over_1000_characters_folds_as_chars() {
    let mut ui = ui();
    let long = "a".repeat(1001);
    ui.app_mut().handle_paste(&long);
    let shown = screen(&mut ui);
    assert!(shown.contains("[Pasted #1 · 1001 chars]"), "{shown}");
    assert_eq!(enter(&mut ui), Some(Action::Send(long)));

    // Characters count, not bytes.
    for text in ["a".repeat(1000), "汉".repeat(400)] {
        let mut ui = self::ui();
        ui.app_mut().handle_paste(&text);
        let shown = screen(&mut ui);
        assert!(!shown.contains("[Pasted"), "{shown}");
        assert_eq!(enter(&mut ui), Some(Action::Send(text)));
    }
}

#[test]
fn sent_parts_are_the_full_text_cleaned_like_a_plain_paste() {
    let mut ui = ui();
    ui.app_mut().handle_paste("before ");
    ui.app_mut()
        .handle_paste(&format!("{}\r\n\tlast", lines(10)));
    ui.app_mut().handle_paste(" after");
    assert_eq!(
        enter(&mut ui),
        Some(Action::Send(format!(
            "before {}\n    last after",
            lines(10)
        )))
    );

    // Next to an image the full text goes as text and the image as ever.
    let mut ui = self::ui();
    press(&mut ui, KeyCode::Char('v'), KeyModifiers::CONTROL);
    ui.app_mut().handle_paste(&lines(11));
    let Some(Action::SendImages { text, input }) = enter(&mut ui) else {
        panic!("the image must be attached");
    };
    assert_eq!(text, format!("[Image #1 screenshot.png]{}", lines(11)));
    assert!(matches!(&input[0], UserInput::LocalImage { .. }));
    assert_eq!(input[1], UserInput::Text { text: lines(11) });
    assert_eq!(input.len(), 2);
}

#[test]
fn two_pastes_are_numbered_one_and_two() {
    let mut ui = ui();
    ui.app_mut().handle_paste(&lines(11));
    ui.app_mut().handle_paste(" and ");
    ui.app_mut().handle_paste(&lines(12));
    let shown = screen(&mut ui);
    assert!(
        shown.contains("[Pasted #1 · 11 lines] and [Pasted #2 · 12 lines]"),
        "{shown}"
    );
    assert_eq!(
        enter(&mut ui),
        Some(Action::Send(format!("{} and {}", lines(11), lines(12))))
    );
}

#[test]
fn backspace_and_delete_remove_a_whole_paste_marker_and_its_text() {
    let mut ui = folded_between();
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Backspace);
    assert_eq!(with_cursor(&mut ui), "a|b");

    let mut ui = folded_between();
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Delete);
    assert_eq!(with_cursor(&mut ui), "a|b");
}

/// `看看`, an image marker and ` ok`.
fn with_image() -> Ui<TestBackend> {
    let mut ui = ui();
    ui.app_mut().handle_paste("看看");
    press(&mut ui, KeyCode::Char('v'), KeyModifiers::CONTROL);
    ui.app_mut().handle_paste(" ok");
    ui
}

#[test]
fn backspace_and_delete_remove_a_whole_image_marker_and_its_attachment() {
    let mut ui = with_image();
    for _ in 0..3 {
        key(&mut ui, KeyCode::Left);
    }
    key(&mut ui, KeyCode::Backspace);
    assert_eq!(with_cursor(&mut ui), "看看| ok");

    let mut ui = with_image();
    for _ in 0..4 {
        key(&mut ui, KeyCode::Left);
    }
    key(&mut ui, KeyCode::Delete);
    assert_eq!(with_cursor(&mut ui), "看看| ok");
}

#[test]
fn left_and_right_skip_a_marker_in_one_step() {
    let mut ui = folded_between();
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Left);
    assert_eq!(with_cursor(&mut ui), format!("a|{}b", lines(11)));

    let mut ui = folded_between();
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Right);
    assert_eq!(with_cursor(&mut ui), format!("a{}|b", lines(11)));

    let mut ui = with_image();
    for _ in 0..4 {
        key(&mut ui, KeyCode::Left);
    }
    key(&mut ui, KeyCode::Char('|'));
    key(&mut ui, KeyCode::Right);
    key(&mut ui, KeyCode::Char('|'));
    let Some(Action::SendImages { text, .. }) = enter(&mut ui) else {
        panic!("the image must still be attached");
    };
    assert_eq!(text, "看看|[Image #1 screenshot.png]| ok");
}

#[test]
fn up_and_down_never_land_inside_a_marker() {
    // Down from the end of a short first line lands in the marker's columns.
    let mut ui = ui();
    ui.app_mut().handle_paste("first line here\nx");
    ui.app_mut().handle_paste(&lines(11));
    ui.app_mut().handle_paste("y");
    key(&mut ui, KeyCode::Up);
    key(&mut ui, KeyCode::Down);
    assert_eq!(
        with_cursor(&mut ui),
        format!("first line here\nx{}|y", lines(11))
    );

    // Up from a longer second line lands in the marker's columns.
    let mut ui = self::ui();
    ui.app_mut().handle_paste("x");
    ui.app_mut().handle_paste(&lines(11));
    ui.app_mut().handle_paste("\nlonger second line");
    key(&mut ui, KeyCode::Up);
    assert_eq!(
        with_cursor(&mut ui),
        format!("x{}|\nlonger second line", lines(11))
    );
}

#[test]
fn a_draft_back_from_a_failed_send_stays_folded() {
    let mut ui = ui();
    press(&mut ui, KeyCode::Char('v'), KeyModifiers::CONTROL);
    ui.app_mut().handle_paste(&lines(11));
    assert!(matches!(enter(&mut ui), Some(Action::SendImages { .. })));
    ui.app_mut().image_send_failed("offline");
    let shown = screen(&mut ui);
    assert!(
        shown.contains("[Image #1 screenshot.png][Pasted #1 · 11 lines]"),
        "{shown}"
    );
    let Some(Action::SendImages { input, .. }) = enter(&mut ui) else {
        panic!("the image must still be attached");
    };
    assert_eq!(input[1], UserInput::Text { text: lines(11) });
}
