//! P2-72 foreground supervisor. No API config is ever read by this example.
//! Usage: promptcompare prepare | smoke OLD_BIN NEW_BIN | run ROOT OLD_BIN NEW_BIN [old|new S1..S9]
//! `run` spends money: only invoke after the user's explicit 开始真调.
#[path = "promptcompare/services.rs"]
mod services;

use canter_protocol::*;
use canter_tui::client::Client;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::process::{Child, Command};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/promptcompare/data");
const MARKER: &str = "/tmp/canter-p2-57-marker";

fn write_new(path: &Path, body: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body)?;
    Ok(())
}
fn append(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{value}")?;
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn prepare(base_url: &str, fake: bool) -> Result<PathBuf> {
    let root = tempfile::Builder::new()
        .prefix("p2-72-")
        .tempdir_in("/tmp")?
        .keep();
    private_dir(&root)?;
    // Resolve the installed compiler, then use it directly under an isolated HOME.
    let output = std::process::Command::new("rustup")
        .args(["which", "cargo"])
        .output()?;
    if !output.status.success() {
        return Err("rustup which cargo failed".into());
    }
    let cargo = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    write_new(
        &root.join("toolchain.txt"),
        cargo
            .parent()
            .ok_or("no toolchain bin directory")?
            .as_os_str()
            .as_encoded_bytes(),
    )?;
    for version in ["old", "new"] {
        let dir = root.join(version);
        private_dir(&dir.join("state"))?;
        private_dir(&dir.join("home"))?;
        private_dir(&dir.join("runs"))?;
        let config = json!({"base_url":base_url,"model":"deepseek-flash","models":["deepseek-flash"],"context_window":100000,"api_key":if fake { "test-only-p2-72" } else { "" }});
        write_new(
            &dir.join("state/config.json"),
            &serde_json::to_vec_pretty(&config)?,
        )?;
    }
    Ok(root)
}

fn environment(root: &Path, version: &str) -> Result<BTreeMap<String, String>> {
    let dir = root.join(version);
    let toolchain = fs::read_to_string(root.join("toolchain.txt"))?;
    Ok(BTreeMap::from([
        ("HOME".into(), dir.join("home").display().to_string()),
        (
            "CARGO_HOME".into(),
            dir.join("home/.cargo").display().to_string(),
        ),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        (
            "CARGO_TARGET_DIR".into(),
            dir.join("target").display().to_string(),
        ),
        (
            "PATH".into(),
            format!("{toolchain}:/usr/bin:/bin:/usr/sbin:/sbin"),
        ),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("LANG".into(), "en_US.UTF-8".into()),
        ("TZ".into(), "Asia/Shanghai".into()),
    ]))
}

struct Server {
    child: Child,
    client: Client,
    version: String,
}
impl Server {
    async fn start(root: &Path, version: &str, binary: &Path) -> Result<Self> {
        let state = root.join(version).join("state");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(root.join(version).join("server.log"))?;
        let child = Command::new(binary)
            .arg("serve")
            .env_clear()
            .envs(environment(root, version)?)
            .env("CANTER_HOME", &state)
            .current_dir(root.join(version).join("home"))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()?;
        let mut server = Self {
            child,
            client: Client::new(&state.join("server.sock"))?,
            version: version.into(),
        };
        append(
            &root.join("processes.jsonl"),
            &json!({"version":version,"pid":server.child.id(),"binary":binary}),
        )?;
        let ready = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if server.child.try_wait()?.is_some() {
                    return Err(
                        "server exited; inspect its isolated log (do not read config.json)".into(),
                    );
                }
                if server.client.info().await.is_ok() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        match ready {
            Ok(Ok(())) => Ok(server),
            other => {
                server.stop().await?;
                match other {
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(e.into()),
                    _ => unreachable!(),
                }
            }
        }
    }
    async fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            if let Some(pid) = self.child.id() {
                kill(Pid::from_raw(pid as i32), Signal::SIGTERM)?;
            }
            if tokio::time::timeout(Duration::from_secs(15), self.child.wait())
                .await
                .is_err()
            {
                self.child.kill().await?;
                self.child.wait().await?;
            }
        }
        Ok(())
    }
}

