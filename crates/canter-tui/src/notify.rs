//! Notifications when the user is wanted (P2-27), as Codex's (`rust-v0.163.0`
//! `tui/src/notifications/`, `chatwidget/notifications.rs`): a turn done, a
//! request for approval, a question from the model. Only while the terminal
//! window is not in front, by default; a terminal that never reports focus
//! counts as in front, so it gets none.
//!
//! How, with the default `auto`: OSC 9, a desktop notification, only in
//! Ghostty itself, by the same rule as the tab mark in [`crate::tab_title`]
//! (terminals emulated inside another one inherit its variables, and some
//! of them would take OSC 9 for something else); a bell (BEL) everywhere
//! else, tmux and screen included.
//!
//! Settings are in `tui.json`, as Codex's three: `notifications` (true,
//! false, or a list of the kinds wanted), `notificationMethod` (`auto`,
//! `osc9`, `bel`) and `notificationCondition` (`unfocused`, `always`).

use std::collections::BTreeMap;

use canter_protocol::{Item, PendingRequest, RequestKind, SessionSnapshot};
use serde_json::Value;

/// Longest answer shown when a turn is done, in characters, as Codex's.
const ANSWER_CHARS: usize = 200;
/// Longest command or question shown, in characters, as Codex's.
const SHORT_CHARS: usize = 30;

/// What the user is wanted for; named in `notifications` as [`Kind::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    TurnComplete,
    ApprovalRequested,
    Question,
}

const KINDS: [Kind; 3] = [Kind::TurnComplete, Kind::ApprovalRequested, Kind::Question];

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::TurnComplete => "turn-complete",
            Kind::ApprovalRequested => "approval-requested",
            Kind::Question => "question",
        }
    }

    /// Waiting for the user goes before a turn being done.
    fn priority(self) -> u8 {
        match self {
            Kind::TurnComplete => 0,
            Kind::ApprovalRequested | Kind::Question => 1,
        }
    }
}

/// One notification and its text, without control, bidi or invisible
/// characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    pub kind: Kind,
    pub text: String,
}

impl Notification {
    /// A turn done: the start of the answer, else `Turn complete`.
    pub(crate) fn turn_complete(answer: &str) -> Self {
        let answer = cut(&clean(answer), ANSWER_CHARS);
        Self {
            kind: Kind::TurnComplete,
            text: if answer.is_empty() {
                "Turn complete".to_owned()
            } else {
                answer
            },
        }
    }

    /// The model waits for `request` in `snapshot`.
    pub(crate) fn request(request: &PendingRequest, snapshot: &SessionSnapshot) -> Option<Self> {
        match &request.kind {
            RequestKind::PlanApproval(_) => Some(Self {
                kind: Kind::ApprovalRequested,
                text: "Implement this plan?".into(),
            }),
            RequestKind::Approval(approval) => {
                let item = snapshot
                    .turns
                    .iter()
                    .find(|turn| turn.id == request.turn_id)
                    .and_then(|turn| {
                        turn.items
                            .iter()
                            .find(|item| item.id() == Some(approval.item_id.as_str()))
                    });
                let text = |key: &str| approval.arguments.get(key).and_then(Value::as_str);
                let cwd = &snapshot.settings.cwd;
                let what = match (approval.tool.as_str(), item) {
                    ("shell", item) => {
                        let command = match item {
                            Some(Item::CommandExecution { command, .. }) => command.as_str(),
                            _ => "",
                        };
                        let command = text("command").unwrap_or(command);
                        format!("Approval requested: {}", cut(&clean(command), SHORT_CHARS))
                    }
                    ("write" | "edit", item) => {
                        let paths: Vec<&str> = match item {
                            Some(Item::FileChange { changes, .. }) if !changes.is_empty() => {
                                changes.iter().map(|change| change.path.as_str()).collect()
                            }
                            _ => text("resolvedPath").or(text("path")).into_iter().collect(),
                        };
                        match paths.as_slice() {
                            [path] => format!(
                                "canter wants to edit {}",
                                clean(&crate::cells::display_path(path, cwd))
                            ),
                            paths => format!("canter wants to edit {} files", paths.len()),
                        }
                    }
                    ("web_fetch", _) => {
                        let url = text("resolvedUrl").or(text("url")).unwrap_or_default();
                        format!("Approval requested: {}", cut(&clean(url), SHORT_CHARS))
                    }
                    (_, Some(Item::McpToolCall { server, .. })) => {
                        format!("Approval requested by {}", clean(server))
                    }
                    (tool, _) => format!("Approval requested: {}", cut(&clean(tool), SHORT_CHARS)),
                };
                Some(Self {
                    kind: Kind::ApprovalRequested,
                    text: what,
                })
            }
            RequestKind::UserInput(input) => Some(Self {
                kind: Kind::Question,
                text: format!("Question: {}", cut(&clean(&input.question), SHORT_CHARS)),
            }),
            RequestKind::Unknown => None,
        }
    }
}

