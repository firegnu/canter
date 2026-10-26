//! How diffs are drawn, in the history and in the approval box: one column of
//! line numbers, `⋮` between hunks, backgrounds on added and removed lines,
//! changed words a step deeper, and the history's short form.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDTH: u16 = 80;

/// Two hunks. The first changes one word in a pair of lines; the second
/// removes two lines and adds one, so its lines are not paired.
const DIFF: &str = "--- a/src/lib.rs
+++ b/src/lib.rs
@@ -13,3 +13,3 @@
 /// Output lines a finished command keeps.
-const FINISHED_TAIL: usize = 3;
+const FINISHED_TAIL: usize = 4;
 /// Output lines shown while a command runs.
@@ -427,3 +427,2 @@
-    let tail = if status == ItemStatus::InProgress {
-    let x = 1;
+    let tail = if running {
     LIVE_TAIL
";

/// The rows under the card's title: removed lines keep their old numbers,
/// added and context lines show the new ones.
const HISTORY_ROWS: [&str; 9] = [
    "       13   /// Output lines a finished command keeps.",
    "       14 - const FINISHED_TAIL: usize = 3;",
    "       14 + const FINISHED_TAIL: usize = 4;",
    "       15   /// Output lines shown while a command runs.",
    "          ⋮",
    "      427 -     let tail = if status == ItemStatus::InProgress {",
    "      428 -     let x = 1;",
    "      427 +     let tail = if running {",
    "      428       LIVE_TAIL",
];

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    /// Draws for `theme`; the theme is per thread, and each test has its own.
    fn new(theme: Theme, height: u16) -> Self {
        theme::set(theme);
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(WIDTH, height), app).unwrap(),
            seq: 0,
        };
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "change it" }],
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

    /// A finished file change in the history.
    fn changed(&mut self, id: &str, path: &str, kind: Value, diff: &str) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
            "type": "fileChange", "id": id, "status": "completed",
            "changes": [{ "path": path, "kind": kind, "diff": diff }],
        }}));
    }

    /// The model asks to edit `path` with `diff`; the approval box shows it.
    fn ask_edit(&mut self, path: &str, diff: &str) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": {
            "type": "fileChange", "id": "edit", "status": "inProgress",
            "changes": [{ "path": path, "kind": update(), "diff": diff }],
        }}));
        self.event(json!({ "type": "request.opened", "request": {
            "id": "r1", "turnId": "t1", "createdAt": 0,
            "kind": {
                "type": "approval", "itemId": "edit", "tool": "edit",
                "arguments": { "path": path, "diff": diff },
                "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
            },
        }}));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
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

    /// The `count` rows after the first row containing `title`.
    fn rows_after(&mut self, title: &str, count: usize) -> Vec<String> {
        let lines = self.lines();
        let at = lines
            .iter()
            .position(|line| line.contains(title))
            .unwrap_or_else(|| panic!("{title:?} is not on screen:\n{}", lines.join("\n")));
        lines[at + 1..].iter().take(count).cloned().collect()
    }
}

fn update() -> Value {
    json!({ "type": "update", "movePath": null })
}

/// What a row of the approval box holds between its borders.
fn inside(row: &str) -> String {
    row.strip_prefix("│ ")
        .unwrap_or(row)
        .trim_end()
        .trim_end_matches('│')
        .trim_end()
        .to_owned()
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

/// The cell where `text` first starts, and the row it is on.
fn find(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            return (row[..byte].width() as u16, y);
        }
    }
    panic!("{text:?} is not on screen");
}

fn cell<'a>(buffer: &'a Buffer, text: &str) -> &'a Cell {
    &buffer[find(buffer, text)]
}

/// The row of `text`, from its first cell to the right edge.
fn row_cells<'a>(buffer: &'a Buffer, text: &str) -> Vec<&'a Cell> {
    let (x, y) = find(buffer, text);
    (x..buffer.area.width).map(|x| &buffer[(x, y)]).collect()
}