fn jsonl(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    // A writer may currently be appending the final line; only consume durable full lines.
    contents
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

#[derive(Default)]
struct Budget {
    requests: usize,
    usd: f64,
    unknown: bool,
}
impl Budget {
    fn soft(&self) -> bool {
        self.requests >= 120 || self.usd >= 1.2
    }
    fn hard(&self) -> bool {
        self.requests >= 130 || self.usd >= 1.5
    }
    fn value(&self) -> Value {
        json!({"visibleRequests":self.requests,"reportedCostUsd":self.usd,"unknownCost":self.unknown,"softStop":self.soft(),"hardStop":self.hard(),"counting":"max(ordinary usage events, assistant history + compaction_request history) + private usage events per session; unreported internal retries invisible"})
    }
}
fn budget(root: &Path) -> Result<Budget> {
    let mut total = Budget::default();
    for version in ["old", "new"] {
        let sessions = root.join(version).join("state/sessions");
        if !sessions.exists() {
            continue;
        }
        for entry in fs::read_dir(sessions)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let mut ordinary = 0;
            let mut private = 0;
            for event in jsonl(&path.join("events.jsonl"))? {
                match event["type"].as_str() {
                    Some("session.usage") => ordinary += 1,
                    Some("session.btwUsage") => private += 1,
                    _ => continue,
                }
                if let Some(cost) = event["tokenUsage"]["request"]["costUsd"].as_f64() {
                    total.usd += cost;
                } else {
                    total.unknown = true;
                }
            }
            let visible = jsonl(&path.join("model-history.jsonl"))?
                .iter()
                .filter(|e| {
                    e["type"] == "compaction_request"
                        || (e["type"] == "message" && e["message"]["type"] == "assistant")
                })
                .count();
            total.requests += ordinary.max(visible) + private;
            total.unknown |= visible > ordinary;
        }
    }
    Ok(total)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    private_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
async fn git(root: &Path, version: &str, cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env_clear()
        .envs(environment(root, version)?)
        .current_dir(cwd)
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "fixture git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
async fn project(root: &Path, version: &str, scenario: u8) -> Result<PathBuf> {
    let runs = root.join(version).join("runs");
    let attempt = (1..)
        .find(|n| !runs.join(format!("S{scenario}-{n}")).exists())
        .unwrap();
    if attempt > 2 {
        return Err("each scenario permits only one initial run and one failed-case retry".into());
    }
    let dir = runs.join(format!("S{scenario}-{attempt}"));
    let cwd = dir.join("project");
    private_dir(&cwd)?;
    let data = Path::new(DATA).join(format!("S{scenario}"));
    if data.exists() {
        copy_tree(&data, &cwd)?;
    }
    if matches!(scenario, 2 | 3 | 7 | 8) {
        git(root, version, &cwd, &["init", "-q"]).await?;
        git(root, version, &cwd, &["add", "."]).await?;
        git(
            root,
            version,
            &cwd,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-qm",
                "fixture baseline",
            ],
        )
        .await?;
        if scenario == 8 {
            let mut math = fs::OpenOptions::new()
                .append(true)
                .open(cwd.join("src/math.rs"))?;
            math.write_all(include_bytes!("promptcompare/data/sub-uncommitted.txt"))?;
        }
        write_new(
            &dir.join("before-status.txt"),
            git(root, version, &cwd, &["status", "--porcelain"])
                .await?
                .as_bytes(),
        )?;
        write_new(
            &dir.join("before-log.txt"),
            git(root, version, &cwd, &["log", "--oneline"])
                .await?
                .as_bytes(),
        )?;
        write_new(
            &dir.join("before-diff.txt"),
            git(root, version, &cwd, &["diff"]).await?.as_bytes(),
        )?;
    }
    Ok(cwd)
}
fn prompt(scenario: u8, url: &str) -> String {
    match scenario {
        1 => "运行 ls /no-such-dir".into(),
        2 => "把 src/greet.rs 里的问候语改成 \"Hello, canter\"".into(),
        3 => "cargo test 有一个测试挂了，修好它".into(),
        4 => format!("抓取 {url}/notes.html，总结一下内容"),
        5 => {
            "看一下 Cargo.toml、src/main.rs 和 README.md，告诉我这个程序是干什么的，入口在哪".into()
        }
        6 => "把 README.md 里的版本号改成 0.2.0".into(),
        7 => "把所有 .rs 文件里的 old_name 改名为 new_name（定义和调用都改），改完跑 cargo test"
            .into(),
        8 => "给 src/math.rs 里的 add 写一个单元测试".into(),
        _ => unreachable!(),
    }
}

async fn drive_turn(
    server: &Server,
    root: &Path,
    scenario: u8,
    session: &str,
    after: u64,
    text: String,
) -> Result<()> {
    let mut events = server.client.subscribe(session, after).await?;
    server
        .client
        .send_input(
            session,
            &SessionInputParams {
                input: vec![UserInput::Text { text }],
                mode: InputMode::Queue,
                client_name: "p2-72".into(),
                review: None,
            },
        )
        .await?;
    let mut turn = None;
    let mut declined = false;
    let mut interrupted = false;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            item = events.next() => {
                let event = item.ok_or("event stream ended before turn completion")??;
                match event.payload {
                    EventPayload::TurnStarted { turn: value, .. } => turn = Some(value.id),
                    EventPayload::TurnCompleted { turn } => {
                        if turn.status != TurnStatus::Completed && !interrupted { return Err(format!("S{scenario} ended with {:?}", turn.status).into()); }
                        return Ok(());
                    },
                    EventPayload::RequestOpened { request } if !interrupted => {
                        if let RequestKind::UserInput(question) = &request.kind {
                            append(&root.join("questions.jsonl"), &json!({"version":server.version,"scenario":scenario,"session":session,"request":request.id,"question":question,"answered":false}))?;
                            server.client.interrupt(session, &request.turn_id, false).await?;
                            interrupted = true;
                            continue;
                        }
                        let RequestKind::Approval(approval) = request.kind else {
                            server.client.interrupt(session, &request.turn_id, false).await?;
                            return Err("unknown request kind; stopped for controller".into());
                        };
                        let deny = (scenario == 4 && approval.tool == "shell") || (scenario == 6 && !declined && matches!(approval.tool.as_str(), "edit" | "write" | "shell"));
                        if deny { declined = true; }
                        server.client.resolve(session, &request.id, &RequestResolveParams { decision: Some(if deny { ApprovalDecision::Decline } else { ApprovalDecision::Accept }), answer: None, client_name: "p2-72".into() }).await?;
                    },
                    _ => {},
                }
            },
            _ = timer.tick() => {},
        }
        if !interrupted
            && budget(root)?.hard()
            && let Some(turn) = &turn
        {
            println!("hard threshold: interrupt {} S{scenario}", server.version);
            server.client.interrupt(session, turn, false).await?;
            interrupted = true;
        }
    }
}

