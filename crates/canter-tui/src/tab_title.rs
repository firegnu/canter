//! The terminal tab's title (OSC 0) and, in terminals known to show it, a
//! busy or waiting mark on the tab (OSC 9;4).
//!
//! The title is `<mark> canter · <directory>`: the status line's spinner
//! while working, `✋` while a request waits for the user, `✓` / `✗` for how
//! the last turn ended, nothing while idle. On quitting the title is
//! emptied so the terminal shows its own again; the title from before is
//! neither read nor restored (terminals differ, and Codex does the same).
//!
//! OSC 9 is a desktop notification in many terminals, and ones that do not
//! know `9;4` would pop one up; so the mark goes only to Ghostty itself
//! (`TERM_PROGRAM=ghostty` and `TERM=xterm-ghostty`), never through tmux or
//! screen. A terminal emulated inside another one (paddock, herdr) inherits
//! `TERM_PROGRAM` but sets its own `TERM`. iTerm2 gets the title only: it
//! knows `9;4` from 3.6.6 on, older ones pop a notification, and its
//! variables are inherited the same way with nothing to tell them apart.

use std::collections::BTreeMap;
use std::path::Path;

use crate::app::SPINNER;

/// What the tab says about the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabState {
    Working,
    Waiting,
    Done,
    Failed,
    Idle,
}

/// The spinner in the title moves on at most this often (ms), as Codex's.
const SPIN_MS: i64 = 100;
/// Longest title in characters, as Codex's: terminals cut long ones anyway.
const MAX_CHARS: usize = 240;

const BUSY: &str = "\x1b]9;4;3\x07";
/// With a percentage, which some terminals need for state 4.
const PAUSED: &str = "\x1b]9;4;4;100\x07";
/// Never the short `OSC 9;4`, which some terminals crash on.
const CLEAR: &str = "\x1b]9;4;0\x07";

/// The title for `state` in directory `dir`, the spinner at `now_ms`.
pub(crate) fn title(state: TabState, dir: &str, now_ms: i64) -> String {
    let mark = match state {
        TabState::Working => SPINNER[(now_ms.max(0) / SPIN_MS) as usize % SPINNER.len()],
        TabState::Waiting => "✋",
        TabState::Done => "✓",
        TabState::Failed => "✗",
        TabState::Idle => return format!("canter · {dir}"),
    };
    format!("{mark} canter · {dir}")
}

/// The last part of `cwd`; `~` for the home directory itself.
pub(crate) fn dir_name(cwd: &str, home: Option<&str>) -> String {
    let path = Path::new(cwd);
    if home.is_some_and(|home| Path::new(home) == path) {
        return "~".to_owned();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => cwd.to_owned(),
    }
}

/// As Codex's `sanitize_terminal_title`: no control, bidi or invisible
/// characters, whitespace runs as one space, at most [`MAX_CHARS`]. GNU
/// Screen cuts each character to a byte (`⠇` would become BEL and end the
/// sequence), so there only ASCII goes: the spinner as `|/-\`, else `?`.
fn sanitize(title: &str, screen: bool) -> String {
    let mut out = String::new();
    let mut written = 0;
    let mut space = false;
    for c in title.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if hidden(c) {
            continue;
        }
        let c = match c {
            '⠋' | '⠼' | '⠇' if screen => '|',
            '⠙' | '⠴' | '⠏' if screen => '/',
            '⠹' | '⠦' if screen => '-',
            '⠸' | '⠧' if screen => '\\',
            c if screen && !c.is_ascii() => '?',
            c => c,
        };
        if space && MAX_CHARS.saturating_sub(written) > 1 {
            out.push(' ');
            written += 1;
            space = false;
        }
        if written >= MAX_CHARS {
            break;
        }
        out.push(c);
        written += 1;
    }
    out
}

/// Control characters, and formatting ones that reorder or hide text.
pub(crate) fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

fn set(env: &BTreeMap<String, String>, name: &str) -> bool {
    env.get(name).is_some_and(|value| !value.is_empty())
}

/// Whether the terminal in `env` shows OSC 9;4 on its tab: Ghostty itself,
/// not through tmux or screen.
pub(crate) fn shows_progress(env: &BTreeMap<String, String>) -> bool {
    let var = |name: &str| env.get(name).map(String::as_str);
    !set(env, "TMUX")
        && !set(env, "STY")
        && var("TERM_PROGRAM") == Some("ghostty")
        && var("TERM") == Some("xterm-ghostty")
}

