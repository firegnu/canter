//! P2-34 foreground experiment, following P2-72's protocol client and supervisor.
//! prepare CONFIG | smoke BIN | run ROOT BIN | report ROOT
//! `run` spends money: invoke only after the controller says 开始真调.
#[path = "plancompare/fake.rs"]
mod fake;
#[path = "plancompare/support.rs"]
mod support;
use canter_protocol::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::*;
use tokio::process::Command;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plancompare/data");

fn prompt(scenario: &str) -> &'static str {
    match scenario {
        "A" => {
            "给这个小 Rust 库新增 pub fn count_inclusive(start: u32, end: u32) -> u64，计算闭区间内整数的个数；start > end 时返回 0，必须支持 u32::MAX，不要溢出。为正常区间、相同端点、反向区间和最大端点写测试，然后运行 cargo test，失败就修到通过。保留已有函数，不提交 git。"
        }
        "B" => {
            "把这个 Rust 项目的公开函数 old_name 改名为 greet，定义、src/one.rs、src/two.rs 和 tests/integration.rs 里的调用一起改，保持返回值不变；改完运行 cargo test，失败就修到通过。不提交 git。"
        }
        "C" => "src/format.rs 这个文件是干什么的？只解释用途，不修改文件，不运行命令。",
        "D" => "请只回复“收到”，不要修改文件或运行命令。",
        "Stop" => "假模型中断演练。",
        _ => unreachable!(),
    }
}

async fn drive(server: &Server, root: &Path, session: &str, after: u64, text: &str) -> Result<()> {
    let mut events = server.client.subscribe(session, after).await?;
    server
        .client
        .send_input(
            session,
            &SessionInputParams {
                input: vec![UserInput::Text { text: text.into() }],
                mode: InputMode::Queue,
                client_name: "p2-34-plancompare".into(),
                review: None,
            },
        )
        .await?;
    let mut turn = None;
    let mut interrupted = false;
    let mut question = false;
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    loop {
        let event = tokio::select! {
            item = events.next() => Some(item.ok_or("event stream ended before turn completion")??),
            _ = timer.tick() => None,
        };
        if let Some(Event {
            payload: EventPayload::TurnStarted { turn: started, .. },
            ..
        }) = &event
        {
            turn = Some(started.id.clone());
        }
        if !interrupted
            && budget(root)?.hard()
            && let Some(turn) = &turn
        {
            server.client.interrupt(session, turn, false).await?;
            interrupted = true;
            println!("hard threshold: interrupted {session}");
        }
        let Some(event) = event else {
            continue;
        };
        match event.payload {
            EventPayload::TurnCompleted { .. } => {
                return if question {
                    Err("unanswered model question: suite stopped for controller".into())
                } else {
                    Ok(())
                };
            }
            EventPayload::RequestOpened { request } if !interrupted => match request.kind {
                RequestKind::Approval(_) => {
                    server
                        .client
                        .resolve(
                            session,
                            &request.id,
                            &RequestResolveParams {
                                plan: None,
                                decision: Some(ApprovalDecision::Accept),
                                feedback: None,
                                answer: None,
                                client_name: "p2-34-plancompare".into(),
                            },
                        )
                        .await?;
                }
                _ => {
                    server
                        .client
                        .interrupt(session, &request.turn_id, false)
                        .await?;
                    interrupted = true;
                    question = true;
                }
            },
            _ => {}
        }
    }
}

