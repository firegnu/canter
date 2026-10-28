use std::{collections::BTreeMap, fs, time::Duration};

use canter_protocol::{PermissionMode, SessionSnapshot, SessionUpdateParams, TokenUsage};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, style::Color};
use serde_json::{Value, json};

use crate::{Action, App, Ui};

struct Screen {
    root: tempfile::TempDir,
    ui: Ui<TestBackend>,
    now: i64,
}

impl Screen {
    fn event(&mut self, at: i64, payload: Value) {
        let mut value = json!({"seq":self.ui.app().snapshot().last_event_seq + 1, "sessionId":"s1", "timestamp":at});
        value
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        self.ui
            .app_mut()
            .apply_event(&serde_json::from_value(value).unwrap(), at);
        self.now = at;
    }
    fn row(&self, y: u16) -> String {
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn mode(&mut self, mode: PermissionMode) {
        let mut settings = self.ui.app().snapshot().settings.clone();
        settings.approval_policy = mode;
        let event = canter_protocol::Event {
            seq: self.ui.app().snapshot().last_event_seq + 1,
            session_id: "s1".into(),
            timestamp: crate::now_ms(),
            payload: canter_protocol::EventPayload::SessionUpdated { settings },
        };
        self.ui.app_mut().apply_event(&event, event.timestamp);
    }
    fn change_model(&mut self, name: &str) {
        let mut settings = self.ui.app().snapshot().settings.clone();
        settings.model = name.into();
        let event = canter_protocol::Event {
            seq: self.ui.app().snapshot().last_event_seq + 1,
            session_id: "s1".into(),
            timestamp: crate::now_ms(),
            payload: canter_protocol::EventPayload::SessionUpdated { settings },
        };
        self.ui.app_mut().apply_event(&event, event.timestamp);
    }
    fn new(script: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("status.sh"), script).unwrap();
        let home = root.path().to_str().unwrap().to_owned();
        let env = BTreeMap::from([
            ("HOME".into(), home.clone()),
            ("CANTER_HOME".into(), home.clone()),
        ]);
        let mut snapshot = SessionSnapshot::empty("s1");
        snapshot.settings.cwd = home.clone();
        snapshot.settings.model = "test-model".into();
        let app = App::new(snapshot, Some(home), 0)
            .with_welcome("0.1.0 · test")
            .with_status_line(Some("sh ./status.sh".into()), env);
        Self {
            root,
            ui: Ui::new(TestBackend::new(80, 12), app).unwrap(),
            now: crate::now_ms(),
        }
    }

    fn bar(&mut self) -> [String; 2] {
        self.ui.draw(self.now).unwrap();
        let buffer = self.ui.backend().buffer();
        [10, 11].map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
    }

    async fn wait_for(&mut self, first: &str) -> [String; 2] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let rows = self.bar();
            if rows[0] == first {
                return rows;
            }
            assert!(tokio::time::Instant::now() < deadline, "bar: {rows:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn tps_estimates_streamed_text_then_uses_actual_tokens_for_each_request() {
    let mut screen = Screen::new("cat > latest.tmp; mv latest.tmp input.json; printf custom");
    screen.wait_for("custom").await;
    assert_eq!(read_input(&screen)["tps"], Value::Null);
    screen.event(
        0,
        json!({"type":"turn.started", "turn":{
            "id":"t1", "items":[], "status":"inProgress", "error":null,
            "input":[{"type":"text","text":"hi"}], "clientName":"test", "usage":null
        }}),
    );
    screen.event(0, json!({"type":"session.status","status":"working"}));
    screen.event(
        900,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"reasoning","id":"r1","summary":[],"content":[]
        }}),
    );
    // First text is at 1000ms; starting the turn and item must not start TPS.
    screen.event(
        1000,
        json!({"type":"item.delta","turnId":"t1","itemId":"r1",
        "delta":{"type":"reasoningContent","contentIndex":0,"delta":"思考"}}),
    );
    screen.event(
        1500,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"agentMessage","id":"m1","text":""
        }}),
    );
    screen.event(
        2000,
        json!({"type":"item.delta","turnId":"t1","itemId":"m1",
        "delta":{"type":"agentMessage","delta":"abcdefg"}}),
    );
    // 6 + 7 UTF-8 bytes = ceil(13 / 3) = 5 tokens, over one second.
    wait_tps(&mut screen, 5.0).await;
    screen.event(
        3000,
        json!({"type":"item.completed","turnId":"t1","item":{
            "type":"agentMessage","id":"m1","text":"abcdefg"
        }}),
    );
    screen.event(
        3000,
        json!({"type":"item.completed","turnId":"t1","item":{
            "type":"reasoning","id":"r1","summary":[],"content":["思考"]
        }}),
    );
    screen.event(3000, usage(20));
    screen.now = 10_000;
    wait_tps(&mut screen, 10.0).await;
    // Another request in this same turn starts a fresh numerator and clock.
    screen.event(
        11_000,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"agentMessage","id":"m2","text":""
        }}),
    );
    screen.event(
        12_000,
        json!({"type":"item.delta","turnId":"t1","itemId":"m2",
        "delta":{"type":"agentMessage","delta":"abc"}}),
    );
    screen.now = 12_500;
    wait_tps(&mut screen, 2.0).await;
    screen.event(
        14_000,
        json!({"type":"item.completed","turnId":"t1","item":{
            "type":"agentMessage","id":"m2","text":"abc"
        }}),
    );
    screen.event(14_000, usage(8));
    screen.event(14_000, json!({"type":"session.status","status":"idle"}));
    screen.now = 100_000;
    wait_tps(&mut screen, 4.0).await;
}