/// What canter last wrote to the tab, so it writes only changes.
pub(crate) struct TabTitle {
    progress: bool,
    screen: bool,
    title: Option<String>,
    /// The OSC 9;4 sequence in effect.
    mark: &'static str,
}

impl TabTitle {
    pub(crate) fn new(env: &BTreeMap<String, String>) -> Self {
        Self {
            progress: shows_progress(env),
            // As Codex: screen is an output hop even with tmux inside it.
            screen: env.contains_key("STY"),
            title: None,
            mark: CLEAR,
        }
    }

    /// The sequences that bring the tab to `state` in `dir`; empty when
    /// nothing changed.
    pub(crate) fn update(&mut self, state: TabState, dir: &str, now_ms: i64) -> String {
        let mut out = String::new();
        let title = sanitize(&title(state, dir, now_ms), self.screen);
        if self.title.as_ref() != Some(&title) {
            out.push_str(&format!("\x1b]0;{title}\x07"));
            self.title = Some(title);
        }
        if self.progress {
            let mark = match state {
                TabState::Working => BUSY,
                TabState::Waiting => PAUSED,
                TabState::Done | TabState::Failed | TabState::Idle => CLEAR,
            };
            if mark != self.mark {
                out.push_str(mark);
                self.mark = mark;
            }
        }
        out
    }