/// The colour a 256-colour index stands for (past the first 16).
fn rgb_256(index: u8) -> [u8; 3] {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    assert!(
        index >= 16,
        "index {index} changes with the terminal's settings"
    );
    if index >= 232 {
        [8 + 10 * (index - 232); 3]
    } else {
        let cube = usize::from(index - 16);
        [LEVELS[cube / 36], LEVELS[cube / 6 % 6], LEVELS[cube % 6]]
    }
}

fn indexed(color: Color) -> u8 {
    match color {
        Color::Indexed(index) => index,
        other => panic!("{other:?} is not a 256-colour index"),
    }
}

/// Green clearly above red and blue.
fn greenish(index: u8) -> bool {
    let [r, g, b] = rgb_256(index);
    g >= r.saturating_add(30) && g >= b.saturating_add(30)
}

fn reddish(index: u8) -> bool {
    let [r, g, b] = rgb_256(index);
    r >= g.saturating_add(30) && r >= b.saturating_add(30)
}

fn theme(depth: Depth, background: Background) -> Theme {
    Theme { depth, background }
}

#[test]
fn history_shows_line_numbers_and_a_gap_between_hunks() {
    let mut screen = Screen::new(Theme::default(), 30);
    screen.changed("c1", "src/lib.rs", update(), DIFF);
    let rows = screen.rows_after("● Edit src/lib.rs  +2 −3", HISTORY_ROWS.len());
    assert_eq!(rows, HISTORY_ROWS);
    assert!(
        screen.lines().iter().all(|line| !line.contains("@@")),
        "no hunk headers"
    );
}

#[test]
fn true_colour_paints_added_and_removed_lines_and_deeper_words() {
    for (background, removed, added, removed_word, added_word) in [
        (
            Background::Dark,
            Color::Rgb(0x4a, 0x22, 0x1d),
            Color::Rgb(0x21, 0x3a, 0x2b),
            Color::Rgb(0x73, 0x32, 0x2a),
            Color::Rgb(0x2e, 0x5c, 0x3f),
        ),
        (
            Background::Light,
            Color::Rgb(0xff, 0xeb, 0xe9),
            Color::Rgb(0xda, 0xfb, 0xe1),
            Color::Rgb(0xff, 0xce, 0xcb),
            Color::Rgb(0xab, 0xf2, 0xbc),
        ),
    ] {
        let mut screen = Screen::new(theme(Depth::TrueColor, background), 30);
        screen.changed("c1", "src/lib.rs", update(), DIFF);
        let buffer = screen.draw();

        let minus = row_cells(buffer, "14 - const");
        assert!(
            minus
                .iter()
                .all(|cell| cell.bg == removed || cell.bg == removed_word),
            "{background:?}: the removed line is painted to the right edge"
        );
        assert_eq!(cell(buffer, "- const").fg, Color::Red, "{background:?}");
        assert_eq!(
            cell(buffer, "const FINISHED").fg,
            Color::Reset,
            "{background:?}"
        );
        let plus = row_cells(buffer, "14 + const");
        assert!(
            plus.iter()
                .all(|cell| cell.bg == added || cell.bg == added_word),
            "{background:?}: the added line is painted to the right edge"
        );
        assert_eq!(cell(buffer, "+ const").fg, Color::Green, "{background:?}");
        assert_eq!(cell(buffer, "const FINISHED").bg, removed, "{background:?}");

        assert_eq!(cell(buffer, "3;").bg, removed_word, "{background:?}");
        assert_eq!(cell(buffer, "4;").bg, added_word, "{background:?}");
        assert!(!cell(buffer, "3;").modifier.contains(Modifier::REVERSED));

        let context = row_cells(buffer, "13   ///");
        assert!(
            context.iter().all(|cell| cell.bg == Color::Reset),
            "{background:?}: context lines have no background"
        );
    }
}

