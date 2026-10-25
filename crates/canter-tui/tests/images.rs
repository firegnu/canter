use canter_protocol::{SessionSnapshot, UserInput};
use canter_tui::{Action, App};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app() -> App {
    let mut snapshot = SessionSnapshot::empty("image-test");
    snapshot.settings.model = "deepseek-flash".into();
    App::new(snapshot, None, 0)
}

#[test]
fn ctrl_v_attaches_clipboard_image_and_no_image_keeps_the_draft() {
    let mut app = app().with_clipboard_reader(|path| {
        std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
        Ok(true)
    });
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
    app.handle_paste("这是什么");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
    let Some(Action::SendImages { text, input }) = action else {
        panic!("Ctrl+V must attach an image: {action:?}");
    };
    assert_eq!(text, "[Image #1 screenshot.png]这是什么");
    assert!(
        matches!(&input[0], UserInput::LocalImage { name: Some(name), .. } if name == "screenshot.png")
    );

    let mut app = self::app().with_clipboard_reader(|_| Ok(false));
    app.handle_paste("unchanged draft");
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("unchanged draft".into()))
    );
}

#[test]
fn image_path_pastes_accept_quotes_escapes_and_spaces_and_other_pastes_stay_text() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("a picture.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let path = path.to_str().unwrap();
    for pasted in [
        format!("'{path}' "),
        format!("\"{path}\""),
        path.replace(' ', "\\ "),
        path.into(),
    ] {
        let mut app = app();
        app.handle_paste(&pasted);
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
        assert!(
            matches!(action, Some(Action::SendImages { .. })),
            "{pasted}: {action:?}"
        );
    }
    std::fs::write(path, b"ordinary text").unwrap();
    for pasted in [path, "/no/such/image.png"] {
        let mut app = app();
        app.handle_paste(pasted);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            Some(Action::Send(pasted.into()))
        );
    }
}

#[test]
fn deleting_the_placeholder_removes_the_image_and_text_models_refuse_attachments() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("image.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let mut app = app();
    app.handle_paste(path.to_str().unwrap());
    for _ in 0.."[Image #1 image.png]".len() {
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), 0);
    }
    app.handle_paste("only text");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("only text".into()))
    );

    let mut snapshot = SessionSnapshot::empty("image-test");
    snapshot.settings.model = "deepseek-v4-pro".into();
    let mut app = App::new(snapshot, None, 0);
    app.handle_paste(path.to_str().unwrap());
    app.handle_paste("text draft");
    let mut ui = canter_tui::Ui::new(ratatui::backend::TestBackend::new(110, 15), app).unwrap();
    ui.draw(0).unwrap();
    let rendered = ui
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        rendered
            .contains("deepseek-v4-pro does not accept images; switch with /model deepseek-flash")
    );
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("text draft".into()))
    );
}