    /// An empty title and no mark, for quitting; empty when canter never
    /// wrote to the tab.
    pub(crate) fn leave(&mut self) -> String {
        if self.title.take().is_none() {
            return String::new();
        }
        self.mark = CLEAR;
        let mut out = "\x1b]0;\x07".to_owned();
        if self.progress {
            out.push_str(CLEAR);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canter_protocol::{Event, SessionSnapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::{Value, json};

    const DIR: &str = "canter";

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn each_state_has_its_title() {
        assert_eq!(title(TabState::Working, DIR, 0), "⠋ canter · canter");
        assert_eq!(title(TabState::Waiting, DIR, 0), "✋ canter · canter");
        assert_eq!(title(TabState::Done, DIR, 0), "✓ canter · canter");
        assert_eq!(title(TabState::Failed, DIR, 0), "✗ canter · canter");
        assert_eq!(title(TabState::Idle, DIR, 0), "canter · canter");
    }

    #[test]
    fn the_spinner_moves_on_at_most_every_100_ms() {
        assert_eq!(title(TabState::Working, DIR, 99), "⠋ canter · canter");
        assert_eq!(title(TabState::Working, DIR, 100), "⠙ canter · canter");
        assert_eq!(title(TabState::Working, DIR, 250), "⠹ canter · canter");
        // Ten frames, then round again.
        assert_eq!(title(TabState::Working, DIR, 1000), "⠋ canter · canter");
    }

    #[test]
    fn the_directory_is_its_last_part_and_home_is_a_tilde() {
        let home = Some("/Users/dev");
        assert_eq!(dir_name("/Users/dev/projects/canter", home), "canter");
        assert_eq!(dir_name("/Users/dev/projects/canter/", home), "canter");
        assert_eq!(dir_name("/Users/dev", home), "~");
        assert_eq!(dir_name("/Users/dev/", home), "~");
        assert_eq!(dir_name("/Users/dev", None), "dev");
        assert_eq!(dir_name("/", home), "/");
    }

    #[test]
    fn titles_lose_control_bidi_and_invisible_characters_and_are_cut() {
        assert_eq!(
            sanitize("✓ canter · a\x1b]0;b\x07c\u{9c}\td\ne", false),
            "✓ canter · a]0;bc d e"
        );
        assert_eq!(
            sanitize(
                "ca\u{202E}n\u{2066}t\u{200F}e\u{061C}r\u{200B} \u{FEFF}x\u{2060}",
                false
            ),
            "canter x"
        );
        let long = sanitize(&format!("canter · {}", "a".repeat(400)), false);
        assert_eq!(long.chars().count(), 240);
        assert!(long.starts_with("canter · aaa"));
        // GNU Screen cuts each character to a byte: only ASCII goes there.
        assert_eq!(sanitize("⠇ canter · 项目", true), "| canter ? ??");
    }

    const GHOSTTY: [(&str, &str); 2] = [("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-ghostty")];

    #[test]
    fn the_tab_mark_goes_only_to_ghostty_itself() {
        assert!(shows_progress(&env(&GHOSTTY)));
        // A terminal emulated inside Ghostty keeps TERM_PROGRAM but sets its own TERM.
        let inside = env(&[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-256color")]);
        assert!(!shows_progress(&inside));
        assert!(!shows_progress(&env(&[("TERM_PROGRAM", "ghostty")])));
        for version in ["3.6.5", "3.6.6", "3.7.0"] {
            let env = env(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", version),
            ]);
            assert!(!shows_progress(&env), "iTerm2 {version}");
        }
        assert!(!shows_progress(&env(&[("TERM_PROGRAM", "iTerm.app")])));
        assert!(!shows_progress(&env(&[("TERM_PROGRAM", "Apple_Terminal")])));
        assert!(!shows_progress(&env(&[])));
        assert!(!shows_progress(&env(&[("TERM_PROGRAM", "tmux")])));
        let in_tmux = env(&[
            GHOSTTY[0],
            GHOSTTY[1],
            ("TMUX", "/tmp/tmux-501/default,1,0"),
        ]);
        assert!(!shows_progress(&in_tmux));
        let in_screen = env(&[GHOSTTY[0], GHOSTTY[1], ("STY", "123.pts-0.host")]);
        assert!(!shows_progress(&in_screen));
    }

    #[test]
    fn writes_only_what_changed() {
        let mut tab = TabTitle::new(&env(&GHOSTTY));
        assert_eq!(
            tab.update(TabState::Idle, DIR, 0),
            "\x1b]0;canter · canter\x07"
        );
        assert_eq!(tab.update(TabState::Idle, DIR, 50), "");
        assert_eq!(
            tab.update(TabState::Working, DIR, 0),
            "\x1b]0;⠋ canter · canter\x07\x1b]9;4;3\x07"
        );
        assert_eq!(tab.update(TabState::Working, DIR, 99), "");
        assert_eq!(
            tab.update(TabState::Working, DIR, 100),
            "\x1b]0;⠙ canter · canter\x07"
        );
        assert_eq!(
            tab.update(TabState::Waiting, DIR, 200),
            "\x1b]0;✋ canter · canter\x07\x1b]9;4;4;100\x07"
        );
        assert_eq!(
            tab.update(TabState::Done, DIR, 300),
            "\x1b]0;✓ canter · canter\x07\x1b]9;4;0\x07"
        );
        assert_eq!(
            tab.update(TabState::Working, DIR, 400),
            "\x1b]0;⠼ canter · canter\x07\x1b]9;4;3\x07"
        );
        assert_eq!(
            tab.update(TabState::Failed, DIR, 500),
            "\x1b]0;✗ canter · canter\x07\x1b]9;4;0\x07"
        );
        assert_eq!(
            tab.update(TabState::Idle, DIR, 600),
            "\x1b]0;canter · canter\x07"
        );
        assert_eq!(tab.leave(), "\x1b]0;\x07\x1b]9;4;0\x07");
    }

    #[test]
    fn other_terminals_get_the_title_only() {
        let mut tab = TabTitle::new(&env(&[("TERM_PROGRAM", "Apple_Terminal")]));
        assert_eq!(
            tab.update(TabState::Working, DIR, 0),
            "\x1b]0;⠋ canter · canter\x07"
        );
        assert_eq!(
            tab.update(TabState::Waiting, DIR, 0),
            "\x1b]0;✋ canter · canter\x07"
        );
        assert_eq!(tab.leave(), "\x1b]0;\x07");
    }

    #[test]
    fn nothing_to_leave_before_anything_was_written() {
        let mut tab = TabTitle::new(&env(&GHOSTTY));
        assert_eq!(tab.leave(), "");
    }

    /// The session screen in `/Users/dev/canter`, home `/Users/dev`; keys act
    /// on the frame drawn last.
    struct Screen {
        ui: crate::Ui<ratatui::backend::TestBackend>,
        seq: u64,
        turns: u64,
    }

    impl Screen {
        fn new() -> Self {
            let mut snapshot = SessionSnapshot::empty("s1");
            snapshot.settings.cwd = "/Users/dev/canter".to_owned();
            let app = crate::App::new(snapshot, Some("/Users/dev".to_owned()), 0);
            let ui = crate::Ui::new(ratatui::backend::TestBackend::new(80, 20), app).unwrap();
            Self {
                ui,
                seq: 0,
                turns: 0,
            }
        }

        fn event(&mut self, payload: Value) {
            self.seq += 1;
            let mut event = json!({ "seq": self.seq, "sessionId": "s1", "timestamp": 0 });
            event
                .as_object_mut()
                .unwrap()
                .extend(payload.as_object().unwrap().clone());
            let event: Event = serde_json::from_value(event).unwrap();
            self.ui.app_mut().apply_event(&event, 0);
            self.ui.draw(0).unwrap();
        }

        fn status(&mut self, status: &str) {
            self.event(json!({ "type": "session.status", "status": status }));
        }

        fn turn(&mut self, status: &str) {
            if status == "inProgress" {
                self.turns += 1;
            }
            let turn = json!({
                "id": format!("t{}", self.turns), "items": [], "status": status, "error": null,
                "input": [{ "type": "text", "text": "hi" }],
                "clientName": "canter-tui", "usage": null,
            });
            if status == "inProgress" {
                self.event(json!({ "type": "turn.started", "turn": turn }));
                self.status("working");
            } else {
                self.event(json!({ "type": "turn.completed", "turn": turn }));
                self.status("idle");
            }
        }

        fn key(&mut self, code: KeyCode) -> Option<crate::Action> {
            self.ui
                .app_mut()
                .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0)
        }

        fn title(&self) -> String {
            let app = self.ui.app();
            title(app.tab_state(), &app.tab_dir(), 0)
        }
    }

    #[test]
    fn the_session_screen_says_what_the_turn_is_doing() {
        let mut screen = Screen::new();
        assert_eq!(screen.title(), "canter · canter");
        screen.turn("inProgress");
        assert_eq!(screen.title(), "⠋ canter · canter");
        screen.event(json!({
            "type": "request.opened",
            "request": {
                "id": "r1", "turnId": "t1", "createdAt": 0,
                "kind": {
                    "type": "approval", "itemId": "c1", "tool": "shell",
                    "arguments": { "command": "ls" },
                    "availableDecisions": ["accept", "decline"],
                },
            },
        }));
        screen.status("waiting_approval");
        assert_eq!(screen.title(), "✋ canter · canter");
        screen.event(json!({
            "type": "request.resolved", "requestId": "r1",
            "resolution": { "type": "answered", "decision": "accept" },
            "clientName": "canter-tui b",
        }));
        screen.status("working");
        assert_eq!(screen.title(), "⠋ canter · canter");
        screen.turn("completed");
        assert_eq!(screen.title(), "✓ canter · canter");
        // Stays until a key is pressed in this canter.
        screen.status("idle");
        assert_eq!(screen.title(), "✓ canter · canter");
        assert_eq!(screen.key(KeyCode::Char('x')), None);
        assert_eq!(screen.title(), "canter · canter");
    }

    #[test]
    fn a_question_waits_too() {
        let mut screen = Screen::new();
        screen.turn("inProgress");
        screen.event(json!({"type":"request.opened","request":{
            "id":"q1","turnId":"t1","createdAt":0,"kind":{
                "type":"userInput","itemId":"item-q1",
                "question":"Which?", "options":[
                    {"label":"A","description":"a"},
                    {"label":"B","description":"b"}
                ]
            }
        }}));
        screen.status("waiting_approval");
        assert_eq!(screen.title(), "✋ canter · canter");
    }

    #[test]
    fn a_failed_turn_shows_a_cross_until_a_key() {
        let mut screen = Screen::new();
        screen.turn("inProgress");
        screen.turn("failed");
        assert_eq!(screen.title(), "✗ canter · canter");
        screen.key(KeyCode::Down);
        assert_eq!(screen.title(), "canter · canter");
    }

    #[test]
    fn the_next_turn_takes_the_mark_away() {
        let mut screen = Screen::new();
        screen.turn("inProgress");
        screen.turn("failed");
        screen.turn("inProgress");
        assert_eq!(screen.title(), "⠋ canter · canter");
        // Interrupted elsewhere: no mark left from the turn before.
        screen.turn("interrupted");
        assert_eq!(screen.title(), "canter · canter");
    }

    #[test]
    fn a_turn_interrupted_with_esc_goes_straight_back_to_idle() {
        let mut screen = Screen::new();
        screen.turn("inProgress");
        assert!(matches!(
            screen.key(KeyCode::Esc),
            Some(crate::Action::Interrupt { .. })
        ));
        assert_eq!(screen.title(), "⠋ canter · canter");
        screen.turn("interrupted");
        assert_eq!(screen.title(), "canter · canter");
    }

    #[test]
    fn home_itself_is_a_tilde() {
        let mut snapshot = SessionSnapshot::empty("s1");
        snapshot.settings.cwd = "/Users/dev".to_owned();
        let app = crate::App::new(snapshot, Some("/Users/dev".to_owned()), 0);
        assert_eq!(title(app.tab_state(), &app.tab_dir(), 0), "canter · ~");
    }
}