async fn run_case(
    server: &Server,
    root: &Path,
    scenario: &str,
    enabled: bool,
    reopen: Option<&str>,
    fake: Option<&Arc<Mutex<fake::Script>>>,
) -> Result<String> {
    let name = if matches!(scenario, "D" | "Stop") {
        scenario.to_owned()
    } else {
        format!("{scenario}-{}", if enabled { "on" } else { "off" })
    };
    let dir = root.join("runs").join(&name);
    if dir.exists() {
        return Err(format!("case {name} already exists; no automatic retries").into());
    }
    private_dir(&dir)?;
    set_plan(root, enabled)?;
    let (snapshot, after) = if let Some(session) = reopen {
        server.close_session(root, session).await?;
        let resumed = server.client.resume(session, &environment(root)?).await?;
        (resumed.session, resumed.event_seq)
    } else {
        let cwd = dir.join("project");
        if scenario == "Stop" {
            private_dir(&cwd)?;
        } else {
            copy_tree(&Path::new(DATA).join(scenario), &cwd)?;
        }
        let created = server
            .client
            .create_session(&SessionCreateParams {
                additional_directories: Vec::new(),
                cwd: cwd.display().to_string(),
                env: environment(root)?,
                model: "deepseek-flash".into(),
                reasoning_effort: Some(ReasoningEffort::High),
                approval_policy: PermissionMode::AskForExecution,
                input: None,
                name: Some(format!("P2-34 {name}")),
                client_name: "p2-34-plancompare".into(),
            })
            .await?;
        (created.session, created.event_seq)
    };
    if snapshot.settings.plan_enabled != Some(enabled) {
        return Err("session plan switch mismatch".into());
    }
    let session = snapshot.id;
    let offset = jsonl(&session_dir(root, &session).join("model-history.jsonl"))?.len();
    let mut record = json!({"case":name,"session":session,"directory":dir,"cwd":snapshot.settings.cwd,
        "planEnabled":enabled,"afterSeq":after,"historyOffset":offset});
    append(&root.join("runs.jsonl"), &record)?;
    if let Some(fake) = fake {
        fake::enqueue(fake, scenario, enabled)?;
    }
    println!("{name}: {session}");
    let result = drive(server, root, &session, after, prompt(scenario)).await;
    let snapshot = server.client.read_session(&session).await?;
    write_new(
        &dir.join("snapshot.json"),
        &serde_json::to_vec_pretty(&snapshot)?,
    )?;
    record["throughSeq"] = json!(snapshot.event_seq);
    record["historyEnd"] =
        json!(jsonl(&session_dir(root, &session).join("model-history.jsonl"))?.len());
    write_new(
        &dir.join("record.json"),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    let summary = report(root, &record)?;
    append(&root.join("results.jsonl"), &summary)?;
    println!("{name}: {}", summary["usage"]);
    result?;
    if matches!(scenario, "A" | "B") {
        // Independent local verification; no model request and no added project files.
        let cargo = fs::read_to_string(root.join("toolchain.txt"))?;
        let output = Command::new(Path::new(&cargo).join("cargo"))
            .arg("test")
            .env_clear()
            .envs(environment(root)?)
            .current_dir(&snapshot.session.settings.cwd)
            .output()
            .await?;
        write_new(&dir.join("verify.stdout"), &output.stdout)?;
        write_new(&dir.join("verify.stderr"), &output.stderr)?;
        write_new(
            &dir.join("verify.json"),
            json!({"command":"cargo test","exitCode":output.status.code()})
                .to_string()
                .as_bytes(),
        )?;
    }
    Ok(session)
}

async fn suite(
    server: &Server,
    root: &Path,
    fake: Option<&Arc<Mutex<fake::Script>>>,
) -> Result<()> {
    let mut a_on = None;
    for scenario in ["A", "B", "C"] {
        for enabled in [true, false] {
            if budget(root)?.soft() {
                println!("soft threshold: no new scenario");
                return Ok(());
            }
            let session = run_case(server, root, scenario, enabled, None, fake).await?;
            if scenario == "A" && enabled {
                a_on = Some(session);
            }
        }
    }
    if !budget(root)?.soft() {
        run_case(server, root, "D", false, a_on.as_deref(), fake).await?;
    }
    if let Some(fake) = fake {
        // A large synthetic usage event exercises the actual hard-stop interrupt path.
        run_case(server, root, "Stop", false, None, Some(fake)).await?;
    }
    Ok(())
}

async fn supervise(
    root: &Path,
    binary: &Path,
    fake: Option<&Arc<Mutex<fake::Script>>>,
) -> Result<()> {
    if root.join("runs.jsonl").exists() {
        return Err("existing run; refusing to repeat scenarios".into());
    }
    let mut server = Server::start(root, binary).await?;
    let result = async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = suite(&server, root, fake) => result,
            _ = tokio::signal::ctrl_c() => Err("interrupted by operator".into()),
            _ = terminate.recv() => Err("terminated by operator".into()),
        }
    }
    .await;
    let stopped = server.stop().await;
    write_new(
        &root.join("totals.json"),
        &serde_json::to_vec_pretty(&budget(root)?.value())?,
    )?;
    result.and(stopped)
}