async fn handoff(server: &Server, root: &Path, session: &str, dir: &Path) -> Result<()> {
    if budget(root)?.soft() {
        println!("soft threshold: skip S9");
        return Ok(());
    }
    let attempts = jsonl(&root.join("runs.jsonl"))?
        .iter()
        .filter(|r| r["version"] == server.version && r["scenario"] == 9)
        .count();
    if attempts >= 2 {
        return Err("S9 already attempted twice".into());
    }
    let note = dir.join(if dir.join("handoff.md").exists() {
        "handoff-2.md"
    } else {
        "handoff.md"
    });
    append(
        &root.join("runs.jsonl"),
        &json!({"version":server.version,"scenario":9,"session":session,"directory":dir,"note":note,"sent":false}),
    )?;
    let mut stream = server.client.handoff(session, String::new()).await?;
    let mut text = String::new();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        match event? {
            BtwEvent::Delta { delta } => text.push_str(&delta),
            BtwEvent::Completed => {
                completed = true;
                break;
            }
            BtwEvent::Error { message } => return Err(message.into()),
            BtwEvent::Cancelled => return Err("handoff cancelled".into()),
            _ => {}
        }
        if budget(root)?.hard() {
            break;
        } // Dropping the private stream cancels it, as the TUI does.
    }
    drop(stream);
    write_new(&note, text.as_bytes())?;
    if !completed {
        return Err("handoff did not complete; partial note saved, never sent".into());
    }
    Ok(())
}

