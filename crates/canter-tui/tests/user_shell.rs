use canter_protocol::{SessionSnapshot, SessionStatus};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

fn screen(status: SessionStatus) -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("user-shell");
    snapshot.status = status;
    let mut ui = Ui::new(TestBackend::new(100, 24), App::new(snapshot, None, 0)).unwrap();
    ui.draw(0).unwrap();
    ui
}

fn text(ui: &mut Ui<TestBackend>) -> String {
    ui.draw(0).unwrap();
    ui.backend()
        .buffer()
        .content
        .chunks(100)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn user_shell_busy_keeps_draft_and_bare_bang_is_not_sent() {
    for status in [
        SessionStatus::Working,
        SessionStatus::WaitingApproval,
        SessionStatus::Idle,
    ] {
        let mut ui = screen(status);
        let draft = if status == SessionStatus::Idle {
            "!  "
        } else {
            "!git status"
        };
        ui.app_mut().handle_paste(draft);
        ui.draw(0).unwrap();
        assert_eq!(
            ui.app_mut()
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            None
        );
        assert!(text(&mut ui).contains(draft.trim_end()));
        if status != SessionStatus::Idle {
            assert!(text(&mut ui).contains("Wait for this turn to finish"));
        }
    }
}

#[test]
fn user_shell_export_labels_the_user_command() {
    let directory = tempfile::tempdir().unwrap();
    let mut snapshot = SessionSnapshot::empty("user-shell");
    snapshot.settings.cwd = directory.path().to_string_lossy().into_owned();
    snapshot.turns.push(
        serde_json::from_value(serde_json::json!({
            "id":"shell-turn", "status":"completed", "input":[], "clientName":"userShell",
            "error":null, "usage":null, "items":[{
                "type":"commandExecution", "id":"shell-item", "command":"git status",
                "source":"userShell", "cwd":"/work", "status":"completed",
                "aggregatedOutput":"On branch main", "exitCode":0, "durationMs":10
            }]
        }))
        .unwrap(),
    );
    let mut app = App::new(snapshot, None, 0);
    app.export(canter_tui::Export::File(Some("conversation.md".into())), 0);
    let exported = std::fs::read_to_string(directory.path().join("conversation.md")).unwrap();
    assert!(exported.contains("## User\n"));
    assert!(exported.contains("User ran `git status`"), "{exported}");
    let mut ui = Ui::new(TestBackend::new(100, 24), app).unwrap();
    assert!(text(&mut ui).contains("! git status"));
}

#[test]
fn user_shell_enter_trims_command_and_records_input_history() {
    let home = tempfile::tempdir().unwrap();
    let env = std::collections::BTreeMap::from([
        ("HOME".into(), home.path().to_string_lossy().into_owned()),
        (
            "CANTER_HOME".into(),
            home.path().to_string_lossy().into_owned(),
        ),
    ]);
    let mut snapshot = SessionSnapshot::empty("history");
    snapshot.status = SessionStatus::Idle;
    snapshot.settings.cwd = home.path().to_string_lossy().into_owned();
    let app = App::new(snapshot.clone(), None, 0).with_input_history(&env);
    let mut ui = Ui::new(TestBackend::new(100, 24), app).unwrap();
    ui.app_mut().handle_paste("!  git status  ");
    ui.draw(0).unwrap();
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::ShellCommand("git status".into()))
    );
    assert!(!text(&mut ui).contains("!  git status"));
    let app = App::new(snapshot, None, 0).with_input_history(&env);
    let mut ui = Ui::new(TestBackend::new(100, 24), app).unwrap();
    ui.draw(0).unwrap();
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 0);
    assert!(text(&mut ui).contains("!  git status"));
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
        Some(Action::ShellCommand("git status".into()))
    );
}

#[test]
fn user_shell_esc_interrupts_the_command_turn() {
    let mut snapshot = SessionSnapshot::empty("user-shell");
    snapshot.status = SessionStatus::Working;
    snapshot.turns.push(
        serde_json::from_value(serde_json::json!({
            "id":"shell-turn", "status":"inProgress", "input":[], "clientName":"userShell",
            "error":null, "usage":null, "items":[{
                "type":"commandExecution", "id":"shell-item", "command":"sleep 30",
                "source":"userShell", "cwd":"/work", "status":"inProgress",
                "aggregatedOutput":"", "exitCode":null, "durationMs":null
            }]
        }))
        .unwrap(),
    );
    let mut ui = Ui::new(TestBackend::new(100, 24), App::new(snapshot, None, 0)).unwrap();
    ui.draw(0).unwrap();
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 0),
        Some(Action::Interrupt {
            turn_id: "shell-turn".into()
        })
    );
}

#[test]
fn user_shell_expands_folded_paste_before_running() {
    let command = format!("printf '%s' '{}'", "x".repeat(1200));
    for separate_bang in [false, true] {
        let mut ui = screen(SessionStatus::Idle);
        if separate_bang {
            ui.app_mut()
                .handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE), 0);
            ui.app_mut().handle_paste(&command);
        } else {
            ui.app_mut().handle_paste(&format!("!{command}"));
        }
        ui.draw(0).unwrap();
        assert_eq!(
            ui.app_mut()
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
            Some(Action::ShellCommand(command.clone()))
        );
    }
}
