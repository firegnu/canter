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

/// Cell `(x, y)` of the last frame.
fn cell(ui: &Ui<TestBackend>, x: usize, y: usize) -> ratatui::buffer::Cell {
    ui.backend().buffer().content[y * 100 + x].clone()
}

/// Row and column of the first `needle` on screen, in cells.
fn find(ui: &mut Ui<TestBackend>, needle: &str) -> (usize, usize) {
    let screen = text(ui);
    let content = &ui.backend().buffer().content;
    (0..24)
        .flat_map(|y| (0..100).map(move |x| (y, x)))
        .find(|&(y, x)| {
            content[y * 100 + x..(y + 1) * 100]
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .starts_with(needle)
        })
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{screen}"))
}

/// The input box's top-left corner and the cell of the draft's first
/// character.
fn input_cells(ui: &mut Ui<TestBackend>) -> (ratatui::buffer::Cell, ratatui::buffer::Cell) {
    let (y, x) = find(ui, "› ");
    (cell(ui, 0, y - 1), cell(ui, x + 2, y))
}

#[test]
fn user_shell_bang_draft_shows_red_border_and_bang() {
    use ratatui::style::Color;
    let mut ui = screen(SessionStatus::Idle);
    for c in "!ls".chars() {
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), 0);
    }
    let (corner, first) = input_cells(&mut ui);
    assert_eq!(corner.symbol(), "╭");
    assert_eq!(corner.fg, Color::Red);
    assert_eq!(first.symbol(), "!");
    assert_eq!(first.fg, Color::Red);
    let (y, x) = find(&mut ui, "!ls");
    assert_ne!(cell(&ui, x + 1, y).fg, Color::Red, "only the bang is red");

    // Deleting the `!` puts the box back at once.
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), 0);
    ui.app_mut()
        .handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE), 0);
    let (corner, first) = input_cells(&mut ui);
    assert_eq!(first.symbol(), "l");
    assert_ne!(corner.fg, Color::Red);
    assert_ne!(first.fg, Color::Red);

    let mut ui = screen(SessionStatus::Idle);
    ui.app_mut().handle_paste("ls");
    let (corner, first) = input_cells(&mut ui);
    assert_eq!(first.symbol(), "l");
    assert_ne!(corner.fg, Color::Red);
    assert_ne!(first.fg, Color::Red);
}

#[test]
fn user_shell_card_bang_is_red_and_agent_card_is_unchanged() {
    use ratatui::style::Color;
    let mut snapshot = SessionSnapshot::empty("user-shell");
    for (turn, source) in [("agent-turn", "agent"), ("shell-turn", "userShell")] {
        snapshot.turns.push(
            serde_json::from_value(serde_json::json!({
                "id":turn, "status":"completed", "input":[], "clientName":"tui",
                "error":null, "usage":null, "items":[{
                    "type":"commandExecution", "id":format!("{turn}-item"),
                    "command":"git status", "source":source, "cwd":"/work",
                    "status":"completed", "aggregatedOutput":"On branch main",
                    "exitCode":0, "durationMs":10
                }]
            }))
            .unwrap(),
        );
    }
    let mut ui = Ui::new(TestBackend::new(100, 24), App::new(snapshot, None, 0)).unwrap();
    let (agent_y, agent_x) = find(&mut ui, "Run git status");
    let (shell_y, shell_x) = find(&mut ui, "Run ! git status");
    assert_ne!(agent_y, shell_y);
    let bang = cell(&ui, shell_x + 4, shell_y);
    assert_eq!(bang.symbol(), "!");
    assert_eq!(bang.fg, Color::Red);
    // The command reads the same on both cards; only the user's has the `!`.
    for column in 0.."git status".len() {
        let agent = cell(&ui, agent_x + 4 + column, agent_y);
        let shell = cell(&ui, shell_x + 6 + column, shell_y);
        assert_eq!(agent.symbol(), shell.symbol());
        assert_eq!(agent.style(), shell.style(), "column {column}");
    }
    assert!(
        (0..100).all(|x| cell(&ui, x, agent_y).symbol() != "!"),
        "the agent's card has no bang"
    );
}