#[test]
fn colours_256_keep_added_lines_green_and_removed_lines_red() {
    for background in [Background::Dark, Background::Light] {
        let mut screen = Screen::new(theme(Depth::Ansi256, background), 30);
        screen.changed("c1", "src/lib.rs", update(), DIFF);
        let buffer = screen.draw();
        let removed = indexed(cell(buffer, "const FINISHED").bg);
        let removed_word = indexed(cell(buffer, "3;").bg);
        let (x, y) = find(buffer, "14 + const");
        let added = indexed(buffer[(x + 5, y)].bg);
        let added_word = indexed(cell(buffer, "4;").bg);
        assert!(reddish(removed), "{background:?}: removed {removed}");
        assert!(reddish(removed_word), "{background:?}: word {removed_word}");
        assert!(greenish(added), "{background:?}: added {added}");
        assert!(greenish(added_word), "{background:?}: word {added_word}");
        assert_ne!(removed, removed_word, "{background:?}");
        assert_ne!(added, added_word, "{background:?}");
    }
}

#[test]
fn without_a_background_lines_are_coloured_text_and_words_reversed() {
    for theme in [
        Theme::default(),
        theme(Depth::Ansi16, Background::Dark),
        theme(Depth::TrueColor, Background::Unknown),
        theme(Depth::Ansi256, Background::Unknown),
    ] {
        let mut screen = Screen::new(theme, 30);
        screen.changed("c1", "src/lib.rs", update(), DIFF);
        let buffer = screen.draw();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                assert_eq!(buffer[(x, y)].bg, Color::Reset, "{theme:?} at {x},{y}");
            }
        }
        assert_eq!(cell(buffer, "- const").fg, Color::Red, "{theme:?}");
        assert_eq!(cell(buffer, "const FINISHED").fg, Color::Red, "{theme:?}");
        assert_eq!(cell(buffer, "+ const").fg, Color::Green, "{theme:?}");
        let reversed = |text| cell(buffer, text).modifier.contains(Modifier::REVERSED);
        assert!(reversed("3;"), "{theme:?}");
        assert!(reversed("4;"), "{theme:?}");
        assert!(!reversed("const FINISHED"), "{theme:?}");
    }
}

#[test]
fn a_run_of_unequal_length_marks_no_words() {
    for theme in [Theme::default(), theme(Depth::TrueColor, Background::Dark)] {
        let mut screen = Screen::new(theme, 30);
        screen.changed("c1", "src/lib.rs", update(), DIFF);
        let buffer = screen.draw();
        for text in ["427 -", "428 -", "427 +"] {
            let line_bg = row_cells(buffer, text)[6].bg;
            assert!(
                row_cells(buffer, text).iter().all(|cell| {
                    !cell.modifier.contains(Modifier::REVERSED) && cell.bg == line_bg
                }),
                "{theme:?}: {text} has no marked words"
            );
        }
    }
}

/// `count` numbered lines, as a hunk of the given kind of change.
fn lines(count: usize, marker: char, word: &str) -> String {
    (1..=count)
        .map(|n| format!("{marker}{word} {n}\n"))
        .collect()
}

#[test]
fn history_shortens_long_changes() {
    let mut screen = Screen::new(Theme::default(), 60);
    let edit = format!(
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,10 +1,10 @@\n{}{}",
        lines(10, '-', "old"),
        lines(10, '+', "new")
    );
    let add = format!(
        "--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1,10 @@\n{}",
        lines(10, '+', "line")
    );
    let delete = format!(
        "--- a/c.txt\n+++ /dev/null\n@@ -1,4 +0,0 @@\n{}",
        lines(4, '-', "gone")
    );
    screen.changed("c1", "a.txt", update(), &edit);
    screen.changed("c2", "b.txt", json!({ "type": "add" }), &add);
    screen.changed("c3", "c.txt", json!({ "type": "delete" }), &delete);

    let rows = screen.rows_after("● Edit a.txt  +10 −10", 13);
    assert_eq!(rows[0], "       1 - old 1");
    assert_eq!(rows[9], "      10 - old 10");
    assert_eq!(rows[10], "       1 + new 1");
    assert_eq!(rows[11], "       2 + new 2");
    assert_eq!(rows[12].trim(), "… 8 more lines");

    let rows = screen.rows_after("● Add b.txt  +10", 7);
    assert_eq!(rows[0], "       1 + line 1");
    assert_eq!(rows[5], "       6 + line 6");
    assert_eq!(rows[6].trim(), "… 4 more lines");

    let rows = screen.rows_after("● Delete c.txt  −4", 1);
    assert!(
        !rows[0].contains("gone"),
        "a deleted file shows only its title: {rows:?}"
    );
    assert!(screen.lines().iter().all(|line| !line.contains("gone")));
}