/// As the tab title's: no control, bidi or invisible characters, runs of
/// whitespace as one space, none at either end.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if crate::tab_title::hidden(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// At most `max` characters, the last three `...` when cut, as Codex's
/// `truncate_text`.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Auto,
    Osc9,
    Bel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Condition {
    Unfocused,
    Always,
}

/// What `tui.json` says about notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    kinds: Vec<Kind>,
    method: Method,
    condition: Condition,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kinds: KINDS.to_vec(),
            method: Method::Auto,
            condition: Condition::Unfocused,
        }
    }
}

impl Settings {
    /// The settings in `tui.json` under `env`'s home, and a warning for each
    /// value not known, which is taken as its default. A file that is not
    /// there or cannot be read gives the defaults.
    pub(crate) fn load(env: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let saved = crate::tui_json::path(env)
            .and_then(|path| crate::tui_json::read(&path).ok().flatten())
            .unwrap_or(Value::Null);
        Self::from_json(&saved)
    }

    fn from_json(saved: &Value) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut warnings = Vec::new();
        match saved.get("notifications") {
            None => {}
            Some(Value::Bool(true)) => {}
            Some(Value::Bool(false)) => settings.kinds.clear(),
            Some(Value::Array(names)) => {
                let kinds: Option<Vec<Kind>> = names
                    .iter()
                    .map(|name| {
                        KINDS
                            .into_iter()
                            .find(|kind| name.as_str() == Some(kind.name()))
                    })
                    .collect();
                match kinds {
                    Some(kinds) => settings.kinds = kinds,
                    None => warnings.push(unknown(
                        "notifications",
                        "true, false or a list of turn-complete, approval-requested, question",
                        "true",
                    )),
                }
            }
            Some(_) => warnings.push(unknown(
                "notifications",
                "true, false or a list of turn-complete, approval-requested, question",
                "true",
            )),
        }
        match saved.get("notificationMethod").map(Value::as_str) {
            None | Some(Some("auto")) => {}
            Some(Some("osc9")) => settings.method = Method::Osc9,
            Some(Some("bel")) => settings.method = Method::Bel,
            Some(_) => warnings.push(unknown("notificationMethod", "auto, osc9 or bel", "auto")),
        }
        match saved.get("notificationCondition").map(Value::as_str) {
            None | Some(Some("unfocused")) => {}
            Some(Some("always")) => settings.condition = Condition::Always,
            Some(_) => warnings.push(unknown(
                "notificationCondition",
                "unfocused or always",
                "unfocused",
            )),
        }
        (settings, warnings)
    }
}

fn unknown(key: &str, known: &str, default: &str) -> String {
    format!("tui.json: {key} should be {known}; using {default}")
}

/// Writes notifications to the terminal as the settings say.
pub(crate) struct Notifier {
    settings: Settings,
    /// OSC 9 rather than a bell.
    osc9: bool,
    /// Through tmux, which passes OSC 9 on only wrapped, as Codex's.
    tmux: bool,
}

impl Notifier {
    pub(crate) fn new(settings: Settings, env: &BTreeMap<String, String>) -> Self {
        let osc9 = match settings.method {
            Method::Auto => crate::tab_title::shows_progress(env),
            Method::Osc9 => true,
            Method::Bel => false,
        };
        Self {
            settings,
            osc9,
            tmux: env.get("TMUX").is_some_and(|value| !value.is_empty()),
        }
    }