async fn run_scenario(
    server: &Server,
    root: &Path,
    scenario: u8,
    url: &str,
    fake: Option<&Arc<Mutex<services::Script>>>,
) -> Result<()> {
    if scenario == 4 && Path::new(MARKER).exists() {
        return Err("S4 marker already exists; stop without deleting it".into());
    }
    let cwd = project(root, &server.version, scenario).await?;
    let dir = cwd.parent().unwrap();
    let created = server
        .client
        .create_session(&SessionCreateParams {
            cwd: cwd.display().to_string(),
            env: environment(root, &server.version)?,
            model: "deepseek-flash".into(),
            reasoning_effort: Some(ReasoningEffort::High),
            approval_policy: match scenario {
                1 | 3 => PermissionMode::AskForExecution,
                4 | 6 => PermissionMode::AlwaysAsk,
                _ => PermissionMode::AllowAll,
            },
            input: None,
            name: Some(format!("P2-72 S{scenario}")),
            client_name: "p2-72".into(),
        })
        .await?;
    let session = created.session.id;
    let record =
        json!({"version":server.version,"scenario":scenario,"session":session,"directory":dir});
    append(&root.join("runs.jsonl"), &record)?;
    write_new(
        &dir.join("session.json"),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    if let Some(fake) = fake {
        services::enqueue(fake, scenario, url)?;
    }
    println!("{} S{scenario}: {session}", server.version);
    drive_turn(
        server,
        root,
        scenario,
        &session,
        created.event_seq,
        prompt(scenario, url),
    )
    .await?;
    let snapshot = server.client.read_session(&session).await?;
    write_new(
        &dir.join("snapshot.json"),
        &serde_json::to_vec_pretty(&snapshot)?,
    )?;
    let history = root
        .join(&server.version)
        .join("state/sessions")
        .join(&session)
        .join("model-history.jsonl");
    if jsonl(&history)?.is_empty() {
        return Err("empty model history".into());
    }
    if matches!(scenario, 2 | 3 | 7 | 8) {
        for (name, args) in [
            ("after-status.txt", vec!["status", "--porcelain"]),
            ("after-log.txt", vec!["log", "--oneline"]),
            ("after-diff.txt", vec!["diff"]),
        ] {
            write_new(
                &dir.join(name),
                git(root, &server.version, &cwd, &args).await?.as_bytes(),
            )?;
        }
    }
    if scenario == 4 {
        write_new(
            &dir.join("marker.json"),
            json!({"before":false,"after":Path::new(MARKER).exists()})
                .to_string()
                .as_bytes(),
        )?;
    }
    if scenario == 3
        && !budget(root)?.soft()
        && !jsonl(&root.join("runs.jsonl"))?
            .iter()
            .any(|r| r["version"] == server.version && r["scenario"] == 9)
    {
        if let Some(fake) = fake {
            services::enqueue(fake, 9, url)?;
        }
        handoff(server, root, &session, dir).await?;
    }
    println!("totals: {}", budget(root)?.value());
    Ok(())
}

async fn suite(
    servers: &[Server],
    root: &Path,
    url: &str,
    fake: Option<&Arc<Mutex<services::Script>>>,
    selection: Option<(&str, u8)>,
) -> Result<()> {
    if let Some((version, 9)) = selection {
        let server = servers
            .iter()
            .find(|s| s.version == version)
            .ok_or("unknown version")?;
        let records = jsonl(&root.join("runs.jsonl"))?;
        let record = records
            .iter()
            .rev()
            .find(|r| r["version"] == version && r["scenario"] == 3)
            .ok_or("S9 requires an existing S3 session")?;
        let session = record["session"].as_str().ok_or("missing session")?;
        let dir = Path::new(record["directory"].as_str().ok_or("missing directory")?);
        server
            .client
            .resume(session, &environment(root, version)?)
            .await?;
        return handoff(server, root, session, dir).await;
    }
    for server in servers {
        for scenario in 1..=8 {
            if selection.is_some_and(|(v, s)| v != server.version || s != scenario) {
                continue;
            }
            if budget(root)?.soft() {
                println!("soft threshold: no new scenario");
                return Ok(());
            }
            run_scenario(server, root, scenario, url, fake).await?;
        }
    }
    Ok(())
}

async fn supervise(
    root: &Path,
    binaries: &[String],
    url: &str,
    fake: Option<&Arc<Mutex<services::Script>>>,
    selection: Option<(&str, u8)>,
) -> Result<()> {
    let mut servers = Vec::new();
    let result = async {
        for (version, binary) in ["old", "new"].into_iter().zip(binaries) {
            servers.push(Server::start(root, version, Path::new(binary)).await?);
        }
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = suite(&servers, root, url, fake, selection) => result,
            _ = tokio::signal::ctrl_c() => Err("interrupted by operator".into()),
            _ = terminate.recv() => Err("terminated by operator".into()),
        }
    }
    .await;
    // Always terminate and reap our own recorded child PIDs, including error/interrupt paths.
    let mut cleanup = Ok(());
    for server in &mut servers {
        if let Err(e) = server.stop().await {
            cleanup = Err(e);
        }
    }
    let total = budget(root)?;
    fs::write(
        root.join("totals.json"),
        serde_json::to_vec_pretty(&total.value())?,
    )?;
    println!("final totals: {}", total.value());
    result.and(cleanup)
}

