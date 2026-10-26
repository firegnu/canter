//! Markdown tables in the model's text, on ratatui's TestBackend: box lines,
//! alignment, Chinese by display width, wrapping inside cells, records when
//! the table is too wide, and a table that is still streaming.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const HEIGHT: u16 = 40;

/// The table from the plan's wireframe: Chinese text, the second column
/// right-aligned.
const FILES: &str = "| 文件 | 行数 | 做什么 |
|---|---:|---|
| cells.rs | 513 | 每种项目怎么画 |
| markdown.rs | 262 | markdown 变成终端行 |
| text.rs | 242 | 清理、宽度、折行 |
";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(width: u16) -> Self {
        theme::set(Theme::default());
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, HEIGHT), app).unwrap(),
            seq: 0,
        };
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "show a table" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        screen.event(json!({ "type": "session.status", "status": "working" }));
        screen
    }

    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": 0 });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, 0);
    }

    /// The model writes `text` as a finished message.
    fn say(&mut self, text: &str) {
        let message = json!({ "type": "agentMessage", "id": "m1", "text": text });
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(0).unwrap();
        self.ui.backend().buffer()
    }

    fn lines(&mut self) -> Vec<String> {
        let buffer = self.draw();
        (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect()
    }

    /// The rows from the first one starting with `first` (after the
    /// indent), `count` of them.
    fn rows_from(&mut self, first: &str, count: usize) -> Vec<String> {
        let lines = self.lines();
        let at = lines
            .iter()
            .position(|line| line.trim_start().starts_with(first))
            .unwrap_or_else(|| panic!("{first:?} is not on screen:\n{}", lines.join("\n")));
        lines[at..].iter().take(count).cloned().collect()
    }
}

fn row(buffer: &Buffer, y: u16) -> String {
    let mut row = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += symbol.width().max(1) as u16;
    }
    row
}

/// The cell where `text` first starts.
fn cell<'a>(buffer: &'a Buffer, text: &str) -> &'a Cell {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            return &buffer[(row[..byte].width() as u16, y)];
        }
    }
    panic!("{text:?} is not on screen");
}

#[test]
fn a_table_that_fits_is_drawn_with_box_lines_aligned_by_display_width() {
    let mut screen = Screen::new(60);
    screen.say(FILES);
    assert_eq!(
        screen.rows_from("┌", 7),
        [
            "  ┌─────────────┬──────┬─────────────────────┐",
            "  │ 文件        │ 行数 │ 做什么              │",
            "  ├─────────────┼──────┼─────────────────────┤",
            "  │ cells.rs    │  513 │ 每种项目怎么画      │",
            "  │ markdown.rs │  262 │ markdown 变成终端行 │",
            "  │ text.rs     │  242 │ 清理、宽度、折行    │",
            "  └─────────────┴──────┴─────────────────────┘",
        ]
    );
    let buffer = screen.draw();
    for text in ["┌", "│ 文件", "├", "└"] {
        assert!(
            cell(buffer, text).modifier.contains(Modifier::DIM),
            "{text:?} is not dim"
        );
    }
    for text in ["文件", "行数", "做什么"] {
        assert!(
            cell(buffer, text).modifier.contains(Modifier::BOLD),
            "header {text:?} is not bold"
        );
    }
    assert!(!cell(buffer, "cells.rs").modifier.contains(Modifier::BOLD));
    assert!(!cell(buffer, "cells.rs").modifier.contains(Modifier::DIM));
    assert!(!screen.lines().iter().any(|line| line.contains("|---")));
}

#[test]
fn a_narrower_screen_wraps_the_last_column_and_draws_lines_between_rows() {
    let mut screen = Screen::new(35);
    screen.say(FILES);
    assert_eq!(
        screen.rows_from("┌", 13),
        [
            "  ┌─────────────┬──────┬──────────┐",
            "  │ 文件        │ 行数 │ 做什么   │",
            "  ├─────────────┼──────┼──────────┤",
            "  │ cells.rs    │  513 │ 每种项目 │",
            "  │             │      │ 怎么画   │",
            "  ├─────────────┼──────┼──────────┤",
            "  │ markdown.rs │  262 │ markdown │",
            "  │             │      │ 变成终端 │",
            "  │             │      │ 行       │",
            "  ├─────────────┼──────┼──────────┤",
            "  │ text.rs     │  242 │ 清理、宽 │",
            "  │             │      │ 度、折行 │",
            "  └─────────────┴──────┴──────────┘",
        ]
    );
}