fn verify_smoke(root: &Path, fake: &Arc<Mutex<fake::Script>>) -> Result<()> {
    let results = jsonl(&root.join("results.jsonl"))?;
    assert_eq!(results.len(), 8);
    for result in &results[..7] {
        let name = result["case"].as_str().unwrap();
        assert_eq!(result["turnStatus"], "completed", "{result}");
        assert_eq!(result["errors"], json!([]));
        assert_eq!(result["consecutiveUnchangedPlans"], 0);
        let expected = match name {
            "A-on" => 3,
            "B-on" => 2,
            _ => 0,
        };
        assert_eq!(result["planCalls"], expected, "{name}");
        assert_eq!(result["completedPlanUpdates"], expected, "{name}");
        let record: Value =
            serde_json::from_slice(&fs::read(root.join("runs").join(name).join("record.json"))?)?;
        assert_eq!(
            report(root, &record)?,
            *result,
            "D must not be counted again in A-on"
        );
        if name.starts_with('A') || name.starts_with('B') {
            let dir = root.join("runs").join(name);
            let verified: Value = serde_json::from_slice(&fs::read(dir.join("verify.json"))?)?;
            assert_eq!(verified["exitCode"], 0);
            let codes: Vec<_> = result["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["exitCode"].as_i64().unwrap())
                .collect();
            assert_eq!(
                codes,
                if name.starts_with('A') {
                    vec![101, 0]
                } else {
                    vec![0]
                }
            );
            if name.starts_with('B') {
                for path in [
                    "src/lib.rs",
                    "src/one.rs",
                    "src/two.rs",
                    "tests/integration.rs",
                ] {
                    assert!(
                        !fs::read_to_string(dir.join("project").join(path))?.contains("old_name")
                    );
                }
            }
        } else if name.starts_with('C') {
            assert_eq!(
                fs::read(root.join("runs").join(name).join("project/src/format.rs"))?,
                include_bytes!("plancompare/data/C/src/format.rs")
            );
        }
    }
    assert_eq!(results[7]["turnStatus"], "interrupted");
    assert!(budget(root)?.hard());
    assert!(budget(root)?.soft());
    let script = fake.lock().unwrap();
    assert!(script.replies.is_empty());
    assert!(script.checked_d);
    assert_eq!(budget(root)?.requests, script.requests);
    let approvals = jsonl(&root.join("runs.jsonl"))?
        .iter()
        .filter(|r| r["case"] != "D")
        .map(|r| jsonl(&session_dir(root, r["session"].as_str().unwrap()).join("events.jsonl")))
        .collect::<Result<Vec<_>>>()?
        .iter()
        .flatten()
        .filter(|e| e["type"] == "request.resolved" && e["resolution"]["decision"] == "accept")
        .count();
    assert!(approvals > 0);
    println!(
        "SMOKE PASS: A/B/C on/off, D and hard-stop interrupt, {} fake requests, {approvals} approvals, actual Rust tests passed; no real charges",
        script.requests
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 2 => {
            let root = prepare(read_config(Path::new(&args[1]))?, false)?;
            println!(
                "ROOT={}\nConfiguration copied; no model requests sent.",
                root.display()
            );
        }
        Some("smoke") if args.len() == 2 => {
            let (url, script, task) = fake::start().await?;
            let root = prepare(json!({"base_url":url,"api_key":"test-only-p2-34"}), true)?;
            println!("FAKE_ROOT={}", root.display());
            let result = supervise(&root, Path::new(&args[1]), Some(&script)).await;
            task.abort();
            let _ = task.await;
            result?;
            // Assertions run only after the child server and fake service have stopped.
            verify_smoke(&root, &script)?;
        }
        Some("run") if args.len() == 3 => {
            let root = PathBuf::from(&args[1]);
            let mode: Value = serde_json::from_slice(&fs::read(root.join("mode.json"))?)?;
            if mode["fake"] != false {
                return Err("run requires a prepared real configuration".into());
            }
            supervise(&root, Path::new(&args[2]), None).await?;
        }
        Some("report") if args.len() == 2 => {
            let root = Path::new(&args[1]);
            for started in jsonl(&root.join("runs.jsonl"))? {
                let record: Value = serde_json::from_slice(&fs::read(
                    Path::new(
                        started["directory"]
                            .as_str()
                            .ok_or("missing run directory")?,
                    )
                    .join("record.json"),
                )?)?;
                println!("{}", report(root, &record)?);
            }
            println!("totals: {}", budget(root)?.value());
        }
        _ => {
            return Err(
                "usage: plancompare prepare CONFIG | smoke BIN | run ROOT BIN | report ROOT".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Budget;
    #[test]
    fn stop_thresholds_use_either_requests_or_cost() {
        for (requests, usd, soft, hard) in [
            (49, 0.79, false, false),
            (50, 0.0, true, false),
            (0, 0.8, true, false),
            (59, 0.99, true, false),
            (60, 0.0, true, true),
            (0, 1.0, true, true),
        ] {
            let budget = Budget {
                requests,
                usd,
                ..Budget::default()
            };
            assert_eq!(budget.soft(), soft);
            assert_eq!(budget.hard(), hard);
        }
    }
}
