//! Word-wise editing and Ctrl+A/E/K/U in the input box (P2-49).

use canter_protocol::{SessionSnapshot, UserInput};
use canter_tui::{Action, App};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app(draft: &str) -> App {
    let mut snapshot = SessionSnapshot::empty("word-edit-test");
    snapshot.settings.model = "deepseek-flash".into();
    let mut app = App::new(snapshot, None, 0);
    app.handle_paste(draft);
    app
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert_eq!(app.handle_key(KeyEvent::new(code, modifiers), 0), None);
}

/// The draft with `|` where the cursor is.
fn with_cursor(app: &mut App) -> String {
    press(app, KeyCode::Char('|'), KeyModifiers::NONE);
    match app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0) {
        Some(Action::Send(text)) => text,
        other => panic!("expected the draft to be sent: {other:?}"),
    }
}

/// `text` with the cursor `left` characters before its end.
fn draft_at(text: &str, left: usize) -> App {
    let mut app = app(text);
    for _ in 0..left {
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    }
    app
}

#[test]
fn alt_left_and_right_jump_a_word() {
    let mut app = app("foo bar_1 baz");
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(with_cursor(&mut app), "foo bar_1 |baz");

    let mut app = self::app("foo bar_1 baz");
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(with_cursor(&mut app), "foo |bar_1 baz");

    let mut app = self::app("foo bar_1 baz");
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(with_cursor(&mut app), "foo| bar_1 baz");
    let mut app = self::app("foo bar_1 baz");
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(with_cursor(&mut app), "foo bar_1| baz");
}

#[test]
fn ctrl_arrows_and_alt_b_f_jump_like_alt_arrows() {
    let text = "把 src/app.rs里的问候语改一下";
    let back = [
        (KeyCode::Left, KeyModifiers::ALT),
        (KeyCode::Left, KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::ALT),
    ];
    let results: Vec<String> = back
        .iter()
        .map(|&(code, modifiers)| {
            let mut app = app(text);
            press(&mut app, code, modifiers);
            press(&mut app, code, modifiers);
            with_cursor(&mut app)
        })
        .collect();
    assert_eq!(results, ["把 src/app.|rs里的问候语改一下"; 3]);

    let forward = [
        (KeyCode::Right, KeyModifiers::ALT),
        (KeyCode::Right, KeyModifiers::CONTROL),
        (KeyCode::Char('f'), KeyModifiers::ALT),
    ];
    let results: Vec<String> = forward
        .iter()
        .map(|&(code, modifiers)| {
            let mut app = app(text);
            press(&mut app, KeyCode::Home, KeyModifiers::NONE);
            press(&mut app, code, modifiers);
            press(&mut app, code, modifiers);
            with_cursor(&mut app)
        })
        .collect();
    assert_eq!(results, ["把 src|/app.rs里的问候语改一下"; 3]);
}

#[test]
fn alt_backspace_and_ctrl_w_delete_mixed_chinese_and_english_word_by_word() {
    let text = "把 src/app.rs里的问候语改一下";
    let steps = ["把 src/app.rs", "把 src/app.", "把 src/", "把 "];
    for (code, modifiers) in [
        (KeyCode::Backspace, KeyModifiers::ALT),
        (KeyCode::Char('w'), KeyModifiers::CONTROL),
    ] {
        for (presses, expected) in steps.iter().enumerate() {
            let mut app = app(text);
            for _ in 0..=presses {
                press(&mut app, code, modifiers);
            }
            assert_eq!(
                app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
                Some(Action::Send((*expected).into())),
                "{code:?} {modifiers:?} pressed {} times",
                presses + 1
            );
        }
        let mut app = app(text);
        for _ in 0..5 {
            press(&mut app, code, modifiers);
        }
        assert_eq!(with_cursor(&mut app), "|");
    }
}

#[test]
fn alt_delete_and_alt_d_delete_the_next_word() {
    for (code, modifiers) in [
        (KeyCode::Delete, KeyModifiers::ALT),
        (KeyCode::Char('d'), KeyModifiers::ALT),
    ] {
        // Cursor after "foo": skips the space, deletes "bar".
        let mut app = draft_at("foo bar.baz", 8);
        press(&mut app, code, modifiers);
        assert_eq!(with_cursor(&mut app), "foo|.baz", "{code:?}");
        let mut app = draft_at("foo bar.baz", 8);
        press(&mut app, code, modifiers);
        press(&mut app, code, modifiers);
        assert_eq!(with_cursor(&mut app), "foo|", "{code:?}");
    }
}

#[test]
fn ctrl_a_and_ctrl_e_go_to_the_start_and_end_of_the_line() {
    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first\n|second line\nthird");

    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first\nsecond line|\nthird");
}

#[test]
fn ctrl_k_deletes_to_the_end_of_the_line_and_at_the_end_joins_the_next() {
    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first\nsec|\nthird");

    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first\nsec|third");
}

#[test]
fn ctrl_u_deletes_to_the_start_of_the_line_and_at_the_start_joins_the_previous() {
    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first\n|ond line\nthird");

    let mut app = draft_at("first\nsecond line\nthird", 14);
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(with_cursor(&mut app), "first|ond line\nthird");
}

fn with_image(after: &str) -> App {
    let mut app = app("看看").with_clipboard_reader(|path| {
        std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
        Ok(true)
    });
    press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    app.handle_paste(after);
    app
}

#[test]
fn image_marker_is_one_word_and_its_attachment_goes_with_it() {
    let mut app = with_image(" ok");
    press(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
    press(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
    // Only the text is left; the image is no longer attached.
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("看看".into()))
    );

    // Jumping skips the whole marker in one step, both ways.
    let mut app = with_image(" ok");
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    press(&mut app, KeyCode::Char('|'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    press(&mut app, KeyCode::Char('|'), KeyModifiers::NONE);
    let Some(Action::SendImages { text, input }) =
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0)
    else {
        panic!("the image must still be attached");
    };
    assert_eq!(text, "看看|[Image #1 screenshot.png]| ok");
    assert!(matches!(&input[1], UserInput::LocalImage { .. }));

    // Next to the marker, deleting a word towards it takes it all (the
    // cursor no longer stops inside a marker, P2-50).
    for (code, lefts) in [(KeyCode::Backspace, 3), (KeyCode::Delete, 4)] {
        let mut app = with_image(" ok");
        for _ in 0..lefts {
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        }
        press(&mut app, code, KeyModifiers::ALT);
        assert_eq!(with_cursor(&mut app), "看看| ok", "{code:?}");
    }
}
