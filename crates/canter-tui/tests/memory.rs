use canter_protocol::{
    MemoryDeleteParams, MemoryEntry, MemoryListResponse, MemoryScope, SessionSnapshot,
};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

fn entries() -> MemoryListResponse {
    MemoryListResponse {
        data: vec![
            MemoryEntry {
                scope: MemoryScope::Global,
                file: "preference.md".into(),
                name: "Testing preference".into(),
                description: "Use isolated fixtures.".into(),
                path: "/server/memory/preference.md".into(),
            },
            MemoryEntry {
                scope: MemoryScope::Project,
                file: "convention.md".into(),
                name: "Project convention".into(),
                description: "Keep the existing style.".into(),
                path: "/server/projects/test/memory/convention.md".into(),
            },
        ],
    }
}
fn screen(list: MemoryListResponse) -> Ui<TestBackend> {
    let mut ui = Ui::new(
        TestBackend::new(110, 35),
        App::new(SessionSnapshot::empty("test"), None, 0),
    )
    .unwrap();
    ui.app_mut().show_memories(Ok(list));
    ui.draw(1000).unwrap();
    ui
}
fn key(ui: &mut Ui<TestBackend>, key: KeyCode) -> Option<Action> {
    let result = ui
        .app_mut()
        .handle_key(KeyEvent::new(key, KeyModifiers::NONE), 1000);
    ui.draw(1000).unwrap();
    result
}
fn text(ui: &Ui<TestBackend>) -> String {
    let buffer = ui.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn memory_list_displays_both_layers_and_enter_opens_the_selected_server_path() {
    let mut ui = screen(entries());
    let text = text(&ui);
    for expected in [
        "Testing preference",
        "Use isolated fixtures.",
        "Global",
        "Project convention",
        "Project",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    assert_eq!(key(&mut ui, KeyCode::Down), None);
    assert_eq!(
        key(&mut ui, KeyCode::Enter),
        Some(Action::EditMemory(entries().data[1].clone()))
    );
}

#[test]
fn memory_delete_requires_a_separate_confirmation_and_escape_cancels() {
    let mut ui = screen(entries());
    assert_eq!(key(&mut ui, KeyCode::Char('d')), None);
    assert!(text(&ui).contains("Delete memory \"Testing preference\"?"));
    assert_eq!(key(&mut ui, KeyCode::Enter), None);
    assert_eq!(key(&mut ui, KeyCode::Esc), None);
    assert!(text(&ui).contains("Use isolated fixtures."));
    assert_eq!(key(&mut ui, KeyCode::Down), None);
    assert_eq!(key(&mut ui, KeyCode::Char('d')), None);
    assert_eq!(
        key(&mut ui, KeyCode::Char('y')),
        Some(Action::DeleteMemory(MemoryDeleteParams {
            scope: MemoryScope::Project,
            file: "convention.md".into()
        }))
    );
}

#[test]
fn memory_empty_list_and_errors_are_visible_without_edit_or_delete_actions() {
    let mut ui = screen(MemoryListResponse { data: Vec::new() });
    assert!(text(&ui).contains("No memories yet."));
    assert_eq!(key(&mut ui, KeyCode::Char('d')), None);
    assert_eq!(key(&mut ui, KeyCode::Enter), None);
    ui.app_mut()
        .show_memories(Err("Memory is unavailable in restricted mode".into()));
    ui.draw(1000).unwrap();
    assert!(text(&ui).contains("Memory is unavailable in restricted mode"));
}