#[tokio::test]
async fn tps_is_null_for_a_request_already_streaming_when_the_terminal_opens() {
    let mut screen = Screen::new("cat > latest.tmp; mv latest.tmp input.json; printf custom");
    screen.event(
        0,
        json!({"type":"turn.started", "turn":{
            "id":"t1", "items":[], "status":"inProgress", "error":null,
            "input":[{"type":"text","text":"hi"}], "clientName":"test", "usage":null
        }}),
    );
    screen.event(0, json!({"type":"session.status","status":"working"}));
    screen.event(
        1000,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"reasoning","id":"r1","summary":[],"content":["earlier output"]
        }}),
    );
    let home = screen.root.path().to_str().unwrap().to_owned();
    *screen.ui.app_mut() = App::new(screen.ui.app().snapshot().clone(), Some(home.clone()), 1500)
        .with_status_line(
            Some("sh ./status.sh".into()),
            BTreeMap::from([("HOME".into(), home.clone()), ("CANTER_HOME".into(), home)]),
        );
    screen.event(
        2000,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"agentMessage","id":"m1","text":""
        }}),
    );
    screen.event(
        3000,
        json!({"type":"item.delta","turnId":"t1","itemId":"m1",
        "delta":{"type":"agentMessage","delta":"abc"}}),
    );
    screen.now = 4000;
    screen.wait_for("custom").await;
    assert_eq!(
        read_input(&screen)["tps"],
        Value::Null,
        "the first reasoning time is unknown"
    );
    screen.event(5000, usage(100));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        screen.bar();
        let input = read_input(&screen);
        if input["context"]["usedTokens"] == 100 {
            assert_eq!(input["tps"], Value::Null);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    screen.event(
        6000,
        json!({"type":"item.started","turnId":"t1","item":{
            "type":"agentMessage","id":"m2","text":""
        }}),
    );
    screen.event(
        7000,
        json!({"type":"item.delta","turnId":"t1","itemId":"m2",
        "delta":{"type":"agentMessage","delta":"abcdef"}}),
    );
    screen.now = 8000;
    wait_tps(&mut screen, 2.0).await;
}

fn usage(output: u64) -> Value {
    let counts = json!({"inputTokens":100,"cachedInputTokens":90,"outputTokens":output,
        "reasoningOutputTokens":2,"totalTokens":100 + output});
    json!({"type":"session.usage","tokenUsage":{"total":counts,"last":counts,
        "contextTokens":100,"modelContextWindow":200000}})
}

fn read_input(screen: &Screen) -> Value {
    serde_json::from_str(&fs::read_to_string(screen.root.path().join("input.json")).unwrap())
        .unwrap()
}