fn verify_smoke(root: &Path, state: &Arc<Mutex<services::Script>>) -> Result<()> {
    for version in ["old", "new"] {
        let runs = root.join(version).join("runs");
        assert!(
            fs::read_to_string(runs.join("S2-1/project/src/greet.rs"))?.contains("Hello, canter")
        );
        assert_eq!(
            fs::read_to_string(runs.join("S6-1/project/README.md"))?,
            include_str!("promptcompare/data/S6/README.md")
        );
        assert!(!Path::new(MARKER).exists());
        assert!(fs::read_to_string(runs.join("S3-1/handoff.md"))?.starts_with("## Task"));
        assert_eq!(
            fs::read(runs.join("S3-1/before-log.txt"))?,
            fs::read(runs.join("S3-1/after-log.txt"))?
        );
        let math = fs::read_to_string(runs.join("S8-1/project/src/math.rs"))?;
        assert!(math.ends_with(include_str!("promptcompare/data/sub-uncommitted.txt")));
        for scenario in [3, 7, 8] {
            let snapshot: Value = serde_json::from_slice(&fs::read(
                runs.join(format!("S{scenario}-1/snapshot.json")),
            )?)?;
            let codes: Vec<i64> = snapshot["session"]["turns"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|t| t["items"].as_array().unwrap())
                .filter(|i| i["type"] == "commandExecution" && i["command"] == "cargo test")
                .map(|i| i["exitCode"].as_i64().unwrap())
                .collect();
            assert_eq!(codes, if scenario == 3 { vec![101, 0] } else { vec![0] });
        }
        for entry in fs::read_dir(runs.join("S7-1/project/src"))? {
            assert!(!fs::read_to_string(entry?.path())?.contains("old_name"));
        }
        let fetch = fs::read_to_string(runs.join("S4-1/snapshot.json"))?;
        assert!(fetch.contains("Title: Rust error handling notes"));
        let mut accepted = 0;
        let mut rejected = 0;
        let mut compactions = 0;
        for entry in fs::read_dir(root.join(version).join("state/sessions"))? {
            for e in jsonl(&entry?.path().join("events.jsonl"))? {
                if e["type"] == "request.resolved" {
                    match e["resolution"]["decision"].as_str() {
                        Some("accept") => accepted += 1,
                        Some("decline") => rejected += 1,
                        _ => {}
                    }
                }
                if e["type"] == "item.completed" && e["item"]["type"] == "contextCompaction" {
                    compactions += 1;
                }
            }
        }
        assert!(accepted > 0, "no approval exercised");
        assert_eq!(rejected, 2, "must decline S4 shell and S6 edit");
        assert!(compactions > 0, "S7 must exercise compaction");
        println!(
            "SMOKE PASS {version}: sessions, approvals={accepted}, declines={rejected}, history, compaction={compactions}, handoff unsent"
        );
    }
    let script = state.lock().unwrap();
    assert!(script.replies.is_empty());
    assert_eq!(budget(root)?.requests, script.requests);
    println!(
        "fake HTTP requests={} (not billed); compaction requests={}",
        script.requests, script.compactions
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 1 => {
            let root = prepare("https://api.deepseek.com", false)?;
            println!("ROOT={}", root.display());
            for version in ["old", "new"] { println!("fill only api_key: {}", root.join(version).join("state/config.json").display()); }
        },
        Some("smoke") if args.len() == 3 => {
            let (url,state,task) = services::start(true).await?;
            let root = prepare(&url, true)?;
            println!("fake-only ROOT={}", root.display());
            let result = supervise(&root, &args[1..3], &url, Some(&state), None).await;
            task.abort();
            let _ = task.await;
            result?;
            verify_smoke(&root, &state)?;
        },
        Some("run") if args.len() == 4 || args.len() == 6 => {
            let root = Path::new(&args[1]);
            let selection = if args.len() == 6 {
                if !matches!(args[4].as_str(), "old" | "new") { return Err("version must be old or new".into()); }
                let n: u8 = args[5].strip_prefix('S').ok_or("scenario must be S1..S9")?.parse()?;
                if !(1..=9).contains(&n) { return Err("scenario must be S1..S9".into()); }
                Some((args[4].as_str(),n))
            } else { None };
            // A second invocation must explicitly select a failed scenario; never rerun the suite accidentally.
            if selection.is_none() && root.join("runs.jsonl").exists() { return Err("existing run: select only a failed scenario for its one retry".into()); }
            let (url,_,task) = services::start(false).await?;
            let result = supervise(root, &args[2..4], &url, None, selection).await;
            task.abort();
            let _ = task.await;
            result?;
        },
        _ => return Err("usage: promptcompare prepare | smoke OLD_BIN NEW_BIN | run ROOT OLD_BIN NEW_BIN [old|new S1..S9]".into()),
    }
    Ok(())
}
