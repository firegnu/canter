//! Adapted from the local P2-72 promptcompare supervisor and accounting rules.
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
    time::Duration,
};
use tokio::process::{Child, Command};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn write_new(path: &Path, body: &[u8]) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(body)?;
    Ok(())
}
pub fn append(path: &Path, value: &Value) -> Result<()> {
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?,
        "{value}"
    )?;
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

// Config errors must never include its contents or credential values.
pub fn read_config(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).map_err(|_| "cannot read config.json")?;
    let config: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid config.json")?;
    if !config.is_object()
        || config["api_key"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err("config.json must contain a non-empty api_key".into());
    }
    Ok(config)
}
pub fn prepare(mut config: Value, fake: bool) -> Result<PathBuf> {
    let root = tempfile::Builder::new()
        .prefix("p2-34-plancompare-")
        .tempdir_in("/tmp")?
        .keep();
    private_dir(&root)?;
    for dir in ["state", "home", "runs"] {
        private_dir(&root.join(dir))?;
    }
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
            .ok_or("missing toolchain directory")?
            .as_os_str()
            .as_encoded_bytes(),
    )?;
    config["model"] = json!("deepseek-flash");
    config["models"] = json!(["deepseek-flash"]);
    config["context_window"] = json!(131072);
    config["plan_enabled"] = json!(true);
    // Keep credentials and endpoint from the authorized file, without connecting MCPs.
    config["mcp_servers"] = json!({});
    write_new(
        &root.join("state/config.json"),
        &serde_json::to_vec_pretty(&config)?,
    )?;
    write_new(
        &root.join("mode.json"),
        &serde_json::to_vec(&json!({"fake":fake}))?,
    )?;
    Ok(root)
}
pub fn set_plan(root: &Path, enabled: bool) -> Result<()> {
    let path = root.join("state/config.json");
    let mut config = read_config(&path)?;
    config["plan_enabled"] = json!(enabled);
    fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
pub fn environment(root: &Path) -> Result<BTreeMap<String, String>> {
    let toolchain = fs::read_to_string(root.join("toolchain.txt"))?;
    Ok(BTreeMap::from([
        ("HOME".into(), root.join("home").display().to_string()),
        (
            "CANTER_HOME".into(),
            root.join("state").display().to_string(),
        ),
        (
            "CARGO_HOME".into(),
            root.join("home/.cargo").display().to_string(),
        ),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        (
            "CARGO_TARGET_DIR".into(),
            root.join("target").display().to_string(),
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

pub struct Server {
    pub child: Child,
    pub client: Client,
}
impl Server {
    pub async fn start(root: &Path, binary: &Path) -> Result<Self> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(root.join("server.log"))?;
        let child = Command::new(binary)
            .arg("serve")
            .env_clear()
            .envs(environment(root)?)
            .current_dir(root.join("home"))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()?;
        let mut server = Self {
            child,
            client: Client::new(&root.join("state/server.sock"))?,
        };
        append(
            &root.join("processes.jsonl"),
            &json!({"pid":server.child.id(),"binary":binary}),
        )?;
        let ready = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if server.child.try_wait()?.is_some() {
                    return Err("isolated server exited; inspect server.log".into());
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
    pub async fn close_session(&self, root: &Path, session: &str) -> Result<()> {
        reqwest::Client::builder()
            .no_proxy()
            .unix_socket(root.join("state/server.sock"))
            .build()?
            .delete(format!("http://localhost/v1/sessions/{session}"))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
    pub async fn stop(&mut self) -> Result<()> {
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

pub fn jsonl(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
pub fn session_dir(root: &Path, session: &str) -> PathBuf {
    root.join("state/sessions").join(session)
}

#[derive(Default)]
pub struct Budget {
    pub requests: usize,
    pub usd: f64,
    pub unknown: bool,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}
impl Budget {
    pub fn soft(&self) -> bool {
        self.requests >= 50 || self.usd >= 0.8
    }
    pub fn hard(&self) -> bool {
        self.requests >= 60 || self.usd >= 1.0
    }
    pub fn value(&self) -> Value {
        json!({"visibleRequests":self.requests,"reportedCostUsd":self.usd,"unknownCost":self.unknown,
            "inputTokens":self.input,"cachedInputTokens":self.cached,"outputTokens":self.output,
            "softStop":self.soft(),"hardStop":self.hard(),
            "counting":"max(ordinary usage events, assistant + compaction_request history) + private usage events; unreported internal retries invisible"})
    }
}
pub fn usage(
    dir: &Path,
    after: u64,
    history_offset: usize,
    through: u64,
    history_end: usize,
) -> Result<Budget> {
    let mut total = Budget::default();
    let mut ordinary = 0;
    let mut private = 0;
    for event in jsonl(&dir.join("events.jsonl"))?.iter().filter(|e| {
        let seq = e["seq"].as_u64().unwrap_or(0);
        seq > after && seq <= through
    }) {
        match event["type"].as_str() {
            Some("session.usage") => ordinary += 1,
            Some("session.btwUsage") => private += 1,
            _ => continue,
        }
        let usage = &event["tokenUsage"];
        if let Some(cost) = usage["request"]["costUsd"].as_f64() {
            total.usd += cost;
        } else {
            total.unknown = true;
        }
        total.input += usage["last"]["inputTokens"].as_u64().unwrap_or(0);
        total.cached += usage["last"]["cachedInputTokens"].as_u64().unwrap_or(0);
        total.output += usage["last"]["outputTokens"].as_u64().unwrap_or(0);
    }
    let visible = jsonl(&dir.join("model-history.jsonl"))?
        .iter()
        .take(history_end)
        .skip(history_offset)
        .filter(|e| {
            e["type"] == "compaction_request"
                || (e["type"] == "message" && e["message"]["type"] == "assistant")
        })
        .count();
    total.requests = ordinary.max(visible) + private;
    total.unknown |= visible > ordinary;
    Ok(total)
}
pub fn budget(root: &Path) -> Result<Budget> {
    let mut total = Budget::default();
    let sessions = root.join("state/sessions");
    if sessions.exists() {
        for entry in fs::read_dir(sessions)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let part = usage(&entry.path(), 0, 0, u64::MAX, usize::MAX)?;
            total.requests += part.requests;
            total.usd += part.usd;
            total.unknown |= part.unknown;
            total.input += part.input;
            total.cached += part.cached;
            total.output += part.output;
        }
    }
    Ok(total)
}
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
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

pub fn report(root: &Path, record: &Value) -> Result<Value> {
    let session = record["session"].as_str().ok_or("missing session")?;
    let dir = session_dir(root, session);
    let after = record["afterSeq"].as_u64().ok_or("missing event cursor")?;
    let offset = record["historyOffset"]
        .as_u64()
        .ok_or("missing history cursor")? as usize;
    let through = record["throughSeq"]
        .as_u64()
        .ok_or("missing final event cursor")?;
    let history_end = record["historyEnd"]
        .as_u64()
        .ok_or("missing final history cursor")? as usize;
    let events = jsonl(&dir.join("events.jsonl"))?;
    let history = jsonl(&dir.join("model-history.jsonl"))?;
    let mut calls = 0;
    let mut unchanged = 0;
    let mut previous = None;
    for entry in history
        .iter()
        .take(history_end)
        .skip(offset)
        .filter(|e| e["type"] == "message" && e["message"]["type"] == "assistant")
    {
        for part in entry["message"]["content"]
            .as_array()
            .ok_or("invalid assistant content")?
        {
            if part["type"] != "tool_call" || part["name"] != "update_plan" {
                continue;
            }
            calls += 1;
            let args: Value = serde_json::from_str(part["arguments"].as_str().unwrap_or("null"))
                .unwrap_or(Value::Null);
            if let Some(plan) = args.get("plan") {
                if previous.as_ref() == Some(plan) {
                    unchanged += 1;
                }
                previous = Some(plan.clone());
            }
        }
    }
    let recent: Vec<_> = events
        .iter()
        .filter(|e| {
            let seq = e["seq"].as_u64().unwrap_or(0);
            seq > after && seq <= through
        })
        .collect();
    let completed: Vec<_> = recent
        .iter()
        .filter(|e| e["type"] == "item.completed")
        .collect();
    let commands: Vec<_> = completed
        .iter()
        .filter(|e| e["item"]["type"] == "commandExecution")
        .map(|e| json!({"command":e["item"]["command"],"exitCode":e["item"]["exitCode"]}))
        .collect();
    let turn = recent.iter().rev().find(|e| e["type"] == "turn.completed");
    Ok(
        json!({"case":record["case"],"session":session,"usage":usage(&dir, after, offset, through, history_end)?.value(),
        "planCalls":calls,"consecutiveUnchangedPlans":unchanged,
        "completedPlanUpdates":completed.iter().filter(|e| e["item"]["type"] == "plan" && e["item"]["status"] == "completed").count(),
        "turnStatus":turn.map(|e| &e["turn"]["status"]),"turnError":turn.map(|e| &e["turn"]["error"]),
        "errors":recent.iter().filter(|e| e["type"] == "error").copied().collect::<Vec<_>>(),"commands":commands}),
    )
}