#[test]
fn a_table_with_many_columns_becomes_records_of_header_and_value() {
    let mut screen = Screen::new(40);
    screen.say(
        "| 名字 | 语言 | 行数 | 测试 | 作者 | 日期 | 状态 | 备注 |
|---|---|---|---|---|---|---|---|
| canter | Rust | 8650 | 有 | firegnu | 10-26 | 开发中 | 展示层第三件 |
| ranch | Rust | 1200 | 有 | firegnu | 10-20 | 在用 | 无 |
",
    );
    let lines = screen.lines();
    assert!(
        !lines
            .iter()
            .any(|line| line.contains('┌') || line.contains("| 名字") || line.contains("|---")),
        "{}",
        lines.join("\n")
    );
    assert_eq!(
        screen.rows_from("名字: canter", 17),
        [
            "  名字: canter",
            "  语言: Rust",
            "  行数: 8650",
            "  测试: 有",
            "  作者: firegnu",
            "  日期: 10-26",
            "  状态: 开发中",
            "  备注: 展示层第三件",
            "  ────────",
            "  名字: ranch",
            "  语言: Rust",
            "  行数: 1200",
            "  测试: 有",
            "  作者: firegnu",
            "  日期: 10-20",
            "  状态: 在用",
            "  备注: 无",
        ]
    );
    let buffer = screen.draw();
    assert!(cell(buffer, "────────").modifier.contains(Modifier::DIM));
}

#[test]
fn columns_follow_left_center_and_right_alignment() {
    let mut screen = Screen::new(60);
    screen.say(
        "| left | center | right | none |
|:---|:---:|---:|---|
| a | b | c | d |
| 长一些 | 中文居中 | 靠右 | 默认 |
",
    );
    assert_eq!(
        screen.rows_from("┌", 6),
        [
            "  ┌────────┬──────────┬───────┬──────┐",
            "  │ left   │  center  │ right │ none │",
            "  ├────────┼──────────┼───────┼──────┤",
            "  │ a      │    b     │     c │ d    │",
            "  │ 长一些 │ 中文居中 │  靠右 │ 默认 │",
            "  └────────┴──────────┴───────┴──────┘",
        ]
    );
}

#[test]
fn bold_and_inline_code_inside_cells_keep_their_styles() {
    let mut screen = Screen::new(60);
    screen.say(
        "| 名字 | 说明 |
|---|---|
| **粗体** | 用 `cargo test` 跑 |
",
    );
    assert_eq!(
        screen.rows_from("┌", 5),
        [
            "  ┌──────┬──────────────────┐",
            "  │ 名字 │ 说明             │",
            "  ├──────┼──────────────────┤",
            "  │ 粗体 │ 用 cargo test 跑 │",
            "  └──────┴──────────────────┘",
        ]
    );
    let buffer = screen.draw();
    assert!(cell(buffer, "粗体").modifier.contains(Modifier::BOLD));
    assert_eq!(cell(buffer, "cargo test").fg, Color::Cyan);
    assert_ne!(cell(buffer, "跑").fg, Color::Cyan);
    assert!(!cell(buffer, "跑").modifier.contains(Modifier::BOLD));
}

#[test]
fn a_table_still_streaming_draws_without_breaking_and_is_whole_once_done() {
    let mut screen = Screen::new(60);
    let (first, rest) = FILES.split_at(FILES.find("| markdown.rs").unwrap() + 8);
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
        "type": "agentMessage", "id": "m1", "text": "",
    }}));
    let mut sent = String::new();
    // Every few bytes, cut on a character boundary, as a stream might.
    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, _) in first.char_indices().skip(1) {
        if i - start >= 7 {
            chunks.push(&first[start..i]);
            start = i;
        }
    }
    chunks.push(&first[start..]);
    for chunk in chunks {
        sent.push_str(chunk);
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage", "delta": chunk } }),
        );
        screen.draw();
    }
    // Half way: the rows so far form a table already.
    let lines = screen.lines();
    assert!(
        lines.iter().any(|line| line.contains("│ cells.rs")),
        "{}",
        lines.join("\n")
    );
    assert!(!lines.iter().any(|line| line.contains("|---")));
    screen.event(
        json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": { "type": "agentMessage", "delta": rest } }),
    );
    screen.draw();
    sent.push_str(rest);
    assert_eq!(sent, FILES);
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "agentMessage", "id": "m1", "text": FILES,
    }}));
    assert_eq!(
        screen.rows_from("┌", 7),
        [
            "  ┌─────────────┬──────┬─────────────────────┐",
            "  │ 文件        │ 行数 │ 做什么              │",
            "  ├─────────────┼──────┼─────────────────────┤",
            "  │ cells.rs    │  513 │ 每种项目怎么画      │",
            "  │ markdown.rs │  262 │ markdown 变成终端行 │",
            "  │ text.rs     │  242 │ 清理、宽度、折行    │",
            "  └─────────────┴──────┴─────────────────────┘",
        ]
    );
}
