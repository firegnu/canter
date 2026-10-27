//! A click in the input box puts the cursor where it clicked (P2-93): on
//! the character drawn there, at the end of a row past its last one, at the
//! end of the draft below the last row, and on a marker at its nearer side.

use canter_protocol::SessionSnapshot;
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use unicode_width::UnicodeWidthStr;

/// 25 columns for the draft: the box takes 6 of the screen's.
const WIDTH: u16 = 31;
const HEIGHT: u16 = 24;

struct Screen {
    ui: Ui<TestBackend>,
}

impl Screen {
    fn new(width: u16) -> Self {
        let mut snapshot = SessionSnapshot::empty("click-cursor-test");
        snapshot.settings.model = "deepseek-flash".into();
        let app = App::new(snapshot, None, 0).with_clipboard_reader(|path| {
            std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
            Ok(true)
        });
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, HEIGHT), app).unwrap(),
        };
        screen.draw();
        screen
    }

    /// A screen whose draft is `text`, the cursor at its end.
    fn with(text: &str) -> Self {
        let mut screen = Self::new(WIDTH);
        screen.paste(text);
        screen
    }

    fn draw(&mut self) {
        self.ui.draw(0).unwrap();
    }

    fn paste(&mut self, text: &str) {
        self.ui.app_mut().handle_paste(text);
        self.draw();
    }

    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        let action = self
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(code, modifiers), 0);
        self.draw();
        action
    }

    fn lines(&self) -> Vec<String> {
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row
            })
            .collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The cell where `text` first starts in the input box.
    fn find(&self, text: &str) -> (u16, u16) {
        let lines = self.lines();
        let top = self.box_top();
        for (y, row) in lines.iter().enumerate().skip(usize::from(top)) {
            if let Some(byte) = row.find(text) {
                return (row[..byte].width() as u16, y as u16);
            }
        }
        panic!("{text:?} is not in the input box:\n{}", self.text());
    }

    /// The input box's top border: the last one on screen.
    fn box_top(&self) -> u16 {
        let lines = self.lines();
        lines.iter().rposition(|row| row.starts_with('╭')).unwrap() as u16
    }

    /// The input box's bottom border.
    fn box_bottom(&self) -> u16 {
        let lines = self.lines();
        lines.iter().rposition(|row| row.starts_with('╰')).unwrap() as u16
    }

    fn click(&mut self, (column, row): (u16, u16)) {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let event = MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };
            self.ui.app_mut().handle_mouse_at(event, 1_000);
        }
        self.draw();
    }

    /// Types `|` where the cursor is and sends the draft; returns its text.
    fn with_cursor(&mut self) -> String {
        self.key(KeyCode::Char('|'), KeyModifiers::NONE);
        match self.key(KeyCode::Enter, KeyModifiers::NONE) {
            Some(Action::Send(text)) => text,
            Some(Action::SendImages { text, .. }) => text,
            other => panic!("expected the draft to be sent: {other:?}"),
        }
    }
}

fn right(at: (u16, u16), columns: u16) -> (u16, u16) {
    (at.0 + columns, at.1)
}

#[test]
fn a_click_on_a_line_puts_the_cursor_on_that_character() {
    let draft = "first line\nsecond line\nthird";
    let mut screen = Screen::with(draft);
    let at = screen.find("cond");
    screen.click(at);
    assert_eq!(screen.with_cursor(), "first line\nse|cond line\nthird");

    let mut screen = Screen::with(draft);
    let at = screen.find("first");
    screen.click(at);
    assert_eq!(screen.with_cursor(), "|first line\nsecond line\nthird");

    // Typing goes on from there, and the keys move it as before.
    let mut screen = Screen::with(draft);
    let at = screen.find("line");
    screen.click(at);
    screen.key(KeyCode::Char('X'), KeyModifiers::NONE);
    screen.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(screen.with_cursor(), "first Xl|ine\nsecond line\nthird");
}

#[test]
fn past_the_text_of_a_row_the_cursor_goes_to_its_end() {
    let draft = "first line\nsecond line\nthird";
    let mut screen = Screen::with(draft);
    let (_, y) = screen.find("first");
    screen.click((20, y));
    assert_eq!(screen.with_cursor(), "first line|\nsecond line\nthird");

    // Also on the padding and the border at the right.
    let mut screen = Screen::with(draft);
    let (_, y) = screen.find("second");
    screen.click((WIDTH - 1, y));
    assert_eq!(screen.with_cursor(), "first line\nsecond line|\nthird");
}

#[test]
fn below_the_last_row_the_cursor_goes_to_the_end() {
    let mut screen = Screen::with("first line\nsecond line\nthird");
    screen.key(KeyCode::Up, KeyModifiers::NONE);
    screen.key(KeyCode::Up, KeyModifiers::NONE);
    let below = screen.box_bottom();
    assert_eq!(screen.find("third").1 + 1, below, "{}", screen.text());
    screen.click((10, below));
    assert_eq!(screen.with_cursor(), "first line\nsecond line\nthird|");
}

