//! The `canter-eval` command.

use std::{collections::BTreeMap, path::PathBuf};

use canter_eval::{
    Result,
    paths::{Home, MemoryMode, State},
    prepare::PrepareOptions,
};

const USAGE: &str = "\
Usage:
  canter-eval prepare --round <r> --run <n> [--task <task>] [options]
      write the eval config, clear memories, create a worktree for every
      repository of the run, print the eval server, trust and corral commands;
      --task is for the first session, later sessions read it from state.json
  canter-eval trust --round <r> --run <n>
      trust the run worktree through POST /v1/folderTrust
  canter-eval steer --round <r> --run <n> [--session <n>] [--timeout <secs>]
      find the run's new session and send the task's chat; run it in another window
      ([[chat]]: waits for the opening turn, each chat's turn and a report wake;
       gives up after 1800 seconds unless --timeout says otherwise)
  canter-eval finish --round <r> --run <n> [--session <n>]
      record the session's end HEAD, shell commands and memory files
  canter-eval grade --round <r> --run <n>
      run the task's checks in the worktree
  canter-eval collect --round <r> --run <n> [--stats-json <path>] [--session <id>]
      save the run's stats and checks as one result JSON
  canter-eval clean --round <r> --run <n>
      remove the run's worktrees
  canter-eval stop
      stop the eval server recorded in the eval home's server.json
  canter-eval report --round <r> [--against <round>] [--out <path>]
      write the round's result table to eval/results/<r>.md

Options:
  --eval-home <path>     eval state directory (default $CANTER_EVAL_HOME or ~/.canter-eval)
  --tasks <dir>          task files for prepare (default eval/tasks)
  --model <p/m>          override the model in local.json
  --source-config <path> canter config to copy the model from
  --memory <keep|clear>  keep memories between sessions, or empty them before each
                         (default clear; fixed when session 1 is prepared)
  --session <n>          one-based session for a multi-session task (default 1)
  --timeout <secs>       how long [[chat]] steering may wait (default 1800)
  --against <round>      include another round in the report
  --out <path>           report destination
  -h, --help             show this help
";

#[tokio::main]
async fn main() {
    match run().await {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("canter-eval: {error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return Ok(true);
    }
    let command = args[0].clone();
    let options = parse(&args[1..])?;
    if options.contains_key("help") {
        print!("{USAGE}");
        return Ok(true);
    }
    let home = Home::resolve(options.get("eval-home").map(PathBuf::from))?;
    match command.as_str() {
        "prepare" => {
            let task_dir = options
                .get("tasks")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(canter_eval::paths::DEFAULT_TASKS));
            let prepared = canter_eval::prepare::prepare(&PrepareOptions {
                home: home.clone(),
                task_dir,
                task: options.get("task").cloned(),
                round: required(&options, "round")?.into(),
                run: required(&options, "run")?.into(),
                session: session(&options)?,
                memory: options
                    .get("memory")
                    .map(|value| MemoryMode::parse(value))
                    .transpose()?,
                model: options.get("model").cloned(),
                source_config: options.get("source-config").map(PathBuf::from),
            })?;
            print!("{}", prepared.summary(&home));
            Ok(true)
        }
        "trust" => {
            let state = state(&home, &options)?;
            let message = canter_eval::server::trust(&home, &state).await?;
            println!("{message}");
            Ok(true)
        }
        "steer" => {
            let mut steer = canter_eval::steer::SteerOptions::new(
                home,
                required(&options, "round")?.into(),
                required(&options, "run")?.into(),
                session(&options)?,
            );
            if let Some(value) = options.get("timeout") {
                let seconds = value
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .ok_or_else(|| format!("--timeout must be seconds from 1: {value:?}"))?;
                steer.timeout = std::time::Duration::from_secs(seconds);
            }
            canter_eval::steer::steer(steer).await?;
            Ok(true)
        }
        "finish" => {
            let (round, run) = round_and_run(&options)?;
            let index = session(&options)?;
            let finished = canter_eval::finish::finish(&home, &round, &run, index)?;
            let head = finished
                .head
                .as_deref()
                .map(|head| format!("HEAD {head}"))
                .unwrap_or_else(|| "no repository worktree".to_string());
            println!(
                "recorded session {index}: {head}, {} memory files, {} commands",
                finished.memory_files, finished.commands
            );
            Ok(true)
        }
        "grade" => {
            let (round, run) = round_and_run(&options)?;
            let file = canter_eval::grade::grade(&home, &round, &run, &mut std::io::stdout())?;
            let passed = file
                .results
                .iter()
                .filter(|check| check.passed == Some(true))
                .count();
            let total = file.results.iter().filter(|check| !check.record).count();
            println!("{passed}/{total} judged checks passed");
            Ok(passed == total)
        }
        "collect" => {
            let (round, run) = round_and_run(&options)?;
            let result = canter_eval::collect::collect(canter_eval::collect::CollectOptions {
                home: home.clone(),
                round,
                run,
                stats_json: options.get("stats-json").map(PathBuf::from),
                session: options.get("session").cloned(),
            })?;
            println!(
                "result: {} {}/{} checks, session {}",
                result.task, result.checks_passed, result.checks_total, result.session_id
            );
            Ok(true)
        }
        "clean" => {
            let (round, run) = round_and_run(&options)?;
            let state = State::load(&home, &round, &run)?;
            canter_eval::prepare::clean(&home, &state)?;
            println!("removed the worktree of {round}/{run}");
            Ok(true)
        }
        "stop" => {
            println!("{}", canter_eval::server::stop(&home)?);
            Ok(true)
        }
        "report" => {
            let round = required(&options, "round")?.to_string();
            let out = options
                .get("out")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("eval/results").join(format!("{round}.md")));
            let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
                home: home.clone(),
                round,
                against: options.get("against").cloned(),
                out,
            })?;
            println!("wrote {}", path.display());
            Ok(true)
        }
        other => Err(format!("unknown command {other:?}; run canter-eval --help")),
    }
}

fn parse(args: &[String]) -> Result<BTreeMap<String, String>> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(name) = arg.strip_prefix("--") else {
            return Err(format!("unexpected argument {arg:?}"));
        };
        if name == "help" {
            options.insert("help".into(), String::new());
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("--{name} needs a value"))?;
        options.insert(name.to_string(), value.clone());
        index += 2;
    }
    Ok(options)
}

fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("--{name} is required; run canter-eval --help"))
}

fn round_and_run(options: &BTreeMap<String, String>) -> Result<(String, String)> {
    Ok((
        required(options, "round")?.into(),
        required(options, "run")?.into(),
    ))
}

fn session(options: &BTreeMap<String, String>) -> Result<usize> {
    match options.get("session") {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|index| *index >= 1)
            .ok_or_else(|| format!("--session must be a number from 1: {value:?}")),
        None => Ok(1),
    }
}

fn state(home: &Home, options: &BTreeMap<String, String>) -> Result<State> {
    let (round, run) = round_and_run(options)?;
    State::load(home, &round, &run)
}