#[test]
fn approval_box_shows_the_whole_diff_the_same_way() {
    let mut screen = Screen::new(Theme::default(), 60);
    let edit = format!(
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,10 +1,10 @@\n{}{}",
        lines(10, '-', "old"),
        lines(10, '+', "new")
    );
    screen.ask_edit("a.txt", &edit);
    let rows: Vec<String> = screen
        .rows_after("│ a.txt  +10 −10", 20)
        .iter()
        .map(|row| inside(row))
        .collect();
    assert_eq!(rows[0], " 1 - old 1");
    assert_eq!(rows[9], "10 - old 10");
    assert_eq!(rows[10], " 1 + new 1");
    assert_eq!(rows[19], "10 + new 10");
    assert!(
        screen
            .lines()
            .iter()
            .all(|line| !line.contains("more lines"))
    );

    let mut screen = Screen::new(Theme::default(), 60);
    screen.ask_edit("src/lib.rs", DIFF);
    let rows: Vec<String> = screen
        .rows_after("│ src/lib.rs  +2 −3", HISTORY_ROWS.len())
        .iter()
        .map(|row| inside(row))
        .collect();
    let expected: Vec<&str> = HISTORY_ROWS.iter().map(|row| &row[6..]).collect();
    assert_eq!(rows, expected);
    assert!(screen.lines().iter().all(|line| !line.contains("@@")));
}

#[test]
fn approval_box_shows_control_characters_as_escapes() {
    let mut screen = Screen::new(theme(Depth::TrueColor, Background::Dark), 40);
    let diff = "--- a/run.sh\n+++ b/run.sh\n@@ -1,1 +1,1 @@\n-echo hi\n+echo \u{1b}[2Jhi\r\n";
    screen.ask_edit("run.sh", diff);
    let rows: Vec<String> = screen
        .rows_after("│ run.sh  +1 −1", 2)
        .iter()
        .map(|row| inside(row))
        .collect();
    assert_eq!(rows, ["1 - echo hi", "1 + echo \\x1b[2Jhi\\r"]);
}

/// Comparing very long lines word by word every frame could stall the
/// screen, so a pair with a line over 1024 bytes is not compared: its lines
/// keep their colour, no words marked. Shorter pairs still are.
#[test]
fn approval_box_does_not_compare_very_long_lines_word_by_word() {
    let numbers = |last: u32| {
        let mut line: String = (0..300).map(|n| format!("{n}, ")).collect();
        line.push_str(&last.to_string());
        line
    };
    let (old, new) = (numbers(1), numbers(2));
    assert!(old.len() > 1024 && new.len() > 1024);
    let marked = |cell: &Cell| {
        cell.modifier.contains(Modifier::REVERSED)
            || cell.bg == Color::Rgb(0x73, 0x32, 0x2a)
            || cell.bg == Color::Rgb(0x2e, 0x5c, 0x3f)
    };
    for theme in [Theme::default(), theme(Depth::TrueColor, Background::Dark)] {
        let mut screen = Screen::new(theme, 60);
        let diff =
            format!("--- a/n.txt\n+++ b/n.txt\n@@ -1,2 +1,2 @@\n-x = 1\n-{old}\n+x = 2\n+{new}\n");
        screen.ask_edit("n.txt", &diff);
        let buffer = screen.draw();
        for short in ["- x = 1", "+ x = 2"] {
            assert!(
                row_cells(buffer, short).iter().any(|cell| marked(cell)),
                "{theme:?}: {short} is compared"
            );
        }
        let mut long_rows = 0;
        for y in 0..buffer.area.height {
            if row(buffer, y).contains(", ") {
                long_rows += 1;
                for x in 0..buffer.area.width {
                    assert!(
                        !marked(&buffer[(x, y)]),
                        "{theme:?}: row {y} has marked words"
                    );
                }
            }
        }
        assert!(long_rows > 4, "{theme:?}: the long lines are on screen");
    }
}
