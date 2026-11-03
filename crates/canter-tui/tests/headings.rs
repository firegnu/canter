//! Markdown headings in the model's text, on ratatui's TestBackend: `#`,
//! `##` and `###` each look different from one another and from bold in a
//! paragraph, on a dark, a light and an unknown background.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const TEXT: &str =
    "# Alpha\n\n## Bravo\n\nText with **Delta** in it.\n\n### Charlie\n\n#### Echo\n";

fn screen(theme: Theme) -> Buffer {
    theme::set(theme);
    let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
    let mut ui = Ui::new(TestBackend::new(80, 30), app).unwrap();
    let mut seq = 0;
    let mut event = |payload: Value| {
        seq += 1;
        let mut event = json!({ "seq": seq, "sessionId": SESSION, "timestamp": 0 });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        ui.app_mut().apply_event(&event, 0);
    };
    event(json!({ "type": "turn.started", "turn": {
        "id": "t1", "items": [], "status": "inProgress", "error": null,
        "input": [{ "type": "text", "text": "show headings" }],
        "clientName": "canter-tui", "usage": null,
    }}));
    let message = json!({ "type": "agentMessage", "id": "m1", "text": TEXT });
    event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
    event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
    ui.draw(0).unwrap();
    ui.backend().buffer().clone()
}

/// The colour and modifiers of the cell where `text` starts.
fn style(buffer: &Buffer, text: &str) -> (Color, Modifier) {
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        if let Some(byte) = row.find(text) {
            let cell = &buffer[(row[..byte].width() as u16, y)];
            return (cell.fg, cell.modifier);
        }
    }
    panic!("{text:?} is not on screen");
}

#[test]
fn heading_levels_and_bold_each_look_different() {
    for background in [Background::Unknown, Background::Dark, Background::Light] {
        let buffer = screen(Theme {
            background,
            ..Theme::default()
        });
        let styles = [
            ("#", style(&buffer, "Alpha")),
            ("##", style(&buffer, "Bravo")),
            ("###", style(&buffer, "Charlie")),
            ("**bold**", style(&buffer, "Delta")),
        ];
        for (i, (a, first)) in styles.iter().enumerate() {
            assert!(
                first.1.contains(Modifier::BOLD),
                "{a} is bold on {background:?}"
            );
            // Yellow is faint on a light background.
            assert!(
                !matches!(first.0, Color::Yellow | Color::LightYellow),
                "{a} on {background:?}"
            );
            for (b, second) in &styles[i + 1..] {
                assert_ne!(first, second, "{a} and {b} on {background:?}");
            }
        }
        // Lower headings look like `###`.
        assert_eq!(
            style(&buffer, "Echo"),
            style(&buffer, "Charlie"),
            "{background:?}"
        );
    }
}