    /// What to write for the notifications since the last frame, the window
    /// in front or not: the first of the most urgent kind wanted, or none.
    pub(crate) fn sequence(&self, pending: Vec<Notification>, focused: bool) -> Option<String> {
        if focused && self.settings.condition == Condition::Unfocused {
            return None;
        }
        let mut chosen: Option<Notification> = None;
        for notification in pending {
            if !self.settings.kinds.contains(&notification.kind) {
                continue;
            }
            // As Codex's: a later one replaces one not more urgent.
            if chosen
                .as_ref()
                .is_none_or(|chosen| chosen.kind.priority() <= notification.kind.priority())
            {
                chosen = Some(notification);
            }
        }
        let notification = chosen?;
        Some(match (self.osc9, self.tmux) {
            (false, _) => "\x07".to_owned(),
            (true, false) => format!("\x1b]9;{}\x07", notification.text),
            // The text has no ESC to double.
            (true, true) => format!("\x1bPtmux;\x1b\x1b]9;{}\x07\x1b\\", notification.text),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    const GHOSTTY: [(&str, &str); 2] = [("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-ghostty")];

    fn done(text: &str) -> Notification {
        Notification::turn_complete(text)
    }

    fn waiting(text: &str) -> Notification {
        Notification {
            kind: Kind::ApprovalRequested,
            text: text.to_owned(),
        }
    }

    fn question(text: &str) -> Notification {
        Notification {
            kind: Kind::Question,
            text: text.to_owned(),
        }
    }

    fn notifier(saved: Value, term: &[(&str, &str)]) -> Notifier {
        let (settings, warnings) = Settings::from_json(&saved);
        assert!(warnings.is_empty(), "{warnings:?}");
        Notifier::new(settings, &env(term))
    }

    #[test]
    fn ghostty_itself_gets_osc_9_others_the_bell() {
        let ghostty = notifier(Value::Null, &GHOSTTY);
        assert_eq!(
            ghostty.sequence(vec![done("Done.")], false).as_deref(),
            Some("\x1b]9;Done.\x07")
        );
        for term in [
            // A terminal emulated inside Ghostty keeps TERM_PROGRAM but sets its own TERM.
            &[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-256color")][..],
            &[
                ("TERM_PROGRAM", "iTerm.app"),
                ("LC_TERMINAL", "iTerm2"),
                ("TERM", "xterm-256color"),
            ],
            &[("TERM_PROGRAM", "WezTerm"), ("TERM", "xterm-256color")],
            &[("TERM_PROGRAM", "Apple_Terminal")],
            &[
                GHOSTTY[0],
                GHOSTTY[1],
                ("TMUX", "/tmp/tmux-501/default,1,0"),
            ],
            &[GHOSTTY[0], GHOSTTY[1], ("STY", "123.pts-0.host")],
            &[],
        ] {
            let other = notifier(Value::Null, term);
            assert_eq!(
                other.sequence(vec![done("Done.")], false).as_deref(),
                Some("\x07"),
                "{term:?}"
            );
        }
    }

    #[test]
    fn the_method_can_be_chosen() {
        let osc9 = notifier(json!({ "notificationMethod": "osc9" }), &[]);
        assert_eq!(
            osc9.sequence(vec![done("Done.")], false).as_deref(),
            Some("\x1b]9;Done.\x07")
        );
        let tmux = notifier(
            json!({ "notificationMethod": "osc9" }),
            &[("TMUX", "/tmp/tmux-501/default,1,0")],
        );
        assert_eq!(
            tmux.sequence(vec![done("Done.")], false).as_deref(),
            Some("\x1bPtmux;\x1b\x1b]9;Done.\x07\x1b\\")
        );
        let bel = notifier(json!({ "notificationMethod": "bel" }), &GHOSTTY);
        assert_eq!(
            bel.sequence(vec![done("Done.")], false).as_deref(),
            Some("\x07")
        );
        let auto = notifier(json!({ "notificationMethod": "auto" }), &GHOSTTY);
        assert_eq!(
            auto.sequence(vec![done("Done.")], false).as_deref(),
            Some("\x1b]9;Done.\x07")
        );
    }

    #[test]
    fn only_while_the_window_is_behind_unless_always() {
        let unfocused = notifier(Value::Null, &GHOSTTY);
        assert_eq!(unfocused.sequence(vec![done("Done.")], true), None);
        assert!(unfocused.sequence(vec![done("Done.")], false).is_some());
        let always = notifier(json!({ "notificationCondition": "always" }), &GHOSTTY);
        assert!(always.sequence(vec![done("Done.")], true).is_some());
        assert!(always.sequence(vec![done("Done.")], false).is_some());
        assert_eq!(unfocused.sequence(Vec::new(), false), None);
    }

    #[test]
    fn one_at_a_time_waiting_before_done() {
        let notifier = notifier(Value::Null, &GHOSTTY);
        let sent = |pending| notifier.sequence(pending, false).unwrap();
        assert_eq!(
            sent(vec![waiting("Approval requested: ls"), done("Done.")]),
            "\x1b]9;Approval requested: ls\x07"
        );
        assert_eq!(
            sent(vec![done("Done."), question("Question: Which?")]),
            "\x1b]9;Question: Which?\x07"
        );
        // Of the same urgency, the latest.
        assert_eq!(
            sent(vec![done("First."), done("Second.")]),
            "\x1b]9;Second.\x07"
        );
        assert_eq!(
            sent(vec![
                question("Question: a"),
                waiting("Approval requested: b")
            ]),
            "\x1b]9;Approval requested: b\x07"
        );
    }

    #[test]
    fn kinds_can_be_turned_off() {
        let off = notifier(json!({ "notifications": false }), &GHOSTTY);
        assert_eq!(off.sequence(vec![done("x"), waiting("y")], false), None);
        let on = notifier(json!({ "notifications": true }), &GHOSTTY);
        assert!(on.sequence(vec![done("x")], false).is_some());
        let some = notifier(
            json!({ "notifications": ["turn-complete", "question"] }),
            &GHOSTTY,
        );
        // Not the approval, which is not wanted: the turn done instead.
        assert_eq!(
            some.sequence(vec![done("x"), waiting("y")], false)
                .as_deref(),
            Some("\x1b]9;x\x07")
        );
        assert_eq!(some.sequence(vec![waiting("y")], false), None);
        assert!(some.sequence(vec![question("z")], false).is_some());
        let none = notifier(json!({ "notifications": [] }), &GHOSTTY);
        assert_eq!(none.sequence(vec![done("x"), question("z")], false), None);
    }

    #[test]
    fn values_not_known_are_the_defaults_with_a_warning_each() {
        let (settings, warnings) = Settings::from_json(&json!({
            "notifications": ["turn-complete", "agent-turn-complete"],
            "notificationMethod": "popup",
            "notificationCondition": 1,
        }));
        assert_eq!(settings, Settings::default());
        assert_eq!(
            warnings,
            [
                "tui.json: notifications should be true, false or a list of turn-complete, \
                 approval-requested, question; using true",
                "tui.json: notificationMethod should be auto, osc9 or bel; using auto",
                "tui.json: notificationCondition should be unfocused or always; using unfocused",
            ]
        );
        let (settings, warnings) = Settings::from_json(&json!({ "notifications": "yes" }));
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
        let (settings, warnings) = Settings::from_json(&json!({ "theme": "terminal" }));
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn settings_are_read_from_tui_json_under_canter_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let env = env(&[("HOME", "/nowhere"), ("CANTER_HOME", home)]);
        assert_eq!(Settings::load(&env), (Settings::default(), Vec::new()));
        std::fs::write(
            dir.path().join("tui.json"),
            r#"{"notificationCondition":"always","notificationMethod":"bel","notifications":["question"]}"#,
        )
        .unwrap();
        let (settings, warnings) = Settings::load(&env);
        assert!(warnings.is_empty());
        assert_eq!(
            settings,
            Settings {
                kinds: vec![Kind::Question],
                method: Method::Bel,
                condition: Condition::Always,
            }
        );
        std::fs::write(dir.path().join("tui.json"), "not json").unwrap();
        assert_eq!(Settings::load(&env), (Settings::default(), Vec::new()));
    }

    #[test]
    fn a_turn_done_shows_the_start_of_the_answer() {
        assert_eq!(
            done("  Fixed   the\n\nbug.\tAll tests pass.  ").text,
            "Fixed the bug. All tests pass."
        );
        assert_eq!(done("").text, "Turn complete");
        assert_eq!(done(" \n\t ").text, "Turn complete");
        let long = done(&"答".repeat(300)).text;
        assert_eq!(long.chars().count(), 200);
        assert!(long.ends_with("答..."));
        assert_eq!(done(&"a".repeat(200)).text, "a".repeat(200));
    }

    #[test]
    fn control_bidi_and_invisible_characters_are_left_out() {
        assert_eq!(
            done("ok\x1b]9;evil\x07\x1b[31mred\x1b\\\u{9c}").text,
            "ok]9;evil[31mred\\"
        );
        assert_eq!(
            done("a\u{202E}b\u{2066}c\u{200F}d\u{061C}e\u{200B}f\u{FEFF}g\u{2060}h").text,
            "abcdefgh"
        );
    }

    fn snapshot(turn_items: Value, request: Value) -> (SessionSnapshot, PendingRequest) {
        let mut snapshot = SessionSnapshot::empty("s1");
        snapshot.settings.cwd = "/work/canter".to_owned();
        snapshot.turns.push(
            serde_json::from_value(json!({
                "id": "t1", "items": turn_items, "status": "inProgress", "error": null,
                "input": [], "clientName": "c", "usage": null,
            }))
            .unwrap(),
        );
        let request = serde_json::from_value(json!({
            "id": "r1", "turnId": "t1", "createdAt": 0, "kind": request,
        }))
        .unwrap();
        (snapshot, request)
    }

    fn approval(tool: &str, item_id: &str, arguments: Value) -> Value {
        json!({
            "type": "approval", "itemId": item_id, "tool": tool,
            "arguments": arguments, "availableDecisions": ["accept", "decline"],
        })
    }

    fn requested(turn_items: Value, request: Value) -> Notification {
        let (snapshot, request) = snapshot(turn_items, request);
        Notification::request(&request, &snapshot).unwrap()
    }

    #[test]
    fn approvals_say_what_for() {
        let command = requested(
            json!([]),
            approval(
                "shell",
                "c1",
                json!({ "command": "cargo nextest run --all-targets --workspace" }),
            ),
        );
        assert_eq!(command.kind, Kind::ApprovalRequested);
        assert_eq!(
            command.text,
            "Approval requested: cargo nextest run --all-tar..."
        );
        let short = requested(
            json!([]),
            approval("shell", "c1", json!({ "command": "ls\n-la" })),
        );
        assert_eq!(short.text, "Approval requested: ls -la");
        let edit = requested(
            json!([]),
            approval(
                "edit",
                "e1",
                json!({ "path": "src/app.rs", "resolvedPath": "/work/canter/src/app.rs" }),
            ),
        );
        assert_eq!(edit.text, "canter wants to edit src/app.rs");
        let write = requested(
            json!([]),
            approval("write", "w1", json!({ "path": "/elsewhere/notes.md" })),
        );
        assert_eq!(write.text, "canter wants to edit /elsewhere/notes.md");
        let changes = requested(
            json!([{ "type": "fileChange", "id": "e2", "status": "inProgress", "changes": [
                { "path": "/work/canter/a.rs", "kind": { "type": "update" }, "diff": "" },
                { "path": "/work/canter/b.rs", "kind": { "type": "add" }, "diff": "" },
            ]}]),
            approval("edit", "e2", json!({ "path": "a.rs" })),
        );
        assert_eq!(changes.text, "canter wants to edit 2 files");
        let fetch = requested(
            json!([]),
            approval(
                "web_fetch",
                "f1",
                json!({ "url": "https://docs.rs/ratatui" }),
            ),
        );
        assert_eq!(fetch.text, "Approval requested: https://docs.rs/ratatui");
        let mcp = requested(
            json!([{ "type": "mcpToolCall", "id": "m1", "server": "git\x1bhub", "tool": "search",
                     "arguments": {}, "result": "", "status": "inProgress" }]),
            approval("mcp__github__search", "m1", json!({})),
        );
        assert_eq!(mcp.text, "Approval requested by github");
        let other = requested(json!([]), approval("something", "x1", json!({})));
        assert_eq!(other.text, "Approval requested: something");
    }

    /// The session screen, fed server events.
    struct Session {
        app: crate::App,
        seq: u64,
    }

    impl Session {
        fn new() -> Self {
            let mut snapshot = SessionSnapshot::empty("s1");
            snapshot.settings.cwd = "/work/canter".to_owned();
            Self {
                app: crate::App::new(snapshot, None, 0),
                seq: 0,
            }
        }

        fn event(&mut self, payload: Value) {
            self.seq += 1;
            let mut event = json!({ "seq": self.seq, "sessionId": "s1", "timestamp": 0 });
            event
                .as_object_mut()
                .unwrap()
                .extend(payload.as_object().unwrap().clone());
            let event: canter_protocol::Event = serde_json::from_value(event).unwrap();
            self.app.apply_event(&event, 0);
        }

        fn turn(&mut self, id: &str, status: &str) {
            let turn = json!({
                "id": id, "items": [], "status": status, "error": null,
                "input": [{ "type": "text", "text": "hi" }],
                "clientName": "c", "usage": null,
            });
            if status == "inProgress" {
                self.event(json!({ "type": "turn.started", "turn": turn }));
            } else {
                self.event(json!({ "type": "turn.completed", "turn": turn }));
            }
        }

        fn answer(&mut self, turn: &str, id: &str, text: &str) {
            let item = json!({ "type": "agentMessage", "id": id, "text": text });
            self.event(json!({ "type": "item.completed", "turnId": turn, "item": item }));
        }

        fn texts(&mut self) -> Vec<String> {
            self.app
                .take_notifications()
                .into_iter()
                .map(|notification| notification.text)
                .collect()
        }
    }

    #[test]
    fn the_session_screen_collects_what_the_user_is_wanted_for() {
        let mut session = Session::new();
        session.turn("t1", "inProgress");
        session.answer("t1", "a1", "Looking.");
        assert!(session.texts().is_empty());
        session.event(json!({ "type": "request.opened", "request": {
            "id": "r1", "turnId": "t1", "createdAt": 0,
            "kind": approval("shell", "c1", json!({ "command": "ls" })),
        }}));
        session.event(json!({ "type": "request.opened", "request": {
            "id": "q1", "turnId": "t1", "createdAt": 0,
            "kind": { "type": "userInput", "itemId": "u1", "question": "Which?", "options": [] },
        }}));
        assert_eq!(
            session.texts(),
            ["Approval requested: ls", "Question: Which?"]
        );
        assert!(session.texts().is_empty(), "taken once");
        // The last answer of the turn.
        session.answer("t1", "a2", "All\ndone.");
        session.turn("t1", "completed");
        assert_eq!(session.texts(), ["All done."]);
        // Without an answer.
        session.turn("t2", "inProgress");
        session.turn("t2", "completed");
        assert_eq!(session.texts(), ["Turn complete"]);
    }

    #[test]
    fn not_for_a_turn_failed_interrupted_or_followed_by_a_queued_message() {
        let mut session = Session::new();
        session.turn("t1", "inProgress");
        session.turn("t1", "failed");
        session.turn("t2", "inProgress");
        session.turn("t2", "interrupted");
        assert!(session.texts().is_empty());
        session.turn("t3", "inProgress");
        session.event(json!({ "type": "input.queued", "input": {
            "id": "i1", "input": [{ "type": "text", "text": "next" }], "clientName": "c",
        }}));
        session.turn("t3", "completed");
        assert!(session.texts().is_empty());
    }

    #[test]
    fn questions_say_the_start_of_the_question() {
        let asked = requested(
            json!([]),
            json!({ "type": "userInput", "itemId": "q1",
                    "question": "Which of the\ntwo databases should the migration target?",
                    "options": [] }),
        );
        assert_eq!(asked.kind, Kind::Question);
        assert_eq!(asked.text, "Question: Which of the two databases ...");
    }
}