#[test]
fn wrapped_rows_and_chinese_go_by_the_columns_drawn() {
    // 25 columns hold 12 wide characters and one blank column.
    let draft = "一二三四五六七八九十甲乙丙丁";
    let screen = Screen::with(draft);
    let first = screen.find("一");
    let second = screen.find("丙");
    assert_eq!(second.1, first.1 + 1, "{}", screen.text());

    // Either column of a wide character is on it.
    for column in [4, 5] {
        let mut screen = Screen::with(draft);
        screen.click(right(first, column));
        assert_eq!(screen.with_cursor(), "一二|三四五六七八九十甲乙丙丁");
    }

    // On the second row, as drawn.
    let mut screen = Screen::with(draft);
    screen.key(KeyCode::Home, KeyModifiers::NONE);
    screen.click(right(second, 2));
    assert_eq!(screen.with_cursor(), "一二三四五六七八九十甲乙丙|丁");

    // The blank column after the first row's last character: its end.
    let mut screen = Screen::with(draft);
    screen.click(right(first, 24));
    assert_eq!(screen.with_cursor(), "一二三四五六七八九十甲乙|丙丁");

    // Mixed with ASCII, wrapped by columns.
    let draft = "abc中文字符def和更多的中文字符写在后面";
    let mut screen = Screen::with(draft);
    let at = screen.find("后面");
    screen.click(right(at, 3));
    assert_eq!(
        screen.with_cursor(),
        "abc中文字符def和更多的中文字符写在后|面"
    );
}

#[test]
fn on_a_marker_the_cursor_goes_to_the_nearer_side() {
    let paste: String = (1..=11)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let folded = || {
        let mut screen = Screen::new(60);
        screen.paste("ab");
        screen.paste(&paste);
        screen.paste("cd");
        assert!(screen.text().contains("ab[Pasted #1 · 11 lines]cd"));
        screen
    };
    let mut screen = folded();
    let at = screen.find("Pasted");
    screen.click(at);
    assert_eq!(screen.with_cursor(), format!("ab|{paste}cd"));

    let mut screen = folded();
    let at = screen.find("lines]");
    screen.click(at);
    assert_eq!(screen.with_cursor(), format!("ab{paste}|cd"));

    // The marker stays whole: typing goes before or after it.
    let mut screen = folded();
    let at = screen.find("#1");
    screen.click(at);
    screen.key(KeyCode::Char('X'), KeyModifiers::NONE);
    screen.key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(screen.with_cursor(), format!("abX{paste}|cd"));

    let image = || {
        let mut screen = Screen::new(60);
        screen.paste("看");
        screen.key(KeyCode::Char('v'), KeyModifiers::CONTROL);
        screen.paste("这个");
        assert!(screen.text().contains("看[Image #1 screenshot.png]这个"));
        screen
    };
    let mut screen = image();
    let at = screen.find("#1");
    screen.click(at);
    assert_eq!(screen.with_cursor(), "看|[Image #1 screenshot.png]这个");

    let mut screen = image();
    let at = screen.find("shot");
    screen.click(at);
    assert_eq!(screen.with_cursor(), "看[Image #1 screenshot.png]|这个");
}

#[test]
fn in_a_box_scrolled_the_rows_shown_count() {
    let lines: Vec<String> = (1..=12).map(|i| format!("row {i}")).collect();
    let mut screen = Screen::new(WIDTH);
    // Ten lines at a time stay as they are.
    screen.paste(&lines[..10].join("\n"));
    screen.paste(&format!("\n{}", lines[10..].join("\n")));
    let shown = screen.text();
    assert!(!shown.contains("row 4"), "{shown}");
    assert!(shown.contains("row 5"), "{shown}");

    let top = screen.box_top();
    screen.click((6, top + 1));
    let expected = lines.join("\n").replacen("row 5", "ro|w 5", 1);
    assert_eq!(screen.with_cursor(), expected);

    // Scrolled up to the start, the first row shown is the first line.
    let mut screen = Screen::new(WIDTH);
    screen.paste(&lines[..10].join("\n"));
    screen.paste(&format!("\n{}", lines[10..].join("\n")));
    for _ in 0..11 {
        screen.key(KeyCode::Up, KeyModifiers::NONE);
    }
    let at = screen.find("row 2");
    assert_eq!(at.1, top + 2, "{}", screen.text());
    screen.click((20, at.1));
    let expected = lines.join("\n").replacen("row 2", "row 2|", 1);
    assert_eq!(screen.with_cursor(), expected);
    // Below the last row shown, which is not the last line: the end of that row.
    let mut screen = Screen::new(WIDTH);
    screen.paste(&lines[..10].join("\n"));
    screen.paste(&format!("\n{}", lines[10..].join("\n")));
    for _ in 0..11 {
        screen.key(KeyCode::Up, KeyModifiers::NONE);
    }
    let below = screen.box_bottom();
    screen.click((10, below));
    let expected = lines.join("\n").replacen("row 8", "row 8|", 1);
    assert_eq!(screen.with_cursor(), expected);
}
