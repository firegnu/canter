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
        // Kept as it was: in shell mode, the rest after the prompt.
        let (prompt, rest) = input_row(&mut ui);
        assert_eq!(prompt.symbol(), "!");
        assert_eq!(rest.trim_end(), draft[1..].trim_end());
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
    // A `!` command from the history comes back in shell mode.
    assert_shell_mode(&mut ui, true);
    let (_, rest) = input_row(&mut ui);
    assert_eq!(rest.trim_end(), "  git status");
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

/// The input box's rows from the top border down, in cells: the top-left
/// corner, the first row (its prompt cell and what is after the prompt)
/// and the bottom border.
fn input_box(
    ui: &mut Ui<TestBackend>,
) -> (ratatui::buffer::Cell, ratatui::buffer::Cell, String, String) {
    let screen = text(ui);
    let rows: Vec<&str> = screen.lines().collect();
    let top = (0..24)
        .rev()
        .find(|&y| rows[y].starts_with('╭'))
        .unwrap_or_else(|| panic!("no input box:\n{screen}"));
    let bottom = (top + 1..24)
        .find(|&y| rows[y].starts_with('╰'))
        .unwrap_or_else(|| panic!("no bottom border:\n{screen}"));
    let rest = rows[top + 1].chars().skip(4).collect::<String>();
    let rest = rest.trim_end_matches('│').to_owned();
    (
        cell(ui, 0, top),
        cell(ui, 2, top + 1),
        rest,
        rows[bottom].to_owned(),
    )
}

/// The prompt cell and what is after the prompt on the box's first row.
fn input_row(ui: &mut Ui<TestBackend>) -> (ratatui::buffer::Cell, String) {
    let (_, prompt, rest, _) = input_box(ui);
    (prompt, rest)
}

/// In shell mode: a magenta border, a magenta `!` in place of `› ` and
/// `! for shell mode` in magenta on the bottom border; otherwise none of it.
fn assert_shell_mode(ui: &mut Ui<TestBackend>, shell: bool) {
    use ratatui::style::Color;
    let (corner, prompt, _, bottom) = input_box(ui);
    assert_eq!(corner.symbol(), "╭");
    if shell {
        assert_eq!(corner.fg, Color::Magenta);
        assert_eq!(prompt.symbol(), "!");
        assert_eq!(prompt.fg, Color::Magenta);
        let at = bottom
            .find("! for shell mode")
            .unwrap_or_else(|| panic!("no hint on {bottom:?}"));
        let x = bottom[..at].chars().count();
        let y = (0..24)
            .rev()
            .find(|&y| cell(ui, 0, y).symbol() == "╰")
            .unwrap();
        assert!(x < 5, "the hint is at the left: {bottom:?}");
        for column in 0.."! for shell mode".len() {
            assert_eq!(cell(ui, x + column, y).fg, Color::Magenta, "{bottom:?}");
        }
    } else {
        assert_ne!(corner.fg, Color::Magenta);
        assert_eq!(prompt.symbol(), "›");
        assert!(!bottom.contains("shell mode"), "{bottom:?}");
    }
}

fn press(ui: &mut Ui<TestBackend>, code: KeyCode) -> Option<Action> {
    let action = ui
        .app_mut()
        .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
    ui.draw(0).unwrap();
    action
}

fn type_text(ui: &mut Ui<TestBackend>, text: &str) {
    for c in text.chars() {
        press(ui, KeyCode::Char(c));
    }
}

#[test]
fn user_shell_bang_draft_shows_magenta_prompt_and_hint() {
    let mut ui = screen(SessionStatus::Idle);
    type_text(&mut ui, "!ls");
    assert_shell_mode(&mut ui, true);
    // The `!` is the prompt, not part of the text.
    let (_, rest) = input_row(&mut ui);
    assert_eq!(rest.trim_end(), "ls");
    assert!(!text(&mut ui).contains("!ls"));

    let mut ui = screen(SessionStatus::Idle);
    ui.app_mut().handle_paste("ls");
    assert_shell_mode(&mut ui, false);
    let (_, rest) = input_row(&mut ui);
    assert_eq!(rest.trim_end(), "ls");
}

#[test]
fn user_shell_backspace_leaves_shell_mode_only_when_empty() {
    // With text, at the start: nothing happens, the command stays one.
    let mut ui = screen(SessionStatus::Idle);
    type_text(&mut ui, "!ls");
    press(&mut ui, KeyCode::Home);
    press(&mut ui, KeyCode::Backspace);
    assert_shell_mode(&mut ui, true);
    let (_, rest) = input_row(&mut ui);
    assert_eq!(rest.trim_end(), "ls");
    assert_eq!(
        press(&mut ui, KeyCode::Enter),
        Some(Action::ShellCommand("ls".into()))
    );

    // With only the `!`: back to a message.
    let mut ui = screen(SessionStatus::Idle);
    type_text(&mut ui, "!");
    assert_shell_mode(&mut ui, true);
    press(&mut ui, KeyCode::Backspace);
    assert_shell_mode(&mut ui, false);
    assert!(text(&mut ui).contains("Type a message"));
}

#[test]
fn user_shell_box_is_back_to_messages_after_running() {
    let mut ui = screen(SessionStatus::Idle);
    type_text(&mut ui, "!ls");
    assert_eq!(
        press(&mut ui, KeyCode::Enter),
        Some(Action::ShellCommand("ls".into()))
    );
    assert_shell_mode(&mut ui, false);
    assert!(text(&mut ui).contains("Type a message"));
}

#[test]
fn user_shell_card_bang_is_magenta_and_agent_card_is_unchanged() {
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
    assert_eq!(bang.fg, Color::Magenta);
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