async fn wait_tps(screen: &mut Screen, expected: f64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        screen.bar();
        let input = read_input(screen);
        if input["tps"] == expected {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected TPS {expected}, input {input}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn shift_tab() -> KeyEvent {
    KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
}

fn set_mode(mode: PermissionMode) -> Option<Action> {
    Some(Action::Update(SessionUpdateParams {
        approval_policy: Some(mode),
        ..Default::default()
    }))
}

#[tokio::test]
async fn custom_output_allows_shift_tab_from_its_frame_without_a_visible_mode() {
    let mut screen = Screen::new("cat >/dev/null; printf custom");
    screen.wait_for("custom").await;
    screen.ui.draw(0).unwrap();
    let frame = screen.ui.frames;
    screen.mode(PermissionMode::AskForExecution);
    // A queued key belongs to the previous frame, even after the next was drawn.
    screen.ui.draw(100).unwrap();
    assert_eq!(
        screen.ui.app_mut().handle_key_on(shift_tab(), frame, 0),
        set_mode(PermissionMode::AskForExecution)
    );
    assert_eq!(
        screen.ui.app_mut().handle_key(shift_tab(), 100),
        set_mode(PermissionMode::AllowAll)
    );
    // A command list keeps P2-20's existing guard.
    screen
        .ui
        .app_mut()
        .handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE), 100);
    screen.ui.draw(100).unwrap();
    assert_eq!(screen.ui.app_mut().handle_key(shift_tab(), 100), None);
}

#[tokio::test]
async fn only_custom_output_shift_tab_briefly_reports_the_new_mode_above_the_input() {
    let mut screen = Screen::new("cat >/dev/null; printf custom");
    // Even before a command succeeds, the default row's fully visible mode works.
    screen.mode(PermissionMode::AskForExecution);
    screen.ui.draw(0).unwrap();
    let before = screen.row(6);
    assert_eq!(
        screen.ui.app_mut().handle_key(shift_tab(), 0),
        set_mode(PermissionMode::AllowAll)
    );
    screen.ui.draw(0).unwrap();
    assert_eq!(
        screen.row(6),
        before,
        "default rows keep their original behavior"
    );
    screen.wait_for("custom").await;
    screen.ui.draw(0).unwrap();
    assert_eq!(
        screen.ui.app_mut().handle_key(shift_tab(), 0),
        set_mode(PermissionMode::AllowAll)
    );
    screen.ui.draw(0).unwrap();
    assert_eq!(screen.row(6), " Mode: Allow all");
    assert_eq!(screen.ui.backend().buffer()[(7, 6)].fg, Color::Red);
    screen.ui.draw(1999).unwrap();
    assert_eq!(screen.row(6), " Mode: Allow all");
    screen.ui.draw(2000).unwrap();
    assert_eq!(screen.row(6), before);
}

#[tokio::test]
async fn large_output_is_bounded_per_line_and_still_drained_to_the_next_line() {
    let mut screen = Screen::new(
        "cat >/dev/null\nhead -c 8388608 /dev/zero | tr '\\000' a\nprintf '\\nsecond\\n'",
    );
    let cwd = screen.root.path().to_str().unwrap().to_owned();
    let env = BTreeMap::from([
        ("HOME".into(), cwd.clone()),
        ("CANTER_HOME".into(), cwd.clone()),
    ]);
    let output = super::execute("sh ./status.sh", &env, &cwd, "{}\n")
        .await
        .unwrap();
    assert_eq!(
        output.lines().nth(1),
        Some("second"),
        "drain excess bytes instead of ending the command early"
    );
    assert!(
        output.len() <= 8194,
        "only a few KiB per line may be retained, got {} bytes",
        output.len()
    );
    let first = format!("{}…", "a".repeat(79));
    assert_eq!(screen.wait_for(&first).await, [first, "second".into()]);
    // Resizing can reveal more of the bounded output; steady frames stay small.
    screen.ui.backend_mut().resize(12, 12);
    assert_eq!(
        screen.bar(),
        [format!("{}…", "a".repeat(11)), "second".into()]
    );
}

