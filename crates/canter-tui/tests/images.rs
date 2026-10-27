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
fn dragged_screenshot_path_preserves_narrow_no_break_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("Screenshot at 10.00.00\u{202f}PM.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let mut app = app();
    app.handle_paste(&path.to_str().unwrap().replace(' ', "\\ "));
    let sent = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0);
    let Some(Action::SendImages { input, .. }) = sent else {
        panic!("dragged screenshot became text: {sent:?}");
    };
    assert!(
        matches!(&input[0], UserInput::LocalImage { path: attached, .. } if std::path::Path::new(attached) == path)
    );
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

fn enter(app: &mut App) -> Option<Action> {
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0)
}

fn attached_paths(input: &[UserInput]) -> Vec<std::path::PathBuf> {
    input
        .iter()
        .filter_map(|part| match part {
            UserInput::LocalImage { path, .. } => Some(path.into()),
            _ => None,
        })
        .collect()
}

#[test]
fn one_paste_of_several_image_paths_attaches_them_all_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a picture.png");
    let second = temp.path().join("b shot.jpg");
    let third = temp.path().join("c.gif");
    std::fs::write(&first, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&second, b"\xff\xd8\xff\xe0").unwrap();
    std::fs::write(&third, b"GIF89a").unwrap();
    let [a, b, c] = [&first, &second, &third].map(|path| path.to_str().unwrap().to_owned());
    for pasted in [
        format!("'{a}' {} \"{c}\"", b.replace(' ', "\\ ")),
        format!("{}  {}\n", a.replace(' ', "\\ "), b.replace(' ', "\\ ")),
    ] {
        let mut app = app();
        app.handle_paste(&pasted);
        let action = enter(&mut app);
        let Some(Action::SendImages { text, input }) = action else {
            panic!("{pasted}: {action:?}");
        };
        if pasted.contains("c.gif") {
            assert_eq!(
                text,
                "[Image #1 a picture.png] [Image #2 b shot.jpg] [Image #3 c.gif]"
            );
            assert_eq!(attached_paths(&input), [first.as_path(), &second, &third]);
        } else {
            assert_eq!(text, "[Image #1 a picture.png] [Image #2 b shot.jpg]");
            assert_eq!(attached_paths(&input), [first.as_path(), &second]);
        }
    }
}

#[test]
fn a_paste_with_any_non_image_word_stays_text_and_attaches_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("a picture.png");
    let text_file = temp.path().join("notes.txt");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&text_file, b"ordinary text").unwrap();
    let image = image.to_str().unwrap().replace(' ', "\\ ");
    let text_file = text_file.to_str().unwrap();
    for pasted in [
        format!("{image} {text_file}"),
        format!("{image} /no/such/image.png"),
        format!("{image} look at this"),
        format!("{image} '{text_file}"),
    ] {
        let mut app = app();
        app.handle_paste(&pasted);
        assert_eq!(enter(&mut app), Some(Action::Send(pasted.clone())));
    }
}

#[test]
fn pasting_several_images_twice_adds_to_the_earlier_ones_at_the_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let paths: Vec<_> = ["1.png", "2.png", "3.png", "4.png"]
        .iter()
        .map(|name| {
            let path = temp.path().join(name);
            std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
            path
        })
        .collect();
    let pair = |a: &std::path::Path, b: &std::path::Path| {
        format!("{} {}", a.to_str().unwrap(), b.to_str().unwrap())
    };
    let mut app = app();
    app.handle_paste(&pair(&paths[0], &paths[1]));
    app.handle_paste(" and");
    app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), 0);
    app.handle_paste(&pair(&paths[2], &paths[3]));
    let action = enter(&mut app);
    let Some(Action::SendImages { text, input }) = action else {
        panic!("{action:?}");
    };
    assert_eq!(
        text,
        "[Image #3 3.png] [Image #4 4.png][Image #1 1.png] [Image #2 2.png] and"
    );
    assert_eq!(
        attached_paths(&input),
        [paths[2].as_path(), &paths[3], &paths[0], &paths[1]]
    );
}

#[test]
fn text_models_warn_once_for_several_dropped_images_and_attach_none() {
    let temp = tempfile::tempdir().unwrap();
    let paths: Vec<_> = ["1.png", "2.png", "3.png"]
        .iter()
        .map(|name| {
            let path = temp.path().join(name);
            std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
            path.to_str().unwrap().to_owned()
        })
        .collect();
    let mut snapshot = SessionSnapshot::empty("image-test");
    snapshot.settings.model = "deepseek-v4-pro".into();
    let mut app = App::new(snapshot, None, 0);
    app.handle_paste(&paths.join(" "));
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
    assert_eq!(rendered.matches("does not accept images").count(), 1);
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::Send("text draft".into()))
    );
}