#[tokio::test]
async fn failures_keep_previous_output_and_first_failure_marks_default_in_red() {
    let mut screen = Screen::new("cat >/dev/null\nexit 1");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let rows = screen.bar();
        if rows[0].ends_with("statusline error") {
            assert!(rows[0].starts_with(" test-model · think high · Always ask"));
            assert_eq!(rows[1], " ~");
            let buffer = screen.ui.backend().buffer();
            let x = (0..80)
                .find(|&x| buffer[(x, 10)].symbol() == "s" && buffer[(x, 10)].fg == Color::Red)
                .unwrap();
            assert_eq!(buffer[(x + 15, 10)].fg, Color::Red);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rows:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fs::write(
        screen.root.path().join("status.sh"),
        "cat >/dev/null\nprintf good",
    )
    .unwrap();
    screen.change_model("next");
    assert_eq!(screen.wait_for("good").await, ["good", ""]);
    for (model, script) in [
        ("failed", "cat >/dev/null; exit 2"),
        ("empty", "cat >/dev/null"),
    ] {
        fs::write(screen.root.path().join("status.sh"), script).unwrap();
        screen.change_model(model);
        for _ in 0..50 {
            assert_eq!(screen.bar(), ["good", ""]);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn timeout_stops_the_process_group_and_keeps_previous_output() {
    let mut screen = Screen::new("cat >/dev/null; printf previous");
    screen.wait_for("previous").await;
    fs::write(screen.root.path().join("status.sh"),
        "cat >/dev/null\necho $$ > parent.pid\n(sleep 2; echo survived > survived) &\necho $! > child.pid\nwait").unwrap();
    screen.change_model("timeout");
    let start = tokio::time::Instant::now();
    while start.elapsed() < Duration::from_millis(1600) {
        assert_eq!(screen.bar(), ["previous", ""]);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for file in ["parent.pid", "child.pid"] {
        let pid = fs::read_to_string(screen.root.path().join(file))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(),
            "{file} still running"
        );
    }
    assert!(!screen.root.path().join("survived").exists());
}

#[tokio::test]
async fn changes_are_coalesced_for_300ms_and_the_latest_state_eventually_runs() {
    let mut screen = Screen::new("cat >> runs.jsonl\nprintf custom");
    screen.wait_for("custom").await;
    for model in ["one", "two", "latest"] {
        screen.change_model(model);
        screen.bar();
        tokio::time::sleep(Duration::from_millis(20)).await;
        screen.bar();
    }
    let runs = || fs::read_to_string(screen.root.path().join("runs.jsonl")).unwrap();
    assert_eq!(
        runs().lines().count(),
        1,
        "changes within 300ms must coalesce"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        screen.bar();
        let input = fs::read_to_string(screen.root.path().join("runs.jsonl")).unwrap();
        if input.lines().count() == 2 {
            let latest: Value = serde_json::from_str(input.lines().last().unwrap()).unwrap();
            assert_eq!(latest["model"], "latest");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn command_replaces_two_rows_keeps_color_and_discards_controls_and_extra_lines() {
    let mut screen = Screen::new(
        "cat > input.json\nprintf '\\033[31mCustom\\033[0m\\033[2J\\007\\t\\r\\nbranch\\nignored\\n'",
    );
    assert_eq!(screen.wait_for("Custom").await, ["Custom", "branch"]);
    assert_eq!(screen.ui.backend().buffer()[(0, 10)].fg, Color::Red);
    assert!(screen.root.path().join("input.json").is_file());
}

#[tokio::test]
async fn config_command_gets_only_public_json_and_terminal_environment() {
    let mut screen = Screen::new(
        "cat > input.json\nprintf '%s' \"$STATUS_TEST:$HOME:$CANTER_HOME\" > env.txt\nprintf configured",
    );
    let home = screen.root.path().to_str().unwrap().to_owned();
    fs::write(
        screen.root.path().join("config.json"),
        json!({
            "statusLine":"sh ./status.sh", "api_key":"test-only-private-config-key",
            "mcp_servers":{"private":{"env":{"SECRET":"test-only-private-mcp-key"}}}
        })
        .to_string(),
    )
    .unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home.clone()),
        ("CANTER_HOME".into(), home.clone()),
        ("STATUS_TEST".into(), "inherited".into()),
    ]);
    let mut snapshot = screen.ui.app().snapshot().clone();
    snapshot.settings.name = Some("task".into());
    snapshot.settings.approval_policy = canter_protocol::PermissionMode::AskForExecution;
    snapshot.status = canter_protocol::SessionStatus::Working;
    snapshot.token_usage = Some(serde_json::from_value::<TokenUsage>(json!({
        "contextTokens":85000,"modelContextWindow":200000,"autoCompactTokens":100000,
        "total":{"inputTokens":1000,"cachedInputTokens":920,"outputTokens":20,"reasoningOutputTokens":5,"totalTokens":1020},
        "last":{"inputTokens":100,"cachedInputTokens":99,"outputTokens":10,"reasoningOutputTokens":3,"totalTokens":110},
        "cost":{"usd":0.31,"unknown":false}
    })).unwrap());
    *screen.ui.app_mut() = App::new(snapshot, Some(home.clone()), 0)
        .with_welcome("0.1.0 · test")
        .with_status_line(super::load(&env), env);
    assert_eq!(screen.wait_for("configured").await, ["configured", ""]);
    let input = fs::read_to_string(screen.root.path().join("input.json")).unwrap();
    assert_eq!(input.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&input).unwrap(),
        json!({
            "version":"0.1.0 · test", "sessionId":"s1", "sessionName":"task",
            "cwd":home,"gitBranch":null,"model":"test-model","reasoningEffort":"high",
            "approvalPolicy":"askForExecution","status":"working",
            "context":{"usedTokens":85000,"windowTokens":200000,"percent":43,
                "autoCompactTokens":100000,"autoCompactPercentLeft":15},
            "cost":{"usd":0.31,"unknown":false},"cache":{"totalPercent":92,"lastPercent":99},
            "tps":null
        })
    );
    assert!(!input.contains("private"));
    assert_eq!(
        fs::read_to_string(screen.root.path().join("env.txt")).unwrap(),
        format!("inherited:{home}:{home}")
    );
}

#[tokio::test]
async fn missing_invalid_or_empty_config_runs_nothing_and_keeps_default_rows() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().to_str().unwrap().to_owned();
    let env = BTreeMap::from([
        ("HOME".into(), home.clone()),
        ("CANTER_HOME".into(), home.clone()),
    ]);
    let mut snapshot = SessionSnapshot::empty("s1");
    snapshot.settings.cwd = home.clone();
    snapshot.settings.model = "test-model".into();
    let baseline = App::new(snapshot.clone(), Some(home.clone()), 0);
    let mut baseline = Ui::new(TestBackend::new(80, 12), baseline).unwrap();
    baseline.draw(0).unwrap();
    let expected = baseline.backend().buffer().clone();
    for config in [
        None,
        Some("{}"),
        Some("{"),
        Some("{\"statusLine\":42}"),
        Some("{\"statusLine\":null}"),
        Some("{\"statusLine\":\"  \"}"),
    ] {
        if let Some(config) = config {
            fs::write(root.path().join("config.json"), config).unwrap();
        }
        let app = App::new(snapshot.clone(), Some(home.clone()), 0)
            .with_status_line(super::load(&env), env.clone());
        let mut ui = Ui::new(TestBackend::new(80, 12), app).unwrap();
        ui.draw(0).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        ui.draw(0).unwrap();
        assert_eq!(ui.backend().buffer(), &expected);
        // The directory contains only the config we wrote, no command outputs.
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            usize::from(config.is_some())
        );
    }
}

#[tokio::test]
async fn a_running_command_is_not_overlapped_and_gets_the_latest_changes_next() {
    let mut screen = Screen::new(
        "cat >> runs.jsonl\nmkdir running || echo overlap > overlap\nsleep 0.5\nrmdir running\nprintf custom",
    );
    screen.bar();
    // Let the command start, then change twice while it holds the marker.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !screen.root.path().join("running").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for model in ["intermediate", "latest"] {
        screen.change_model(model);
        screen.bar();
    }
    while tokio::time::Instant::now() < deadline {
        screen.bar();
        let runs = fs::read_to_string(screen.root.path().join("runs.jsonl")).unwrap();
        if runs.lines().count() == 2 && !screen.root.path().join("running").exists() {
            assert_eq!(
                serde_json::from_str::<Value>(runs.lines().last().unwrap()).unwrap()["model"],
                "latest"
            );
            assert!(!screen.root.path().join("overlap").exists());
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("latest state never ran after the first command");
}

#[tokio::test]
async fn custom_rows_are_cut_to_width_and_keep_indexed_and_rgb_colors() {
    let mut screen =
        Screen::new("printf '\\033[38;5;123mabcdefghi\\033[0m\\n\\033[48;2;12;34;56msecond\\n'");
    screen.ui.backend_mut().resize(6, 12);
    assert_eq!(screen.wait_for("abcde…").await, ["abcde…", "second"]);
    assert_eq!(
        screen.ui.backend().buffer()[(0, 10)].fg,
        Color::Indexed(123)
    );
    assert_eq!(
        screen.ui.backend().buffer()[(0, 11)].bg,
        Color::Rgb(12, 34, 56)
    );
}

#[tokio::test]
async fn branch_changes_refresh_the_script_while_idle() {
    let mut screen = Screen::new("cat > latest.tmp; mv latest.tmp input.json; printf custom");
    screen.wait_for("custom").await;
    fs::create_dir(screen.root.path().join(".git")).unwrap();
    fs::write(
        screen.root.path().join(".git/HEAD"),
        "ref: refs/heads/new-branch\n",
    )
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        screen.bar();
        if read_input(&screen)["gitBranch"] == "new-branch" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "branch change did not refresh the command"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
