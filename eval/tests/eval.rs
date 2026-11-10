//! Local tests: temporary git repositories, a fake unix-socket server, and a
//! fake stats report. No real model, no `~/.canter`, no real server.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use canter_eval::{
    collect::{CollectOptions, ContextShare, Metrics, RereadFiles, RunResult, SessionRow},
    grade::{CheckFile, CheckResult},
    paths::{Home, MemoryMode, State, StateRepo},
    prepare::PrepareOptions,
    steer::SteerOptions,
    task::Task,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

fn long_task_path() -> String {
    format!("{}/tasks/long.toml", env!("CARGO_MANIFEST_DIR"))
}

fn task_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

/// The long task file pointing at a fixture repository's base commit.
fn task_dir_with_base(temp: &Path, base: &str) -> PathBuf {
    let text = std::fs::read_to_string(task_dir().join("long.toml")).unwrap();
    let text = text.replace(
        "base = \"8e19ca8db4b181c15a3a2a8cc75e1fccfee98538\"",
        &format!("base = \"{base}\""),
    );
    let dir = temp.join("tasks");
    write(&dir.join("long.toml"), &text);
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "eval@test.invalid"]);
    git(dir, &["config", "user.name", "eval test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn commit(dir: &Path, subject: &str, body: Option<&str>) {
    git(dir, &["add", "-A"]);
    match body {
        Some(body) => {
            git(dir, &["commit", "-q", "-m", subject, "-m", body]);
        }
        None => {
            git(dir, &["commit", "-q", "-m", subject]);
        }
    }
}

struct FixtureOptions {
    /// Commit 3 and later carry `Task: N/12` as the last line.
    trailers: bool,
    /// Commit 12 adds the interactive confirmation text.
    phrase: bool,
    /// Commits that also change README.md.
    readme_commits: Vec<usize>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            trailers: true,
            phrase: true,
            readme_commits: vec![6, 7, 8, 9],
        }
    }
}

/// A canter-test-ts-shaped repository: 12 group items, 12 ritual commits and
/// a local test runner that needs no network (node's test runner and a fake tsc).
fn lifecycle_repo(dir: &Path, options: &FixtureOptions) -> String {
    init_repo(dir);
    write(
        &dir.join("package.json"),
        &serde_json::to_string_pretty(&json!({
            "name": "canter-test-ts",
            "version": "0.1.0",
            "private": true,
            "type": "commonjs",
            "scripts": {"test": "node --test --test-reporter=spec tests/*.test.js"},
            "devDependencies": {"typescript": "^7.0.2"}
        }))
        .unwrap(),
    );
    write(&dir.join(".gitignore"), "node_modules/\ndist/\n");
    let todo: String = (1..=12)
        .map(|n| format!("- [ ] **Task {n}.** Do item {n}.\n"))
        .collect();
    write(&dir.join("TODO.md"), &todo);
    write(&dir.join("README.md"), "# Habits\n");
    write(&dir.join("src/habit.ts"), "export const version = 1;\n");
    write(
        &dir.join("tests/habit.test.js"),
        "const test = require(\"node:test\");\n\
         const assert = require(\"node:assert\");\n\
         test(\"habit\", () => { assert.ok(true); });\n",
    );
    write(&dir.join("node_modules/.bin/tsc"), "#!/bin/sh\nexit 0\n");
    let tsc = dir.join("node_modules/.bin/tsc");
    let mut permissions = std::fs::metadata(&tsc).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&tsc, permissions).unwrap();
    commit(dir, "初始骨架", None);
    let base = git(dir, &["rev-parse", "HEAD"]).trim().to_string();

    for n in 1..=12 {
        let todo: String = (1..=12)
            .map(|item| {
                format!(
                    "- [{}] **Task {item}.** Do item {item}.\n",
                    if item <= n { "x" } else { " " }
                )
            })
            .collect();
        write(&dir.join("TODO.md"), &todo);
        if options.readme_commits.contains(&n) {
            let mut readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
            readme.push_str(&format!("- task {n} usage example\n"));
            write(&dir.join("README.md"), &readme);
        }
        if n == 12 && options.phrase {
            let mut source = std::fs::read_to_string(dir.join("src/habit.ts")).unwrap();
            source.push_str(
                "export function ask(name: string): string {\n  return `Remove \"${name}\"? [y/N] `;\n}\n",
            );
            write(&dir.join("src/habit.ts"), &source);
        }
        if options.trailers && n >= 3 {
            commit(
                dir,
                &format!("ritual: task {n}"),
                Some(&format!("Task: {n}/12")),
            );
        } else {
            commit(dir, &format!("ritual: task {n}"), None);
        }
    }
    base
}

fn state(home: &Home, dir: &Path, base: &str, round: &str, run: &str) -> State {
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: round.into(),
        run: run.into(),
        repo: dir.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base.into(),
        worktree: dir.display().to_string(),
        session_file: home.session_id_path(round, run, 1).display().to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(home).unwrap();
    state
}

fn grade(home: &Home, dir: &Path, base: &str, run: &str) -> CheckFile {
    state(home, dir, base, "r", run);
    canter_eval::grade::grade(home, "r", run, &mut Vec::new()).unwrap()
}

fn find<'a>(file: &'a CheckFile, id: &str) -> &'a CheckResult {
    file.results.iter().find(|check| check.id == id).unwrap()
}

fn source_config(dir: &Path) -> PathBuf {
    let path = dir.join("source-config.json");
    write(
        &path,
        &serde_json::to_string_pretty(&json!({
            "model": "opencode-go/deepseek-v4.1-flash",
            "providers": {
                "opencode-go": {
                    "base_url": "https://example.invalid/v1",
                    "api_key": "test-placeholder-key",
                    "headers": {"x-opencode-session": "{session_id}"},
                    "models": {
                        "deepseek-v4.1-flash": {
                            "context_window": 1000000,
                            "auto_compact_tokens": 400000,
                            "image": true,
                            "price": {"peak": {"cache_hit": 0.006, "cache_miss": 0.3, "output": 1.2}}
                        },
                        "another-model": {"context_window": 900000}
                    }
                },
                "kimi": {
                    "base_url": "https://other.invalid/v1",
                    "api_key": "other-placeholder-key",
                    "models": {"k3.1-pro": {"context_window": 900000}}
                }
            },
            "prices": {
                "opencode-go/deepseek-v4.1-flash": {
                    "peak": {"cache_hit": 0.006, "cache_miss": 0.3, "output": 1.2}
                },
                "kimi/k3.1-pro": {
                    "peak": {"cache_hit": 1, "cache_miss": 2, "output": 3}
                }
            }
        }))
        .unwrap(),
    );
    path
}

fn local_config(home: &Home, repo: &Path, source: &Path) -> PathBuf {
    let worktrees = home.root.parent().unwrap().join("worktrees");
    write_local_config(home, repo, source, &worktrees);
    worktrees
}

fn write_local_config(home: &Home, repo: &Path, source: &Path, worktrees: &Path) {
    canter_eval::paths::write_private(
        &home.local_path(),
        serde_json::to_vec_pretty(&json!({
            "repos": {"canter-test-ts": repo.display().to_string()},
            "model": "opencode-go/deepseek-v4.1-flash",
            "sourceConfig": source.display().to_string(),
            "canter": "canter",
            "worktrees": worktrees.display().to_string()
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
}

#[test]
fn prepare_writes_one_model_and_a_worktree_at_the_base_commit() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("canter-test-ts");
    let base = lifecycle_repo(&repo, &FixtureOptions::default());
    let source = source_config(temp.path());
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    let worktrees = local_config(&home, &repo, &source);
    write(&home.root.join("memory/global.md"), "# memory\n");
    write(
        &home.root.join("projects/git-abc/memory/project.md"),
        "# memory\n",
    );

    let prepared = canter_eval::prepare::prepare(&PrepareOptions {
        home: home.clone(),
        task_dir: task_dir_with_base(temp.path(), &base),
        task: Some("long".into()),
        round: "r1".into(),
        run: "long-1".into(),
        session: 1,
        memory: None,
        model: None,
        source_config: Some(source.clone()),
    })
    .unwrap();

    let config: Value =
        serde_json::from_slice(&std::fs::read(home.config_path()).unwrap()).unwrap();
    assert_eq!(config["model"], "opencode-go/deepseek-v4.1-flash");
    let providers = config["providers"].as_object().unwrap();
    assert_eq!(providers.len(), 1);
    let models = providers["opencode-go"]["models"].as_object().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models["deepseek-v4.1-flash"]["auto_compact_tokens"], 60_000);
    assert_eq!(
        models["deepseek-v4.1-flash"]["default_reasoning_effort"],
        "high"
    );
    assert_eq!(providers["opencode-go"]["api_key"], "test-placeholder-key");
    assert_eq!(
        config["prices"]["opencode-go/deepseek-v4.1-flash"]["peak"]["cache_miss"],
        0.3
    );
    assert!(config["prices"]["other/another-model"].is_null());

    assert_eq!(
        git(prepared.state.worktree(), &["rev-parse", "HEAD"]).trim(),
        base
    );
    assert!(
        !Path::new(&prepared.state.worktree()).starts_with(&home.root),
        "the worktree must stay outside the eval state directory"
    );
    assert!(Path::new(&prepared.state.worktree()).starts_with(&worktrees));
    let node_modules = Path::new(&prepared.state.worktree()).join("node_modules");
    assert!(node_modules.is_dir());
    assert!(!node_modules.is_symlink());
    assert!(node_modules.join(".bin/tsc").is_file());
    assert_eq!(
        git(prepared.state.worktree(), &["status", "--short"]).trim(),
        ""
    );
    assert_eq!(
        std::fs::read_dir(home.root.join("memory")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(home.root.join("projects/git-abc/memory"))
            .unwrap()
            .count(),
        0
    );

    let summary = prepared.summary(&home);
    assert!(summary.contains("corral start eval/long-1"));
    assert!(summary.contains("CANTER_HOME="));
    assert!(summary.contains("顺序完成 TODO.md 里的全部 12 件任务"));
    assert!(!summary.contains("test-placeholder-key"));
    assert!(!summary.contains("other-placeholder-key"));

    canter_eval::prepare::clean(&home, &prepared.state).unwrap();
    assert!(!Path::new(&prepared.state.worktree()).exists());

    let outside = canter_eval::paths::State {
        worktree: temp.path().join("elsewhere").display().to_string(),
        ..prepared.state.clone()
    };
    assert!(canter_eval::prepare::clean(&home, &outside).is_err());
}

#[test]
fn eval_home_refuses_home_and_parents_of_canter_state() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let daily = home.join(".canter");
    assert!(
        Home::resolve_with(Some(home.clone()), Some(&home), Some(&daily)).is_err(),
        "$HOME itself must be refused"
    );
    assert!(
        Home::resolve_with(Some(daily.clone()), Some(&home), Some(&daily)).is_err(),
        "canter's state directory itself must be refused"
    );
    assert!(
        Home::resolve_with(Some(daily.join("nested")), Some(&home), Some(&daily)).is_err(),
        "a directory inside canter's state directory must be refused"
    );
    let custom = temp.path().join("data/canter");
    assert!(
        Home::resolve_with(Some(temp.path().join("data")), Some(&home), Some(&custom)).is_err(),
        "the parent of a custom canter state directory must be refused"
    );
    assert!(
        Home::resolve_with(Some(home.join(".canter-eval")), Some(&home), Some(&daily)).is_ok(),
        "a sibling of the daily state directory is fine"
    );
}

#[test]
fn prepare_refuses_a_worktree_root_inside_the_eval_state_directory() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("canter-test-ts");
    let base = lifecycle_repo(&repo, &FixtureOptions::default());
    let source = source_config(temp.path());
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    write_local_config(&home, &repo, &source, &home.root.join("worktrees"));
    let error = canter_eval::prepare::prepare(&PrepareOptions {
        home: home.clone(),
        task_dir: task_dir_with_base(temp.path(), &base),
        task: Some("long".into()),
        round: "r1".into(),
        run: "long-1".into(),
        session: 1,
        memory: None,
        model: None,
        source_config: Some(source),
    })
    .unwrap_err();
    assert!(
        error.contains("inside the eval state directory"),
        "unexpected error: {error}"
    );
}

#[test]
fn every_long_check_has_a_passing_and_a_failing_example() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };

    // The passing fixture.
    let good = temp.path().join("good");
    let base = lifecycle_repo(&good, &FixtureOptions::default());
    let file = grade(&home, &good, &base, "good");
    for id in [
        "todo-checked",
        "twelve-ritual-commits",
        "task-trailer",
        "tests-and-types",
        "confirmation-prompt",
        "dependencies-unchanged",
    ] {
        assert_eq!(find(&file, id).passed, Some(true), "{id} should pass");
    }
    assert_eq!(find(&file, "readme-in-late-commits").value, Some(4));
    assert_eq!(find(&file, "final-test-count").value, Some(1));

    // TODO.md with an unfinished item.
    let todo = temp.path().join("todo-fail");
    let base = lifecycle_repo(&todo, &FixtureOptions::default());
    let text = std::fs::read_to_string(todo.join("TODO.md")).unwrap();
    write(&todo.join("TODO.md"), &text.replacen("- [x]", "- [ ]", 1));
    assert_eq!(
        find(&grade(&home, &todo, &base, "todo-fail"), "todo-checked").passed,
        Some(false)
    );

    // An extra commit that is not a ritual commit.
    let extra = temp.path().join("extra-commit");
    let base = lifecycle_repo(&extra, &FixtureOptions::default());
    write(&extra.join("notes.txt"), "extra\n");
    commit(&extra, "not a ritual commit", None);
    assert_eq!(
        find(
            &grade(&home, &extra, &base, "extra-commit"),
            "twelve-ritual-commits"
        )
        .passed,
        Some(false)
    );

    // A commit without the Task trailer.
    let trailer = temp.path().join("trailer-fail");
    let base = lifecycle_repo(
        &trailer,
        &FixtureOptions {
            trailers: false,
            ..FixtureOptions::default()
        },
    );
    assert_eq!(
        find(
            &grade(&home, &trailer, &base, "trailer-fail"),
            "task-trailer"
        )
        .passed,
        Some(false)
    );

    // A failing test.
    let tests = temp.path().join("tests-fail");
    let base = lifecycle_repo(&tests, &FixtureOptions::default());
    write(
        &tests.join("tests/extra.test.js"),
        "const test = require(\"node:test\");\n\
         test(\"extra\", () => { throw new Error(\"no\"); });\n",
    );
    assert_eq!(
        find(
            &grade(&home, &tests, &base, "tests-fail"),
            "tests-and-types"
        )
        .passed,
        Some(false)
    );

    // The confirmation text is missing.
    let phrase = temp.path().join("phrase-fail");
    let base = lifecycle_repo(
        &phrase,
        &FixtureOptions {
            phrase: false,
            ..FixtureOptions::default()
        },
    );
    assert_eq!(
        find(
            &grade(&home, &phrase, &base, "phrase-fail"),
            "confirmation-prompt"
        )
        .passed,
        Some(false)
    );

    // An added dependency.
    let deps = temp.path().join("deps-fail");
    let base = lifecycle_repo(&deps, &FixtureOptions::default());
    let mut package: Value =
        serde_json::from_slice(&std::fs::read(deps.join("package.json")).unwrap()).unwrap();
    package["dependencies"] = json!({"left-pad": "1.0.0"});
    write(
        &deps.join("package.json"),
        &serde_json::to_string_pretty(&package).unwrap(),
    );
    assert_eq!(
        find(
            &grade(&home, &deps, &base, "deps-fail"),
            "dependencies-unchanged"
        )
        .passed,
        Some(false)
    );

    // No README changes in the late commits: the recorded number changes.
    let readme = temp.path().join("readme-zero");
    let base = lifecycle_repo(
        &readme,
        &FixtureOptions {
            readme_commits: Vec::new(),
            ..FixtureOptions::default()
        },
    );
    assert_eq!(
        find(
            &grade(&home, &readme, &base, "readme-zero"),
            "readme-in-late-commits"
        )
        .value,
        Some(0)
    );

    // One more passing test: the recorded count changes.
    let count = temp.path().join("test-count");
    let base = lifecycle_repo(&count, &FixtureOptions::default());
    write(
        &count.join("tests/second.test.js"),
        "const test = require(\"node:test\");\n\
         test(\"second\", () => {});\n",
    );
    assert_eq!(
        find(
            &grade(&home, &count, &base, "test-count"),
            "final-test-count"
        )
        .value,
        Some(2)
    );
}

#[tokio::test]
async fn steer_waits_for_the_counted_commits_and_sends_steer_mode() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "long-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "long-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    write(
        Path::new(&state.session_file),
        &json!({"sessionId": "s-test", "cwd": repo, "stateDir": home.root}).to_string(),
    );

    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": "t-1"})).await;
    write(
        &home.server_path(),
        &serde_json::to_string(&json!({
            "pid": 1,
            "version": "test",
            "protocolVersion": 1,
            "socketPath": fake.socket.clone(),
            "instance": "i-test"
        }))
        .unwrap(),
    );

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "long-1".into(),
                session: 1,
                poll: Duration::from_millis(50),
                timeout: Duration::from_secs(30),
            })
            .await
        }
    });

    commit_ritual(&repo, 1);
    assert!(fake.next(Duration::from_millis(300)).await.is_none());

    commit_ritual(&repo, 2);
    let (method, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(method, "POST");
    assert_eq!(path, "/v1/sessions/s-test/input");
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["mode"], "steer");
    assert_eq!(body["clientName"], "canter-eval");
    assert_eq!(body["input"][0]["type"], "text");
    assert_eq!(
        body["input"][0]["text"],
        "补一条要求：从现在开始，每个提交说明的最后一行加上 Task: N/12，N 是这件在 TODO 里的序号。"
    );

    commit_ritual(&repo, 3);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        body["input"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("先记住一个细节")
    );

    commit_ritual(&repo, 4);
    assert!(fake.next(Duration::from_millis(300)).await.is_none());

    commit_ritual(&repo, 5);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        body["input"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("再补一条")
    );
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // A restart does not send the same steers again.
    canter_eval::steer::steer(SteerOptions {
        home,
        round: "r1".into(),
        run: "long-1".into(),
        session: 1,
        poll: Duration::from_millis(50),
        timeout: Duration::from_secs(30),
    })
    .await
    .unwrap();
    assert!(fake.next(Duration::from_millis(300)).await.is_none());
}

/// A repo with one base commit and a saved run state whose `session.json`
/// does not exist yet, as after `corral start` (corral owns `--session-file`).
fn steer_state_without_session_file(temp: &Path, home: &Home) -> (PathBuf, State) {
    let repo = temp.join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "long-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "long-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(home).unwrap();
    (repo, state)
}

fn register_fake(home: &Home, fake: &Fake) {
    write(
        &home.server_path(),
        &serde_json::to_string(&json!({
            "pid": 1, "version": "test", "protocolVersion": 1,
            "socketPath": fake.socket.clone(), "instance": "i"
        }))
        .unwrap(),
    );
}

fn listed(id: &str, cwd: &Path, parent: Option<&str>) -> Value {
    let mut session = json!({
        "id": id, "cwd": cwd, "name": null, "status": "idle",
        "updatedAt": 1, "lastEventSeq": 1
    });
    if let Some(parent) = parent {
        session["parent"] = json!({
            "agentType": "explorer", "sessionId": parent, "turnId": "t", "task": "look"
        });
    }
    session
}

#[tokio::test]
async fn steer_finds_the_run_session_by_worktree_and_writes_the_session_file() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let (repo, state) = steer_state_without_session_file(temp.path(), &home);
    let other = temp.path().join("other");
    let list = json!({"data": [
        listed("s-other", &other, None),
        listed("s-child", &repo, Some("s-run")),
        listed("s-run", &repo, None),
    ]});
    let mut fake = fake_server_with(
        temp.path(),
        vec![("/v1/sessions", list)],
        json!({"eventSeq": 1, "turnId": "t-1"}),
    )
    .await;
    register_fake(&home, &fake);

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "long-1".into(),
                session: 1,
                poll: Duration::from_millis(50),
                timeout: Duration::from_secs(30),
            })
            .await
        }
    });

    let (method, path, _) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!((method.as_str(), path.as_str()), ("GET", "/v1/sessions"));
    for n in 1..=2 {
        commit_ritual(&repo, n);
    }
    let (method, path, _) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/v1/sessions/s-run/input")
    );
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&state.session_file).unwrap()).unwrap();
    assert_eq!(saved["sessionId"], "s-run");
    handle.abort();
}

#[tokio::test]
async fn steer_refuses_to_guess_between_two_run_sessions() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let (repo, state) = steer_state_without_session_file(temp.path(), &home);
    let list = json!({"data": [listed("s-a", &repo, None), listed("s-b", &repo, None)]});
    let fake = fake_server_with(temp.path(), vec![("/v1/sessions", list)], json!({})).await;
    register_fake(&home, &fake);

    let error = tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home,
            round: "r1".into(),
            run: "long-1".into(),
            session: 1,
            poll: Duration::from_millis(50),
            timeout: Duration::from_secs(30),
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("s-a") && error.contains("s-b"), "{error}");
    assert!(!Path::new(&state.session_file).exists());
}

#[tokio::test]
async fn trust_posts_the_worktree_through_the_public_interface() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "long-1".into(),
        repo: worktree.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: "abc".into(),
        worktree: worktree.display().to_string(),
        session_file: home
            .session_id_path("r1", "long-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    let mut fake = fake_server(
        temp.path(),
        json!({"path": worktree, "trusted": true, "warning": null}),
    )
    .await;
    write(
        &home.server_path(),
        &serde_json::to_string(&json!({
            "pid": 1, "version": "test", "protocolVersion": 1,
            "socketPath": fake.socket.clone(), "instance": "i"
        }))
        .unwrap(),
    );
    let message = canter_eval::server::trust(&home, &state).await.unwrap();
    assert!(message.contains("trusted"));
    let (method, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(method, "POST");
    assert_eq!(path, "/v1/folderTrust");
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["cwd"], worktree.display().to_string());
}

fn commit_ritual(dir: &Path, n: usize) {
    write(&dir.join(format!("task-{n}.txt")), &format!("task {n}\n"));
    commit(dir, &format!("ritual: task {n}"), None);
}

#[tokio::test]
async fn collect_extracts_metrics_and_counts_events() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "long-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: "abc".into(),
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "long-1", 1)
            .display()
            .to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    let checks = CheckFile {
        ran_at: 0,
        results: vec![
            CheckResult {
                id: "todo-checked".into(),
                description: "TODO.md 12 件全勾".into(),
                record: false,
                passed: Some(true),
                value: None,
                output_tail: String::new(),
            },
            CheckResult {
                id: "tests-and-types".into(),
                description: "tests".into(),
                record: false,
                passed: Some(false),
                value: None,
                output_tail: "boom".into(),
            },
            CheckResult {
                id: "readme-in-late-commits".into(),
                description: "record".into(),
                record: true,
                passed: None,
                value: Some(4),
                output_tail: "4".into(),
            },
        ],
    };
    canter_eval::paths::write_private(
        &home.run_dir("r1", "long-1").join("checks.json"),
        serde_json::to_vec_pretty(&checks).unwrap().as_slice(),
    )
    .unwrap();
    let stats_path = temp.path().join("stats.json");
    write(
        &stats_path,
        &serde_json::to_string(&stats_fixture()).unwrap(),
    );
    let events = [
        json!({"seq": 1, "sessionId": "s-collect", "timestamp": 1, "type": "warning",
               "message": "The model reply was truncated; continuing automatically."}),
        json!({"seq": 2, "sessionId": "s-collect", "timestamp": 2, "type": "item.completed", "turnId": "t",
               "item": {"type": "commandExecution", "id": "i", "command": "rm -rf /", "cwd": "/tmp",
                        "status": "declined", "aggregatedOutput": "Blocked", "exitCode": null, "durationMs": null}}),
        json!({"seq": 3, "sessionId": "s-collect", "timestamp": 3, "type": "item.completed", "turnId": "t",
               "item": {"type": "commandExecution", "id": "j", "command": "ls", "cwd": "/tmp",
                        "status": "completed", "aggregatedOutput": "ok\n\n[Repeated call] try another", "exitCode": 0, "durationMs": 1}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    write(&home.session_events("s-collect"), &events);
    write(
        &home.server_path(),
        &serde_json::to_string(&json!({
            "pid": 1, "version": "0.1.0 · abc", "protocolVersion": 1,
            "socketPath": temp.path().join("unused.sock"), "instance": "i"
        }))
        .unwrap(),
    );

    let result = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "long-1".into(),
        stats_json: Some(stats_path),
        session: None,
    })
    .unwrap();

    assert_eq!(result.session_id, "s-collect");
    assert_eq!(result.canter_version.as_deref(), Some("0.1.0 · abc"));
    assert_eq!(result.checks_passed, 1);
    assert_eq!(result.checks_total, 2);
    let metrics = &result.metrics;
    assert_eq!(metrics.duration_ms, Some(734_000));
    assert_eq!(metrics.requests, 30);
    assert_eq!(metrics.conversation_requests, 1);
    assert_eq!(metrics.compactions.len(), 1);
    assert_eq!(metrics.compactions[0].before_tokens, Some(60_000));
    assert_eq!(metrics.compactions[0].after_tokens, Some(12_000));
    assert_eq!(metrics.cached_input_tokens, Some(1_000));
    assert_eq!(metrics.uncached_input_tokens, Some(2_000));
    assert_eq!(metrics.output_tokens, Some(3_000));
    assert_eq!(metrics.reasoning_output_tokens, 10);
    assert_eq!(metrics.cost_usd, Some(0.5));
    assert!(!metrics.cost_unknown);
    assert_eq!(metrics.compaction_cost_usd, Some(0.05));
    assert_eq!(metrics.read_calls, 20);
    assert_eq!(metrics.update_plan_calls, 12);
    assert_eq!(metrics.reread_files_after_compaction, 2);
    assert_eq!(metrics.tool_failures, 4);
    assert_eq!(metrics.tool_failures_by_name["shell"], 3);
    assert_eq!(metrics.guard_reminders, 2);
    assert_eq!(metrics.blocked_commands, 1);
    assert_eq!(metrics.event_parse_errors, 0);

    let saved = RunResult::load(&home, "r1", "long-1").unwrap();
    assert_eq!(saved.session_id, "s-collect");

    // An explicit --session wins for a one-session run, as it did before;
    // without one, a recorded id that differs from the stats is an error.
    write(
        &home.session_id_path("r1", "long-1", 1),
        &json!({"sessionId": "s-other"}).to_string(),
    );
    let error = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "long-1".into(),
        stats_json: Some(temp.path().join("stats.json")),
        session: None,
    })
    .unwrap_err();
    assert!(error.contains("recorded as s-other"), "{error}");
    // An explicit id still has to match the stats it is paired with.
    let error = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "long-1".into(),
        stats_json: Some(temp.path().join("stats.json")),
        session: Some("s-wanted".into()),
    })
    .unwrap_err();
    assert!(error.contains("asked for as s-wanted"), "{error}");
    let result = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "long-1".into(),
        stats_json: Some(temp.path().join("stats.json")),
        session: Some("s-collect".into()),
    })
    .unwrap();
    assert_eq!(result.session_id, "s-collect");
}

fn stats_fixture() -> Value {
    json!({
        "sessionId": "s-collect",
        "eventSeq": 42,
        "overview": {
            "cwd": "/tmp/x",
            "model": "opencode-go/deepseek-v4.1-flash",
            "startedAt": 1_000_000,
            "updatedAt": 1_734_000,
            "tags": {"eval": "r1"},
            "turnCount": 12,
            "requestCount": 30,
            "usage": {
                "data": [{
                    "cachedInputTokens": 100,
                    "uncachedInputTokens": 200,
                    "outputTokens": 300,
                    "cost": {"usd": 0.5, "unknown": false}
                }],
                "total": {
                    "cachedInputTokens": 1000,
                    "uncachedInputTokens": 2000,
                    "outputTokens": 3000,
                    "cost": {"usd": 0.5, "unknown": false}
                }
            },
            "retryCount": 0,
            "recordedRetryCount": 0
        },
        "requests": [
            {
                "startedAt": 1,
                "purpose": "conversation",
                "outcome": "completed",
                "toolCallIds": [],
                "usage": {"inputTokens": 100, "cachedInputTokens": 10, "outputTokens": 20,
                          "reasoningOutputTokens": 7, "totalTokens": 120}
            },
            {
                "startedAt": 2,
                "purpose": "compaction",
                "outcome": "completed",
                "toolCallIds": [],
                "usage": {"inputTokens": 100, "cachedInputTokens": 10, "outputTokens": 20,
                          "reasoningOutputTokens": 3, "totalTokens": 120},
                "cost": {"usd": 0.05, "unknown": false}
            },
            {
                "startedAt": 3,
                "purpose": "suggestion",
                "outcome": "completed",
                "toolCallIds": []
            }
        ],
        "compactions": [{
            "turnId": "t",
            "itemId": "i",
            "eventSeq": 5,
            "timestamp": 10,
            "trigger": "auto",
            "beforeTokens": 60000,
            "afterTokens": 12000,
            "succeeded": true,
            "signals": {
                "rereadFiles": ["a.ts", "b.ts"],
                "plan": {"before": [], "after": [], "removed": [], "statusChanged": []}
            }
        }],
        "tools": [
            {"name": "read", "callCount": 20, "failureCount": 1, "truncatedCount": 0,
             "averageResultTokens": 1.0, "maxResultTokens": 10, "unknownStatusCount": 0,
             "unknownTruncatedCount": 0, "unknownResultSizeCount": 0},
            {"name": "update_plan", "callCount": 12, "failureCount": 0, "truncatedCount": 0,
             "averageResultTokens": 1.0, "maxResultTokens": 10, "unknownStatusCount": 0,
             "unknownTruncatedCount": 0, "unknownResultSizeCount": 0},
            {"name": "shell", "callCount": 40, "failureCount": 3, "truncatedCount": 0,
             "averageResultTokens": 1.0, "maxResultTokens": 10, "unknownStatusCount": 0,
             "unknownTruncatedCount": 0, "unknownResultSizeCount": 0}
        ],
        "toolCalls": [],
        "approvals": {"count": 0, "data": []}
    })
}

fn sample_result(round: &str, run: &str, passed: usize, duration: u64, cost: f64) -> RunResult {
    RunResult {
        task: "long".into(),
        round: round.into(),
        run: run.into(),
        session_id: format!("s-{round}-{run}"),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        canter_version: Some("0.1.0 · abc".into()),
        protocol_version: Some(1),
        completed_at: 1_760_000_000_000,
        checks: vec![
            CheckResult {
                id: "todo-checked".into(),
                description: "checked".into(),
                record: false,
                passed: Some(true),
                value: None,
                output_tail: String::new(),
            },
            CheckResult {
                id: "tests-and-types".into(),
                description: "tests".into(),
                record: false,
                passed: Some(passed > 1),
                value: None,
                output_tail: String::new(),
            },
            CheckResult {
                id: "readme-in-late-commits".into(),
                description: "record".into(),
                record: true,
                passed: None,
                value: Some(4),
                output_tail: "4".into(),
            },
        ],
        checks_passed: passed,
        checks_total: 2,
        metrics: Metrics {
            duration_ms: Some(duration),
            requests: 30,
            conversation_requests: 12,
            context_peak_tokens: Some(49_710),
            context_average_tokens: Some(34_148),
            context_share: Some(ContextShare {
                system_percent: 17,
                tool_definition_percent: 13,
                conversation_percent: 45,
                tool_result_percent: 26,
            }),
            compactions: vec![canter_eval::collect::CompactionRow {
                trigger: Some("auto".into()),
                before_tokens: Some(60_000),
                after_tokens: Some(12_000),
                succeeded: Some(true),
                reread_files: 2,
                reread: RereadFiles {
                    changed: vec!["a.ts".into()],
                    restored: Vec::new(),
                    unchanged: vec!["b.ts".into()],
                },
            }],
            cached_input_tokens: Some(1_000_000),
            uncached_input_tokens: Some(20_000),
            output_tokens: Some(30_000),
            reasoning_output_tokens: 9_000,
            cost_usd: Some(cost),
            cost_unknown: false,
            compaction_cost_usd: Some(0.02),
            read_calls: 20,
            update_plan_calls: 12,
            reread_files_after_compaction: 2,
            reread: RereadFiles {
                changed: vec!["a.ts".into()],
                restored: Vec::new(),
                unchanged: vec!["b.ts".into()],
            },
            tool_failures: 3,
            tool_failures_by_name: [("shell".to_string(), 2), ("edit".to_string(), 1)].into(),
            guard_reminders: 1,
            blocked_commands: 1,
            event_parse_errors: 0,
        },
        session_count: 1,
        memory_mode: None,
        sessions: vec![SessionRow {
            index: 1,
            session_id: format!("s-{round}-{run}"),
            duration_ms: Some(duration),
            cost_usd: Some(cost),
            cost_unknown: false,
            memory_writes: 0,
            memory_reads: 0,
            memory_files: 0,
            memory_file_names: Vec::new(),
        }],
        sub_agents: Vec::new(),
    }
}

#[test]
fn report_puts_two_rounds_side_by_side() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    for result in [
        sample_result("old", "long-1", 1, 754_000, 0.31),
        sample_result("new", "long-1", 2, 3_723_000, 0.52),
    ] {
        canter_eval::paths::write_private(
            &home.run_dir(&result.round, &result.run).join("result.json"),
            serde_json::to_vec_pretty(&result).unwrap().as_slice(),
        )
        .unwrap();
    }
    let out = temp.path().join("results/new.md");
    let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
        home,
        round: "new".into(),
        against: Some("old".into()),
        out,
    })
    .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("# eval 结果：new"));
    assert!(text.contains("上一轮：old"));
    assert!(text.contains("| 轮次 | 题 | 次 |"));
    assert!(text.contains("| old | long | long-1 | 1/2 |"));
    assert!(text.contains("| new | long | long-1 | 2/2 |"));
    assert!(text.contains("12m34s"));
    assert!(text.contains("1h02m"));
    assert!(text.contains("$0.52 (压缩 $0.02)"));
    assert!(text.contains("| 压后重读 | 工具失败 | 改计划 |"));
    assert!(text.contains("| 压缩 | 上下文峰值 | 平均上下文 | 上下文组成（估算） | 缓存输入 |"));
    assert!(text.contains("50k | 34k | 系统 17% · 工具定义 13% · 对话 45% · 工具结果 26% |"));
    assert!(text.contains("2（改过 1 · 读回 0 · 没改 1）"));
    assert!(text.contains("3（shell 2 · edit 1）"));
    assert!(text.contains("readme-in-late-commits=4"));
    assert!(text.contains("## 通过次数"));
    assert!(text.contains("| old | long | 0/1 | 1.0/2 |"));
    assert!(text.contains("| new | long | 1/1 | 2.0/2 |"));
    assert!(text.contains("## 压缩"));
    assert!(text.contains("60k → 12k（成功） · 压后重读 2（改过 1 · 读回 0 · 没改 1）"));
}

#[test]
fn stop_signals_only_the_pid_in_server_json() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let _lock = hold_server_lock(&home);
    let first = spawn_detached_sleep();
    let second = spawn_detached_sleep();
    canter_eval::paths::write_private(
        &home.server_path(),
        serde_json::to_vec(&json!({
            "pid": first, "version": "test", "protocolVersion": 1,
            "socketPath": temp.path().join("server.sock"), "instance": "i"
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    let message = canter_eval::server::stop(&home).unwrap();
    assert!(message.contains(&first.to_string()));
    assert!(!is_running(first), "the recorded pid should be gone");
    assert!(is_running(second), "the other pid must not be touched");
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(second as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[test]
fn stop_does_not_signal_a_stale_registration() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    std::fs::create_dir_all(&home.root).unwrap();
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.server_lock_path())
        .unwrap();
    let pid = spawn_detached_sleep();
    canter_eval::paths::write_private(
        &home.server_path(),
        serde_json::to_vec(&json!({
            "pid": pid, "version": "test", "protocolVersion": 1,
            "socketPath": temp.path().join("server.sock"), "instance": "i"
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    let error = canter_eval::server::stop(&home).unwrap_err();
    assert!(error.contains("stale"), "unexpected error: {error}");
    assert!(is_running(pid), "the process must not be signalled");
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

fn hold_server_lock(home: &Home) -> std::fs::File {
    std::fs::create_dir_all(&home.root).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.server_lock_path())
        .unwrap();
    file.lock().unwrap();
    file
}

fn spawn_detached_sleep() -> u32 {
    let output = Command::new("sh")
        .arg("-c")
        .arg("sleep 60 >/dev/null 2>&1 & echo $!")
        .output()
        .unwrap();
    assert!(output.status.success());
    let pid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap();
    for _ in 0..100 {
        if is_running(pid) {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the sleep helper did not start");
}

fn is_running(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

struct Fake {
    socket: PathBuf,
    requests: mpsc::UnboundedReceiver<(String, String, Vec<u8>)>,
    task: JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    async fn next(&mut self, wait: Duration) -> Option<(String, String, Vec<u8>)> {
        tokio::time::timeout(wait, self.requests.recv())
            .await
            .ok()
            .flatten()
    }
}

async fn fake_server(dir: &Path, response: Value) -> Fake {
    fake_server_with(dir, Vec::new(), response).await
}

/// Replies with the body routed by exact path, or `response` for any other path.
async fn fake_server_with(dir: &Path, routes: Vec<(&str, Value)>, response: Value) -> Fake {
    let socket = dir.join("fake.sock");
    let routes: Vec<(String, String)> = routes
        .into_iter()
        .map(|(path, body)| (path.to_string(), body.to_string()))
        .collect();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let default_body = response.to_string();
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let end = loop {
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break None;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break Some(position + 4);
                }
            };
            let Some(end) = end else { continue };
            let header = String::from_utf8_lossy(&buffer[..end]).to_string();
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while buffer.len() < end + length {
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let body = buffer[end..].to_vec();
            let mut parts = header.lines().next().unwrap_or("").split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let reply_body = routes
                .iter()
                .find(|(route, _)| *route == path)
                .map_or(default_body.as_str(), |(_, body)| body.as_str());
            let _ = sender.send((method, path, body));
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply_body.len(),
                reply_body
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    Fake {
        socket,
        requests,
        task,
    }
}

fn completed_event(seq: u64, item: Value) -> String {
    json!({
        "seq": seq, "sessionId": "s", "timestamp": seq, "type": "item.completed",
        "turnId": "t", "item": item
    })
    .to_string()
}

fn command_event(seq: u64, command: &str) -> String {
    completed_event(
        seq,
        json!({
            "type": "commandExecution", "id": format!("cmd-{seq}"), "command": command,
            "cwd": "/tmp", "status": "completed", "aggregatedOutput": "",
            "exitCode": 0, "durationMs": 1
        }),
    )
}

fn file_read_event(seq: u64, path: &Path) -> String {
    completed_event(
        seq,
        json!({
            "type": "fileRead", "id": format!("read-{seq}"),
            "path": path.display().to_string(), "startLine": 1, "content": "",
            "status": "completed"
        }),
    )
}

fn file_change_event(seq: u64, path: &Path) -> String {
    completed_event(
        seq,
        json!({
            "type": "fileChange", "id": format!("change-{seq}"),
            "changes": [{
                "path": path.display().to_string(),
                "kind": {"type": "update", "movePath": null},
                "diff": ""
            }],
            "status": "completed"
        }),
    )
}

fn entries(path: &Path) -> usize {
    std::fs::read_dir(path).unwrap().count()
}

fn two_session_task(temp: &Path, base: &str) -> PathBuf {
    let dir = temp.join("tasks-two");
    write(
        &dir.join("two.toml"),
        &format!(
            "id = \"two\"\nname = \"two sessions\"\nrepo = \"canter-test-ts\"\nbase = \"{base}\"\n\n\
             [[sessions]]\nprompt = \"first prompt\"\n\n\
             [[sessions]]\nprompt = \"second prompt\"\n"
        ),
    );
    dir
}

fn write_local_config_repos(home: &Home, repos: &[(&str, &Path)], source: &Path, worktrees: &Path) {
    write_local_config_canter(home, repos, source, worktrees, "canter");
}

fn write_local_config_canter(
    home: &Home,
    repos: &[(&str, &Path)],
    source: &Path,
    worktrees: &Path,
    canter: &str,
) {
    let repos: serde_json::Map<String, Value> = repos
        .iter()
        .map(|(name, path)| ((*name).to_string(), json!(path.display().to_string())))
        .collect();
    canter_eval::paths::write_private(
        &home.local_path(),
        serde_json::to_vec_pretty(&json!({
            "repos": repos,
            "model": "opencode-go/deepseek-v4.1-flash",
            "sourceConfig": source.display().to_string(),
            "canter": canter,
            "worktrees": worktrees.display().to_string()
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
}

/// Two repositories, three sessions: the first in the main one, then two in
/// `canter-test-ts`; each repository's checks prove where they ran.
fn multi_repo_task(temp: &Path, rust_base: &str, ts_base: &str) -> PathBuf {
    let dir = temp.join("tasks-multi");
    write(
        &dir.join("multi.toml"),
        &format!(
            r#"id = "multi"
name = "two repositories"
repo = "canter-test-rust"
base = "{rust_base}"

[[repos]]
name = "canter-test-ts"
base = "{ts_base}"
setup = ['''echo prepared > prepared.txt''']

[[sessions]]
prompt = "rust prompt"

[[sessions]]
repo = "canter-test-ts"
prompt = "ts prompt"

[[sessions]]
repo = "canter-test-ts"
prompt = "ts prompt two"

[[checks]]
id = "ts-check"
description = "runs in the second repository"
repo = "canter-test-ts"
command = '''test -f ts-marker.txt && test "$(git log -1 --format=%s "$BASE")" = ts-base && test "$(cd "$WORKTREE" && pwd -P)" = "$(pwd -P)"'''

[[checks]]
id = "rust-check"
description = "runs in the main repository"
command = '''test -f rust-marker.txt && test "$(git log -1 --format=%s "$BASE")" = rust-base && test "$(cd "$WORKTREE" && pwd -P)" = "$(pwd -P)"'''
"#
        ),
    );
    dir
}

/// A canter-test-ts-shaped repository with the three TODO items the global
/// memory task's sessions 2-4 do, and a local test runner that needs no network.
fn ts_repo(dir: &Path) -> String {
    init_repo(dir);
    write(
        &dir.join("package.json"),
        &serde_json::to_string_pretty(&json!({
            "name": "canter-test-ts",
            "version": "0.1.0",
            "private": true,
            "type": "commonjs",
            "scripts": {"test": "node --test tests/*.test.js"},
            "devDependencies": {"typescript": "^7.0.2"}
        }))
        .unwrap(),
    );
    write(&dir.join(".gitignore"), "node_modules/\ndist/\n");
    let todo: String = [
        "Fix future-date handling.",
        "Add habit archiving.",
        "Implement CSV export.",
    ]
    .iter()
    .map(|title| format!("- [ ] **{title}** Do it.\n"))
    .collect();
    write(&dir.join("TODO.md"), &todo);
    write(&dir.join("README.md"), "# Ritual\n");
    write(
        &dir.join("AGENTS.md"),
        "# 仓库说明\n\n只用标准库，不加依赖。\n",
    );
    write(&dir.join(".agents/skills/readme/SKILL.md"), "# skill\n");
    write(
        &dir.join("tests/habit.test.js"),
        "const test = require(\"node:test\");\n\
         const assert = require(\"node:assert\");\n\
         test(\"habit\", () => { assert.ok(true); });\n",
    );
    write(&dir.join("node_modules/.bin/tsc"), "#!/bin/sh\nexit 0\n");
    let tsc = dir.join("node_modules/.bin/tsc");
    let mut permissions = std::fs::metadata(&tsc).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&tsc, permissions).unwrap();
    commit(dir, "初始骨架", None);
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

/// One commit per session of the global memory task's second half; each follows
/// the remembered habit: a trailer and the TODO item checked.
fn ts_session_commits(dir: &Path) -> Vec<String> {
    let mut heads = Vec::new();
    for (index, title) in [
        "Fix future-date handling.",
        "Add habit archiving.",
        "Implement CSV export.",
    ]
    .iter()
    .enumerate()
    {
        check_todo(dir, title);
        let mut source = std::fs::read_to_string(dir.join("tests/habit.test.js")).unwrap();
        source.push_str(&format!("// session {}\n", index + 2));
        write(&dir.join("tests/habit.test.js"), &source);
        commit(
            dir,
            &format!("session {} work", index + 2),
            Some("Assisted-by: canter"),
        );
        heads.push(git(dir, &["rev-parse", "HEAD"]).trim().to_string());
    }
    heads
}

fn prepare_session(
    home: &Home,
    source: &Path,
    tasks: &Path,
    run: &str,
    session: usize,
    memory: Option<MemoryMode>,
) -> canter_eval::Result<canter_eval::prepare::Prepared> {
    let task = (session == 1).then_some("two");
    prepare_named(home, source, tasks, run, session, memory, task)
}

fn prepare_named(
    home: &Home,
    source: &Path,
    tasks: &Path,
    run: &str,
    session: usize,
    memory: Option<MemoryMode>,
    task: Option<&str>,
) -> canter_eval::Result<canter_eval::prepare::Prepared> {
    canter_eval::prepare::prepare(&PrepareOptions {
        home: home.clone(),
        task_dir: tasks.to_path_buf(),
        task: task.map(str::to_string),
        round: "r1".into(),
        run: run.into(),
        session,
        memory,
        model: None,
        source_config: (session == 1).then(|| source.to_path_buf()),
    })
}

#[test]
fn memory_task_has_four_sessions_and_the_rules_in_the_first() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory.toml");
    let task = Task::load(&path).unwrap();
    assert_eq!(task.id, "memory");
    assert_eq!(task.repo.as_deref(), Some("canter-test-rust"));
    assert_eq!(
        task.base.as_deref(),
        Some("e9ee31cd4e4a0bb671374313cb9699354affa1b1")
    );
    assert_eq!(task.auto_compact_tokens, Some(60_000));
    assert_eq!(task.reasoning.as_deref(), Some("high"));
    assert_eq!(task.session_count(), 4);

    let titles = [
        "Add an edit command",
        "Support weekly reports",
        "Add CSV export",
        "Add monthly project comparisons",
    ];
    for (index, title) in titles.iter().enumerate() {
        let prompt = task.prompt(index + 1).unwrap();
        assert!(
            prompt.starts_with(&format!("做 TODO.md 里的「{title}」这一件")),
            "session {index} prompt: {prompt}"
        );
        assert!(prompt.contains("cargo test 和 cargo clippy -- -D warnings 都通过后"));
        assert!(prompt.contains("通过后，在 TODO.md 勾掉它"));
        assert!(prompt.contains("不要加任何依赖"));
    }
    let first = task.prompt(1).unwrap();
    for line in [
        "另外这个仓库有几条长期的规矩，以后在这里做的每件活都要守：",
        "1. 提交说明的标题写成 tc(<子命令名>): <说明>，比如 tc(edit): add edit command。",
        "2. 提交时按文件名 git add，不要用 git add -A 或 git add .。",
        "3. 每加一个子命令或新选项，在 docs/commands.md 里加一节，写用法和一个例子；没有这个文件就建一个。",
    ] {
        assert!(first.contains(line), "the first prompt is missing {line:?}");
    }
    assert!(!task.prompt(2).unwrap().contains("长期的规矩"));
    assert!(task.prompt(5).is_err());

    let checks: Vec<&str> = task.checks.iter().map(|check| check.id.as_str()).collect();
    assert_eq!(
        checks,
        [
            "four-todos-checked",
            "tests-and-lints",
            "dependencies-unchanged",
            "tc-commit-subjects",
            "commands-doc-per-session",
            "no-blanket-git-add",
        ]
    );
}

#[test]
fn memory_2_task_has_four_sessions_and_the_rules_in_the_first() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory-2.toml");
    let task = Task::load(&path).unwrap();
    assert_eq!(task.id, "memory-2");
    assert_eq!(
        task.name,
        "记忆第二版：canter-test-rust 连做 4 件（规矩从仓库里看不出来）"
    );
    assert_eq!(task.repo.as_deref(), Some("canter-test-rust"));
    assert_eq!(
        task.base.as_deref(),
        Some("e9ee31cd4e4a0bb671374313cb9699354affa1b1")
    );
    assert_eq!(task.auto_compact_tokens, Some(60_000));
    assert_eq!(task.reasoning.as_deref(), Some("high"));
    assert_eq!(task.session_count(), 4);

    let titles = [
        "Add an edit command",
        "Support weekly reports",
        "Add CSV export",
        "Add monthly project comparisons",
    ];
    for (index, title) in titles.iter().enumerate() {
        let prompt = task.prompt(index + 1).unwrap();
        assert!(
            prompt.starts_with(&format!("做 TODO.md 里的「{title}」这一件")),
            "session {index} prompt: {prompt}"
        );
        assert!(prompt.contains("cargo test 和 cargo clippy -- -D warnings 都通过后"));
        assert!(prompt.contains("通过后，在 TODO.md 勾掉它"));
        assert!(prompt.contains("不要加任何依赖"));
    }
    let first = task.prompt(1).unwrap();
    for line in [
        "另外这个仓库有两条长期的规矩，以后在这里做的每件活都要守：",
        "1. README.md 由人维护，不要改它。",
        "2. 每次提交前跑一次 cargo doc --no-deps，要通过。",
    ] {
        assert!(first.contains(line), "the first prompt is missing {line:?}");
    }
    for n in 2..=4 {
        assert!(
            !task.prompt(n).unwrap().contains("长期的规矩"),
            "session {n} prompt must not repeat the rules"
        );
    }
    assert!(task.prompt(5).is_err());

    let checks: Vec<&str> = task.checks.iter().map(|check| check.id.as_str()).collect();
    assert_eq!(
        checks,
        [
            "four-todos-checked",
            "tests-and-lints",
            "dependencies-unchanged",
            "readme-untouched",
            "cargo-doc-per-session",
            "rules-not-in-repo",
        ]
    );
}

#[test]
fn memory_global_task_has_four_sessions_in_two_repositories() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory-global.toml");
    let task = Task::load(&path).unwrap();
    assert_eq!(task.id, "memory-global");
    assert!(task.name.contains("全局记忆"), "{}", task.name);
    assert_eq!(task.repo.as_deref(), Some("canter-test-rust"));
    assert_eq!(
        task.base.as_deref(),
        Some("e9ee31cd4e4a0bb671374313cb9699354affa1b1")
    );
    assert_eq!(task.auto_compact_tokens, Some(60_000));
    assert_eq!(task.reasoning.as_deref(), Some("high"));
    assert_eq!(task.session_count(), 4);

    let repos = task.repositories();
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].name, "canter-test-rust");
    assert_eq!(repos[1].name, "canter-test-ts");
    assert_eq!(repos[1].base, "8e19ca8db4b181c15a3a2a8cc75e1fccfee98538");
    assert_eq!(repos[1].setup.len(), 2);

    let session_repos: Vec<Option<&str>> = (1..=4).map(|n| task.session_repo(n).unwrap()).collect();
    assert_eq!(
        session_repos,
        [
            Some("canter-test-rust"),
            Some("canter-test-ts"),
            Some("canter-test-ts"),
            Some("canter-test-ts")
        ]
    );

    let first = task.prompt(1).unwrap();
    assert!(
        first.starts_with("做 TODO.md 里的「Add an edit command」这一件"),
        "session 1 prompt: {first}"
    );
    for line in [
        "另外说两个我个人的习惯，以后我所有的项目都这样，不只是这个仓库：",
        "1. 每个提交说明的最后一行写 Assisted-by: canter。",
        "2. 每次提交前先跑一次 git diff --check，有问题先改掉。",
    ] {
        assert!(first.contains(line), "the first prompt is missing {line:?}");
    }
    for (index, title) in [
        "Fix future-date handling",
        "Add habit archiving",
        "Implement CSV export",
    ]
    .iter()
    .enumerate()
    {
        let prompt = task.prompt(index + 2).unwrap();
        assert!(
            prompt.starts_with(&format!("做 TODO.md 里的「{title}」这一件")),
            "session {} prompt: {prompt}",
            index + 2
        );
        assert!(prompt.contains("npm test 和 npx tsc --noEmit 都通过后"));
        assert!(prompt.contains("通过后，在 TODO.md 勾掉它"));
        assert!(prompt.contains("不要加任何依赖"));
        assert!(!prompt.contains("Assisted-by"));
        assert!(!prompt.contains("git diff --check"));
        assert!(!prompt.contains("习惯"));
    }
    assert!(task.prompt(5).is_err());

    let checks: Vec<&str> = task.checks.iter().map(|check| check.id.as_str()).collect();
    assert_eq!(
        checks,
        [
            "three-todos-checked",
            "tests-and-types",
            "dependencies-unchanged",
            "assisted-by-trailer",
            "diff-check-per-session",
            "preference-in-global-memory",
            "rules-not-in-repo",
        ]
    );
    for check in &task.checks {
        let expected = if check.id == "preference-in-global-memory" {
            "canter-test-rust"
        } else {
            "canter-test-ts"
        };
        assert_eq!(task.check_repo(check), Some(expected), "check {}", check.id);
    }
}

#[test]
fn an_old_state_file_without_repositories_still_loads() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    // Exactly the shape written before tasks could use several repositories.
    let path = home.run_dir("r1", "long-1").join("state.json");
    write(
        &path,
        &serde_json::to_string_pretty(&json!({
            "task": "long",
            "taskPath": long_task_path(),
            "round": "r1",
            "run": "long-1",
            "repo": "/repos/canter-test-ts",
            "repoName": "canter-test-ts",
            "baseCommit": "abc",
            "worktree": "/worktrees/r1/long-1",
            "sessionFile": "/home/runs/r1/long-1/sessions/1/session.json",
            "model": "m",
            "autoCompactTokens": 60_000,
            "createdAt": 0,
            "sessionCount": 1,
            "memoryMode": "clear"
        }))
        .unwrap(),
    );
    let state = State::load(&home, "r1", "long-1").unwrap();
    assert_eq!(state.worktree(), Path::new("/worktrees/r1/long-1"));
    assert_eq!(state.all_repos().len(), 1);
    let main = state.repo_by_name("canter-test-ts").unwrap();
    assert_eq!(main.repo, "/repos/canter-test-ts");
    assert_eq!(main.base_commit, "abc");
    assert_eq!(main.worktree, "/worktrees/r1/long-1");
    assert!(state.repo_by_name("canter-test-rust").is_err());
}

#[test]
fn prepare_sessions_in_one_worktree_and_clears_memories_by_mode() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    let repo = temp.path().join("canter-test-ts");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let source = source_config(temp.path());
    let worktrees = local_config(&home, &repo, &source);
    let tasks = two_session_task(temp.path(), &base);
    write(&home.root.join("memory/a.md"), "# a\n");
    write(&home.root.join("projects/git-abc/memory/b.md"), "# b\n");
    write(&home.root.join("projects/git-abc/other.md"), "keep\n");
    write(&home.root.join("config.json"), "{}");

    let first =
        prepare_session(&home, &source, &tasks, "two-1", 1, Some(MemoryMode::Clear)).unwrap();
    assert_eq!(first.state.session_count, 2);
    assert_eq!(first.state.memory_mode, Some(MemoryMode::Clear));
    assert_eq!(first.memories_cleared, 2);
    assert_eq!(first.session, 1);
    assert_eq!(entries(&home.root.join("memory")), 0);
    assert!(home.root.join("projects/git-abc/other.md").exists());
    assert!(home.root.join("config.json").exists());
    assert!(home.session_dir("r1", "two-1", 1).is_dir());
    assert!(Path::new(&first.state.worktree).starts_with(&worktrees));
    let summary = first.summary(&home);
    assert!(summary.contains("memory: clear"));
    assert!(summary.contains("session: 1 of 2"));
    assert!(summary.contains("first prompt"));
    assert!(summary.contains("corral start eval/two-1 --cwd"));
    assert!(!summary.contains("--unique"));
    assert!(summary.contains("--session 2"));
    assert!(summary.contains("--session 1"));
    assert!(!summary.contains("second prompt"));

    // Clearing mode empties both layers again before the next session.
    write(&home.root.join("memory/a.md"), "# a\n");
    write(
        &home.session_head_path("r1", "two-1", 1),
        &format!("{base}\n"),
    );
    let second = prepare_session(&home, &source, &tasks, "two-1", 2, None).unwrap();
    assert_eq!(second.session, 2);
    assert_eq!(second.memories_cleared, 1);
    assert_eq!(entries(&home.root.join("memory")), 0);
    let summary = second.summary(&home);
    assert!(summary.contains("session: 2 of 2"));
    assert!(summary.contains("second prompt"));
    assert!(!summary.contains("first prompt"));
    assert!(summary.contains("canter-eval grade"));
    assert!(prepare_session(&home, &source, &tasks, "two-1", 2, None).is_err());

    // Keep mode does not clear between sessions; only the first prepare clears.
    write(&home.root.join("memory/a.md"), "# a\n");
    let keep = prepare_session(&home, &source, &tasks, "two-2", 1, Some(MemoryMode::Keep)).unwrap();
    assert_eq!(keep.memories_cleared, 1);
    write(&home.root.join("memory/a.md"), "# a\n");
    write(
        &home.session_head_path("r1", "two-2", 1),
        &format!("{base}\n"),
    );
    let kept = prepare_session(&home, &source, &tasks, "two-2", 2, None).unwrap();
    assert_eq!(kept.memories_cleared, 0);
    assert!(home.root.join("memory/a.md").exists());
    assert!(kept.summary(&home).contains("memory: keep"));

    // A next session needs the previous one recorded, and a mode that matches.
    let empty =
        prepare_session(&home, &source, &tasks, "two-3", 1, Some(MemoryMode::Keep)).unwrap();
    let error = prepare_session(&home, &source, &tasks, "two-3", 2, None).unwrap_err();
    assert!(error.contains("finish --session 1"), "{error}");
    // A later session reads the task from state.json; a different --task is refused.
    let error = prepare_named(&home, &source, &tasks, "two-3", 2, None, Some("other")).unwrap_err();
    assert!(error.contains("leave --task out"), "{error}");
    write(
        &home.session_head_path("r1", "two-3", 1),
        &format!("{base}\n"),
    );
    let error =
        prepare_session(&home, &source, &tasks, "two-3", 2, Some(MemoryMode::Clear)).unwrap_err();
    assert!(error.contains("--memory keep"), "{error}");
    assert_eq!(empty.state.memory_mode, Some(MemoryMode::Keep));
    assert!(prepare_session(&home, &source, &tasks, "two-3", 3, None).is_err());
}

#[test]
fn finish_records_head_commands_and_memory_files() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "long-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "long-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 1,
        memory_mode: Some(MemoryMode::Clear),
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    write(
        &home.session_id_path("r1", "long-1", 1),
        &json!({"sessionId": "s-1"}).to_string(),
    );
    write(&home.root.join("memory/a.md"), "# a\n");
    write(&home.root.join("memory/notes.txt"), "no\n");
    write(&home.root.join("projects/git-abc/memory/b.md"), "# b\n");
    let events = [
        command_event(1, "cargo test"),
        command_event(2, "git add -A\nls"),
        file_change_event(3, &home.root.join("memory/a.md")),
        file_read_event(4, &home.root.join("memory/a.md")),
    ]
    .join("\n");
    write(&home.session_events("s-1"), &events);
    write(&repo.join("next.txt"), "next\n");
    commit(&repo, "tc(edit): change", None);
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let finished = canter_eval::finish::finish(&home, "r1", "long-1", 1).unwrap();
    assert_eq!(finished.head.as_deref(), Some(head.as_str()));
    assert_eq!(finished.commands, 2);
    assert_eq!(finished.memory_files, 2);
    assert_eq!(
        std::fs::read_to_string(home.session_head_path("r1", "long-1", 1))
            .unwrap()
            .trim(),
        head
    );
    assert_eq!(
        std::fs::read_to_string(home.session_commands_path("r1", "long-1", 1)).unwrap(),
        "cargo test\ngit add -A; ls\n"
    );
    let memory: Value = serde_json::from_str(
        &std::fs::read_to_string(home.session_memory_path("r1", "long-1", 1)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        memory["files"],
        json!(["memory/a.md", "projects/git-abc/memory/b.md"])
    );

    let error = canter_eval::finish::finish(&home, "r1", "long-1", 1).unwrap_err();
    assert!(error.contains("already recorded"), "{error}");
    assert!(canter_eval::finish::finish(&home, "r1", "long-1", 2).is_err());
}

#[tokio::test]
async fn steer_finds_the_new_session_after_the_recorded_one() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = two_session_task(temp.path(), &base);
    let state = State {
        task: "two".into(),
        task_path: tasks.join("two.toml").display().to_string(),
        round: "r1".into(),
        run: "two-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home.session_id_path("r1", "two-1", 1).display().to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 2,
        memory_mode: Some(MemoryMode::Clear),
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    write(
        &home.session_id_path("r1", "two-1", 1),
        &json!({"sessionId": "s-old"}).to_string(),
    );
    let other = temp.path().join("other");
    let list = json!({"data": [
        listed("s-old", &repo, None),
        listed("s-child", &repo, Some("s-new")),
        listed("s-new", &repo, None),
        listed("s-elsewhere", &other, None),
    ]});
    let fake_dir = temp.path().join("fake-a");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_server_with(&fake_dir, vec![("/v1/sessions", list)], json!({})).await;
    register_fake(&home, &fake);

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "two-1".into(),
                session: 2,
                poll: Duration::from_millis(50),
                timeout: Duration::from_secs(30),
            })
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(home.session_id_path("r1", "two-1", 2)).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["sessionId"], "s-new");

    // Two unrecorded sessions still stop with both named, not with the old one.
    std::fs::remove_file(home.session_id_path("r1", "two-1", 2)).unwrap();
    let fake_dir = temp.path().join("fake-b");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_server_with(
        &fake_dir,
        vec![(
            "/v1/sessions",
            json!({"data": [
                listed("s-old", &repo, None),
                listed("s-a", &repo, None),
                listed("s-b", &repo, None),
            ]}),
        )],
        json!({}),
    )
    .await;
    register_fake(&home, &fake);
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home,
            round: "r1".into(),
            run: "two-1".into(),
            session: 2,
            poll: Duration::from_millis(50),
            timeout: Duration::from_secs(30),
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("s-a") && error.contains("s-b"), "{error}");
    assert!(!error.contains("s-old"), "{error}");
    assert!(
        error.contains("sessions/2/session.json"),
        "the error must name this session's file: {error}"
    );
}

#[tokio::test]
async fn steer_reads_a_first_version_session_file_without_excluding_it() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    // A task with no steers: steer only has to resolve the session and return.
    let task_path = temp.path().join("quiet.toml");
    write(
        &task_path,
        &format!(
            "id = \"quiet\"\nname = \"quiet\"\nrepo = \"canter-test-ts\"\nbase = \"{base}\"\nprompt = \"do it\"\n"
        ),
    );
    let run_dir = home.run_dir("r1", "quiet-1");
    // The first version wrote the run's only session at the run level and no
    // `sessionCount` or `memoryMode` fields.
    write(
        &run_dir.join("state.json"),
        &json!({
            "task": "quiet", "taskPath": task_path, "round": "r1", "run": "quiet-1",
            "repo": repo, "repoName": "canter-test-ts", "baseCommit": base,
            "worktree": repo, "sessionFile": run_dir.join("session.json"),
            "model": "m", "createdAt": 0
        })
        .to_string(),
    );
    write(
        &run_dir.join("session.json"),
        &json!({"sessionId": "s-legacy"}).to_string(),
    );
    let fake_dir = temp.path().join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let mut fake = fake_server_with(
        &fake_dir,
        vec![(
            "/v1/sessions",
            json!({"data": [
                listed("s-legacy", &repo, None),
                listed("s-other", &repo, None),
            ]}),
        )],
        json!({}),
    )
    .await;
    register_fake(&home, &fake);

    tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home: home.clone(),
            round: "r1".into(),
            run: "quiet-1".into(),
            session: 1,
            poll: Duration::from_millis(50),
            timeout: Duration::from_secs(30),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // The recorded legacy id is used as is: not excluded as a past session,
    // and not replaced by the other candidate.
    assert!(!home.session_id_path("r1", "quiet-1", 1).exists());
    assert!(fake.next(Duration::from_millis(200)).await.is_none());
}

fn memory_repo(dir: &Path) -> String {
    init_repo(dir);
    write(
        &dir.join("Cargo.toml"),
        "[package]\nname = \"timecard-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nname = \"timecard\"\npath = \"src/lib.rs\"\n\n[dependencies]\n",
    );
    write(
        &dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\
         #[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(1, 2), 3);\n    }\n}\n",
    );
    write(&dir.join("docs/commands.md"), "# Commands\n");
    write(&dir.join("README.md"), "# timecard\n\nUsage notes.\n");
    write(
        &dir.join("AGENTS.md"),
        "# 仓库说明\n\n只用标准库，不加依赖。\n",
    );
    write(
        &dir.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"timecard-fixture\"\nversion = \"0.1.0\"\n",
    );
    let todo: String = [
        "Add an edit command.",
        "Support weekly reports.",
        "Add CSV export.",
        "Detect malformed date ranges.",
        "Make concurrent saves safer.",
        "Add entry templates.",
        "Improve Unicode table alignment.",
        "Add monthly project comparisons.",
    ]
    .iter()
    .map(|title| format!("- [ ] **{title}** Do it.\n"))
    .collect();
    write(&dir.join("TODO.md"), &todo);
    commit(dir, "初始骨架", None);
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn check_todo(dir: &Path, title: &str) {
    let text = std::fs::read_to_string(dir.join("TODO.md")).unwrap();
    let line = format!("- [ ] **{title}**");
    assert!(text.contains(&line), "TODO.md has no {line:?}");
    write(
        &dir.join("TODO.md"),
        &text.replace(&line, &format!("- [x] **{title}**")),
    );
}

/// One commit per session of the memory task; sessions 2-4 also touch
/// `docs/commands.md` because the remembered rule says so.
fn memory_commits(dir: &Path) -> Vec<String> {
    let mut heads = Vec::new();
    for (n, (title, subject)) in [
        ("Add an edit command.", "tc(edit): add edit command"),
        ("Support weekly reports.", "tc(summary): add weekly reports"),
        ("Add CSV export.", "tc(export): add csv export"),
        (
            "Add monthly project comparisons.",
            "tc(compare): add monthly comparisons",
        ),
    ]
    .iter()
    .enumerate()
    {
        check_todo(dir, title);
        let mut doc = std::fs::read_to_string(dir.join("docs/commands.md")).unwrap();
        doc.push_str(&format!("## {subject}\n\nusage here\n"));
        write(&dir.join("docs/commands.md"), &doc);
        let mut source = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        source.push_str(&format!("\n// session {}\n", n + 1));
        write(&dir.join("src/lib.rs"), &source);
        commit(dir, subject, None);
        heads.push(git(dir, &["rev-parse", "HEAD"]).trim().to_string());
    }
    heads
}

fn run_check(task: &Task, id: &str, worktree: &Path, base: &str, run_dir: &Path) -> bool {
    let check = task
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check {id}"));
    Command::new("sh")
        .arg("-c")
        .arg(&check.command)
        .current_dir(worktree)
        .env("BASE", base)
        .env("REPO", worktree)
        .env("WORKTREE", worktree)
        .env("RUN_DIR", run_dir)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn memory_checks_pass_and_fail_on_the_expected_worktree() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    let base = memory_repo(&repo);
    let heads = memory_commits(&repo);
    let task =
        Task::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory.toml")).unwrap();
    let state = State {
        task: "memory".into(),
        task_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tasks/memory.toml")
            .display()
            .to_string(),
        round: "r1".into(),
        run: "mem-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-rust".into(),
        base_commit: base.clone(),
        worktree: repo.display().to_string(),
        session_file: home.session_id_path("r1", "mem-1", 1).display().to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 4,
        memory_mode: Some(MemoryMode::Clear),
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    let run_dir = home.run_dir("r1", "mem-1");
    for (index, head) in heads.iter().enumerate() {
        let n = index + 1;
        write(
            &home.session_head_path("r1", "mem-1", n),
            &format!("{head}\n"),
        );
        if n >= 2 {
            write(
                &home.session_commands_path("r1", "mem-1", n),
                "cargo test\ncargo clippy -- -D warnings\n",
            );
        }
    }

    let file = canter_eval::grade::grade(&home, "r1", "mem-1", &mut Vec::new()).unwrap();
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{} failed: {}",
            check.id,
            check.output_tail
        );
    }

    // A blanket `git add` in a judged session's commands fails the rule check,
    // whatever global options and spacing it uses; adding named files passes.
    let commands = home.session_commands_path("r1", "mem-1", 2);
    let saved = std::fs::read_to_string(&commands).unwrap();
    for failing in [
        "git add -A",
        "git  add  -A",
        "git add --all",
        "git add .",
        "cargo test && git add .",
        "cargo test; git add -A",
        "git -C \"$WORKTREE\" add .",
        "git add -- .",
        "git add -v -A",
        "git add --verbose --all",
        "git add src/lib.rs -v -A",
    ] {
        write(&commands, &format!("{saved}{failing}\n"));
        assert!(
            !run_check(&task, "no-blanket-git-add", &repo, &base, &run_dir),
            "{failing:?} must fail the check"
        );
    }
    for passing in [
        "git add .gitignore",
        "git add ./src/cli.rs",
        "git add -- .gitignore",
        "git add -v .gitignore",
        "cargo test && git add src/cli.rs TODO.md",
    ] {
        write(&commands, &format!("{saved}{passing}\n"));
        assert!(
            run_check(&task, "no-blanket-git-add", &repo, &base, &run_dir),
            "{passing:?} must pass the check"
        );
    }
    write(&commands, &saved);

    // An unchecked TODO item fails.
    let todo = std::fs::read_to_string(repo.join("TODO.md")).unwrap();
    write(
        &repo.join("TODO.md"),
        &todo.replacen("- [x] **Add CSV export.**", "- [ ] **Add CSV export.**", 1),
    );
    assert!(!run_check(
        &task,
        "four-todos-checked",
        &repo,
        &base,
        &run_dir
    ));
    write(&repo.join("TODO.md"), &todo);

    // A dependency declared through a `[dependencies.foo]` subtable fails:
    // the lock file gains its package, which the check compares by name.
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    let lock = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
    write(
        &repo.join("Cargo.toml"),
        &format!("{manifest}\n[dependencies.foo]\nversion = \"1\"\n"),
    );
    write(
        &repo.join("Cargo.lock"),
        &format!("{lock}\n[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\n"),
    );
    assert!(!run_check(
        &task,
        "dependencies-unchanged",
        &repo,
        &base,
        &run_dir
    ));
    write(&repo.join("Cargo.toml"), &manifest);
    write(&repo.join("Cargo.lock"), &lock);
    assert!(run_check(
        &task,
        "dependencies-unchanged",
        &repo,
        &base,
        &run_dir
    ));

    // A commit subject that does not follow the remembered rule fails.
    write(&repo.join("notes.txt"), "later\n");
    commit(&repo, "update csv export", None);
    let bad = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let head3 = home.session_head_path("r1", "mem-1", 3);
    let saved = std::fs::read_to_string(&head3).unwrap();
    write(&head3, &format!("{bad}\n"));
    assert!(!run_check(
        &task,
        "tc-commit-subjects",
        &repo,
        &base,
        &run_dir
    ));
    write(&head3, &saved);
    assert!(run_check(
        &task,
        "tc-commit-subjects",
        &repo,
        &base,
        &run_dir
    ));

    // A session that did not touch docs/commands.md fails.
    let head4 = home.session_head_path("r1", "mem-1", 4);
    let saved = std::fs::read_to_string(&head4).unwrap();
    write(&head4, &std::fs::read_to_string(&head3).unwrap());
    assert!(!run_check(
        &task,
        "commands-doc-per-session",
        &repo,
        &base,
        &run_dir
    ));
    write(&head4, &saved);

    // A failing test fails the cargo check.
    let source = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
    write(
        &repo.join("src/lib.rs"),
        &source.replace("assert_eq!(super::add(1, 2), 3);", "assert!(false);"),
    );
    assert!(!run_check(&task, "tests-and-lints", &repo, &base, &run_dir));
    write(&repo.join("src/lib.rs"), &source);
}

#[test]
fn memory_2_checks_pass_and_fail_on_the_expected_worktree() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    let base = memory_repo(&repo);
    let heads = memory_commits(&repo);
    let task_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory-2.toml");
    let task = Task::load(&task_path).unwrap();
    let state = State {
        task: "memory-2".into(),
        task_path: task_path.display().to_string(),
        round: "r1".into(),
        run: "mem2-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-rust".into(),
        base_commit: base.clone(),
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "mem2-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 4,
        memory_mode: Some(MemoryMode::Clear),
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    let run_dir = home.run_dir("r1", "mem2-1");
    for (index, head) in heads.iter().enumerate() {
        let n = index + 1;
        write(
            &home.session_head_path("r1", "mem2-1", n),
            &format!("{head}\n"),
        );
        if n >= 2 {
            write(
                &home.session_commands_path("r1", "mem2-1", n),
                "cargo test\ncargo clippy -- -D warnings\ncargo doc --no-deps\n",
            );
        }
    }

    let file = canter_eval::grade::grade(&home, "r1", "mem2-1", &mut Vec::new()).unwrap();
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{} failed: {}",
            check.id,
            check.output_tail
        );
    }

    // A session whose commits touch README.md fails, and only that session's
    // range matters.
    write(
        &repo.join("README.md"),
        "# timecard\n\nA session rewrote me.\n",
    );
    commit(&repo, "tc(edit): mention the edit command", None);
    let readme_head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let head4 = home.session_head_path("r1", "mem2-1", 4);
    let saved = std::fs::read_to_string(&head4).unwrap();
    write(&head4, &format!("{readme_head}\n"));
    assert!(!run_check(
        &task,
        "readme-untouched",
        &repo,
        &base,
        &run_dir
    ));
    write(&head4, &saved);
    assert!(run_check(&task, "readme-untouched", &repo, &base, &run_dir));

    // Each judged session has to run cargo doc; alone or chained counts.
    let commands = home.session_commands_path("r1", "mem2-1", 3);
    let saved = std::fs::read_to_string(&commands).unwrap();
    write(&commands, "cargo test\ncargo clippy -- -D warnings\n");
    assert!(!run_check(
        &task,
        "cargo-doc-per-session",
        &repo,
        &base,
        &run_dir
    ));
    for passing in [
        "cargo doc --no-deps",
        "cargo test && cargo doc --no-deps",
        "cargo test; cargo doc --no-deps",
    ] {
        write(
            &commands,
            &format!("cargo test\ncargo clippy -- -D warnings\n{passing}\n"),
        );
        assert!(
            run_check(&task, "cargo-doc-per-session", &repo, &base, &run_dir),
            "{passing:?} must pass the check"
        );
    }
    write(&commands, &saved);

    // The rules reaching the repository through AGENTS.md fails, edited in the
    // worktree...
    let agents = repo.join("AGENTS.md");
    let saved_agents = std::fs::read_to_string(&agents).unwrap();
    write(&agents, "# 仓库说明\n\n只改 src/ 和 docs/。\n");
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));
    write(&agents, &saved_agents);
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));

    // ...or committed there (a separate repository, so this one stays clean).
    let committed_repo = temp.path().join("committed");
    let committed_base = memory_repo(&committed_repo);
    write(
        &committed_repo.join("AGENTS.md"),
        "# 仓库说明\n\n只改 src/ 和 docs/。\n",
    );
    commit(&committed_repo, "docs: describe the workflow", None);
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &committed_repo,
        &committed_base,
        &run_dir
    ));

    // A new untracked file that spells out the rule fails.
    let notes = repo.join("notes.md");
    write(&notes, "before committing, run cargo doc --no-deps\n");
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));
    std::fs::remove_file(&notes).unwrap();
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));

    // So does a global AGENTS.md in the eval state directory.
    write(&home.root.join("AGENTS.md"), "全局说明\n");
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));
    std::fs::remove_file(home.root.join("AGENTS.md")).unwrap();
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &repo,
        &base,
        &run_dir
    ));
}

#[test]
fn memory_global_checks_pass_and_fail_on_the_expected_worktree() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let rust = temp.path().join("rust");
    let rust_base = memory_repo(&rust);
    let ts = temp.path().join("ts");
    let ts_base = ts_repo(&ts);
    let heads = ts_session_commits(&ts);

    let task_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/memory-global.toml");
    let task = Task::load(&task_path).unwrap();
    let state = State {
        task: "memory-global".into(),
        task_path: task_path.display().to_string(),
        round: "r1".into(),
        run: "memg-1".into(),
        repo: rust.display().to_string(),
        repo_name: "canter-test-rust".into(),
        base_commit: rust_base.clone(),
        worktree: rust.display().to_string(),
        session_file: home
            .session_id_path("r1", "memg-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 4,
        memory_mode: Some(MemoryMode::Clear),
        repos: vec![StateRepo {
            name: "canter-test-ts".into(),
            repo: ts.display().to_string(),
            base_commit: ts_base.clone(),
            worktree: ts.display().to_string(),
        }],
    };
    state.save(&home).unwrap();
    let run_dir = home.run_dir("r1", "memg-1");
    write(
        &home.session_head_path("r1", "memg-1", 1),
        &format!("{rust_base}\n"),
    );
    for (index, head) in heads.iter().enumerate() {
        let n = index + 2;
        write(
            &home.session_head_path("r1", "memg-1", n),
            &format!("{head}\n"),
        );
        write(
            &home.session_commands_path("r1", "memg-1", n),
            "npm test\nnpx tsc --noEmit\ngit diff --check\n",
        );
    }
    let memory = home.session_memory_path("r1", "memg-1", 1);
    write(
        &memory,
        &json!({"files": ["memory/assisted-by.md"]}).to_string(),
    );

    let file = canter_eval::grade::grade(&home, "r1", "memg-1", &mut Vec::new()).unwrap();
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{} failed: {}",
            check.id,
            check.output_tail
        );
    }

    // An unchecked TODO item fails.
    let todo = std::fs::read_to_string(ts.join("TODO.md")).unwrap();
    write(
        &ts.join("TODO.md"),
        &todo.replacen(
            "- [x] **Add habit archiving.**",
            "- [ ] **Add habit archiving.**",
            1,
        ),
    );
    assert!(!run_check(
        &task,
        "three-todos-checked",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&ts.join("TODO.md"), &todo);
    assert!(run_check(
        &task,
        "three-todos-checked",
        &ts,
        &ts_base,
        &run_dir
    ));

    // A failing test fails the test-and-type check.
    let tests = std::fs::read_to_string(ts.join("tests/habit.test.js")).unwrap();
    write(
        &ts.join("tests/habit.test.js"),
        &format!("{tests}test(\"broken\", () => {{ throw new Error(\"broken\"); }});\n"),
    );
    assert!(!run_check(
        &task,
        "tests-and-types",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&ts.join("tests/habit.test.js"), &tests);
    assert!(run_check(&task, "tests-and-types", &ts, &ts_base, &run_dir));

    // A new dependency fails.
    let manifest = std::fs::read_to_string(ts.join("package.json")).unwrap();
    let mut with_dependency: Value = serde_json::from_str(&manifest).unwrap();
    with_dependency["devDependencies"]["left-pad"] = json!("1.0.0");
    write(
        &ts.join("package.json"),
        &serde_json::to_string_pretty(&with_dependency).unwrap(),
    );
    assert!(!run_check(
        &task,
        "dependencies-unchanged",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&ts.join("package.json"), &manifest);
    assert!(run_check(
        &task,
        "dependencies-unchanged",
        &ts,
        &ts_base,
        &run_dir
    ));

    // A commit without the trailer in a judged session fails, and only the
    // commits of that session's range count.
    write(&ts.join("notes.txt"), "later\n");
    commit(&ts, "session 4 without a trailer", None);
    let bare = git(&ts, &["rev-parse", "HEAD"]).trim().to_string();
    let head4 = home.session_head_path("r1", "memg-1", 4);
    let saved = std::fs::read_to_string(&head4).unwrap();
    write(&head4, &format!("{bare}\n"));
    assert!(!run_check(
        &task,
        "assisted-by-trailer",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&head4, &saved);
    assert!(run_check(
        &task,
        "assisted-by-trailer",
        &ts,
        &ts_base,
        &run_dir
    ));

    // A session that did not run git diff --check fails; alone or chained counts.
    let commands = home.session_commands_path("r1", "memg-1", 3);
    let saved = std::fs::read_to_string(&commands).unwrap();
    write(&commands, "npm test\nnpx tsc --noEmit\n");
    assert!(!run_check(
        &task,
        "diff-check-per-session",
        &ts,
        &ts_base,
        &run_dir
    ));
    for passing in [
        "git diff --check",
        "git add -A && git diff --check",
        "npm test; git diff --check",
    ] {
        write(
            &commands,
            &format!("npm test\nnpx tsc --noEmit\n{passing}\n"),
        );
        assert!(
            run_check(&task, "diff-check-per-session", &ts, &ts_base, &run_dir),
            "{passing:?} must pass the check"
        );
    }
    write(&commands, &saved);

    // The global memory is where the preference has to end up: project memory
    // alone, or no memory at all, fails.
    write(&memory, &json!({"files": []}).to_string());
    assert!(!run_check(
        &task,
        "preference-in-global-memory",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(
        &memory,
        &json!({"files": ["projects/git-abc/memory/preference.md"]}).to_string(),
    );
    assert!(!run_check(
        &task,
        "preference-in-global-memory",
        &ts,
        &ts_base,
        &run_dir
    ));
    std::fs::remove_file(&memory).unwrap();
    assert!(!run_check(
        &task,
        "preference-in-global-memory",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(
        &memory,
        &json!({"files": ["memory/assisted-by.md"]}).to_string(),
    );
    assert!(run_check(
        &task,
        "preference-in-global-memory",
        &ts,
        &ts_base,
        &run_dir
    ));

    // The habits reaching the repository fails: an edited AGENTS.md...
    let agents = ts.join("AGENTS.md");
    let saved_agents = std::fs::read_to_string(&agents).unwrap();
    write(
        &agents,
        "# 仓库说明\n\n每个提交说明的最后一行写 Assisted-by: canter。\n",
    );
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&agents, &saved_agents);
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));

    // ...an added line elsewhere...
    let readme = std::fs::read_to_string(ts.join("README.md")).unwrap();
    write(
        &ts.join("README.md"),
        &format!("{readme}\n提交前先跑 git diff --check。\n"),
    );
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));
    write(&ts.join("README.md"), &readme);

    // ...or a new untracked file that spells it out.
    let notes = ts.join("habits.md");
    write(&notes, "每个提交说明的最后一行写 Assisted-by: canter\n");
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));
    std::fs::remove_file(&notes).unwrap();
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));

    // The eval state directory's own AGENTS.md would be read too.
    write(&home.root.join("AGENTS.md"), "全局说明\n");
    assert!(!run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));
    std::fs::remove_file(home.root.join("AGENTS.md")).unwrap();
    assert!(run_check(
        &task,
        "rules-not-in-repo",
        &ts,
        &ts_base,
        &run_dir
    ));
}

#[test]
fn prepare_makes_a_worktree_for_every_repository_and_finish_uses_the_session_one() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    let rust = temp.path().join("canter-test-rust");
    init_repo(&rust);
    write(&rust.join("rust-marker.txt"), "rust\n");
    commit(&rust, "rust-base", None);
    let rust_base = git(&rust, &["rev-parse", "HEAD"]).trim().to_string();
    let ts = temp.path().join("canter-test-ts");
    init_repo(&ts);
    write(&ts.join("ts-marker.txt"), "ts\n");
    commit(&ts, "ts-base", None);
    let ts_base = git(&ts, &["rev-parse", "HEAD"]).trim().to_string();
    let source = source_config(temp.path());
    let worktrees = temp.path().join("worktrees");
    write_local_config_repos(
        &home,
        &[("canter-test-rust", &rust), ("canter-test-ts", &ts)],
        &source,
        &worktrees,
    );
    let tasks = multi_repo_task(temp.path(), &rust_base, &ts_base);

    let first = prepare_named(&home, &source, &tasks, "multi-1", 1, None, Some("multi")).unwrap();
    let main = Path::new(&first.state.worktree).to_path_buf();
    assert_eq!(first.repo.as_ref().unwrap().name, "canter-test-rust");
    assert!(main.starts_with(&worktrees));
    assert_eq!(first.state.repos.len(), 1);
    let ts_worktree = Path::new(&first.state.repos[0].worktree).to_path_buf();
    assert_eq!(first.state.repos[0].name, "canter-test-ts");
    assert_eq!(
        ts_worktree,
        worktrees
            .join("r1")
            .join("@repos")
            .join("multi-1")
            .join("canter-test-ts")
    );
    assert_ne!(ts_worktree, main);
    assert_eq!(git(&main, &["rev-parse", "HEAD"]).trim(), rust_base);
    assert_eq!(git(&ts_worktree, &["rev-parse", "HEAD"]).trim(), ts_base);
    assert!(ts_worktree.join("prepared.txt").is_file());
    assert!(!main.join("prepared.txt").exists());
    let summary = first.summary(&home);
    assert!(
        summary.contains("session repo: canter-test-rust"),
        "{summary}"
    );
    assert!(summary.contains(&ts_worktree.display().to_string()));
    assert!(summary.contains(&format!("--cwd {}", main.display())));

    // finish records the HEAD of the session's own repository: a commit in the
    // other worktree does not move session 1's record.
    write(
        &home.session_id_path("r1", "multi-1", 1),
        &json!({"sessionId": "s-rust"}).to_string(),
    );
    write(&home.session_events("s-rust"), "");
    write(&ts_worktree.join("later.txt"), "later\n");
    commit(&ts_worktree, "ts later", None);
    let ts_head = git(&ts_worktree, &["rev-parse", "HEAD"]).trim().to_string();
    let finished = canter_eval::finish::finish(&home, "r1", "multi-1", 1).unwrap();
    assert_eq!(finished.head.as_deref(), Some(rust_base.as_str()));

    // Session 2 runs in the second repository's worktree, once session 1 is
    // recorded.
    let second = prepare_named(&home, &source, &tasks, "multi-1", 2, None, None).unwrap();
    assert_eq!(second.repo.as_ref().unwrap().name, "canter-test-ts");
    let summary = second.summary(&home);
    assert!(
        summary.contains("session repo: canter-test-ts"),
        "{summary}"
    );
    // The `@repos` segment is quoted in the printed commands.
    assert!(summary.contains(&format!("--cwd '{}'", ts_worktree.display())));
    assert!(summary.contains("ts prompt"));

    write(
        &home.session_id_path("r1", "multi-1", 2),
        &json!({"sessionId": "s-ts"}).to_string(),
    );
    write(&home.session_events("s-ts"), "");
    let finished = canter_eval::finish::finish(&home, "r1", "multi-1", 2).unwrap();
    assert_eq!(finished.head.as_deref(), Some(ts_head.as_str()));

    // Each check runs in its own repository, with that repository's variables.
    let file = canter_eval::grade::grade(&home, "r1", "multi-1", &mut Vec::new()).unwrap();
    assert_eq!(file.results.len(), 2);
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{} failed: {}",
            check.id,
            check.output_tail
        );
    }

    // clean removes both worktrees and leaves what the state does not record.
    let kept = worktrees.join("r1").join("kept-by-hand");
    std::fs::create_dir_all(&kept).unwrap();
    canter_eval::prepare::clean(&home, &first.state).unwrap();
    assert!(!main.exists());
    assert!(!ts_worktree.exists());
    assert!(kept.is_dir());
}

#[test]
fn clean_of_one_run_never_removes_another_runs_worktree() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    let rust = temp.path().join("canter-test-rust");
    init_repo(&rust);
    write(&rust.join("rust-marker.txt"), "rust\n");
    commit(&rust, "rust-base", None);
    let rust_base = git(&rust, &["rev-parse", "HEAD"]).trim().to_string();
    let ts = temp.path().join("canter-test-ts");
    init_repo(&ts);
    write(&ts.join("ts-marker.txt"), "ts\n");
    commit(&ts, "ts-base", None);
    let ts_base = git(&ts, &["rev-parse", "HEAD"]).trim().to_string();
    let source = source_config(temp.path());
    let worktrees = temp.path().join("worktrees");
    write_local_config_repos(
        &home,
        &[("canter-test-rust", &rust), ("canter-test-ts", &ts)],
        &source,
        &worktrees,
    );
    let tasks = multi_repo_task(temp.path(), &rust_base, &ts_base);
    let single_tasks = temp.path().join("tasks-single");
    write(
        &single_tasks.join("single.toml"),
        &format!(
            "id = \"single\"\nname = \"single\"\nrepo = \"canter-test-ts\"\nbase = \"{ts_base}\"\n\nprompt = \"single prompt\"\n"
        ),
    );

    // Run `a` has a worktree for the additional repository; cleaning it frees
    // those paths.
    let a = prepare_named(&home, &source, &tasks, "a", 1, None, Some("multi")).unwrap();
    let a_main = Path::new(&a.state.worktree).to_path_buf();
    let a_ts = Path::new(&a.state.repos[0].worktree).to_path_buf();
    assert_eq!(
        a_ts,
        worktrees
            .join("r1")
            .join("@repos")
            .join("a")
            .join("canter-test-ts")
    );
    canter_eval::prepare::clean(&home, &a.state).unwrap();
    assert!(!a_main.exists());
    assert!(!a_ts.exists());

    // A later run whose name spells `<a>-<repository>` gets its own path, and
    // its uncommitted file stays there.
    let b = prepare_named(
        &home,
        &source,
        &single_tasks,
        "a-canter-test-ts",
        1,
        None,
        Some("single"),
    )
    .unwrap();
    let b_worktree = Path::new(&b.state.worktree).to_path_buf();
    assert_eq!(b_worktree, worktrees.join("r1").join("a-canter-test-ts"));
    assert_ne!(b_worktree, a_ts);
    write(&b_worktree.join("uncommitted.txt"), "keep me\n");

    // Cleaning the old run again leaves the new run alone.
    canter_eval::prepare::clean(&home, &a.state).unwrap();
    assert!(b_worktree.is_dir());
    assert_eq!(
        std::fs::read_to_string(b_worktree.join("uncommitted.txt")).unwrap(),
        "keep me\n"
    );

    // A state recording another run's worktree is refused, and nothing is
    // deleted: the path was freed once and belongs to someone else now.
    let mut borrowed = a.state.clone();
    borrowed.repos[0].worktree = b_worktree.display().to_string();
    let error = canter_eval::prepare::clean(&home, &borrowed).unwrap_err();
    assert!(error.contains("refusing"), "{error}");
    assert!(b_worktree.join("uncommitted.txt").is_file());

    canter_eval::prepare::clean(&home, &b.state).unwrap();
    assert!(!b_worktree.exists());
}

#[tokio::test]
async fn trust_trusts_every_worktree_of_the_run() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let rust = temp.path().join("rust");
    std::fs::create_dir_all(&rust).unwrap();
    let ts = temp.path().join("ts");
    std::fs::create_dir_all(&ts).unwrap();
    let state = State {
        task: "two".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "multi-1".into(),
        repo: rust.display().to_string(),
        repo_name: "canter-test-rust".into(),
        base_commit: "abc".into(),
        worktree: rust.display().to_string(),
        session_file: home
            .session_id_path("r1", "multi-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 2,
        memory_mode: None,
        repos: vec![StateRepo {
            name: "canter-test-ts".into(),
            repo: ts.display().to_string(),
            base_commit: "def".into(),
            worktree: ts.display().to_string(),
        }],
    };
    let mut fake = fake_server(
        temp.path(),
        json!({"path": rust, "trusted": true, "warning": null}),
    )
    .await;
    register_fake(&home, &fake);
    let message = canter_eval::server::trust(&home, &state).await.unwrap();
    assert!(message.contains("trusted"), "{message}");
    let (_, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(path, "/v1/folderTrust");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["cwd"],
        rust.display().to_string()
    );
    let (_, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(path, "/v1/folderTrust");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["cwd"],
        ts.display().to_string()
    );
}

fn stats_for(session_id: &str, started: i64, updated: i64, input_tokens: u64, cost: f64) -> Value {
    json!({
        "sessionId": session_id,
        "eventSeq": 1,
        "overview": {
            "cwd": "/w", "model": "m", "startedAt": started, "updatedAt": updated,
            "tags": {}, "turnCount": 1, "requestCount": 4,
            "usage": {"data": [], "total": {
                "cachedInputTokens": 100, "uncachedInputTokens": 200,
                "outputTokens": 300, "cost": {"usd": cost, "unknown": false}}},
            "retryCount": 0, "recordedRetryCount": 0
        },
        "requests": [{
            "startedAt": 1, "purpose": "conversation", "outcome": "completed",
            "toolCallIds": [],
            "usage": {"inputTokens": input_tokens, "cachedInputTokens": 0,
                      "outputTokens": 10, "reasoningOutputTokens": 2,
                      "totalTokens": input_tokens + 10},
            "context": {"estimated": true, "systemTokens": 10,
                        "toolDefinitionTokens": 10, "conversationTokens": 10,
                        "toolResultTokens": 10}
        }],
        "compactions": [],
        "tools": [{"name": "read", "callCount": 2, "failureCount": 1,
                   "truncatedCount": 0, "averageResultTokens": 1.0,
                   "maxResultTokens": 10, "unknownStatusCount": 0,
                   "unknownTruncatedCount": 0, "unknownResultSizeCount": 0}],
        "toolCalls": [],
        "approvals": {"count": 0, "data": []}
    })
}

#[test]
fn collect_a_multi_session_run_by_session() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = two_session_task(temp.path(), &base);
    let state = State {
        task: "two".into(),
        task_path: tasks.join("two.toml").display().to_string(),
        round: "r1".into(),
        run: "two-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home.session_id_path("r1", "two-1", 1).display().to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 2,
        memory_mode: Some(MemoryMode::Keep),
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    canter_eval::paths::write_private(
        &home.run_dir("r1", "two-1").join("checks.json"),
        serde_json::to_vec_pretty(&CheckFile {
            ran_at: 0,
            results: vec![CheckResult {
                id: "todo-checked".into(),
                description: "checked".into(),
                record: false,
                passed: Some(true),
                value: None,
                output_tail: String::new(),
            }],
        })
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    write(
        &home.session_id_path("r1", "two-1", 1),
        &json!({"sessionId": "s-1"}).to_string(),
    );
    write(
        &home.session_id_path("r1", "two-1", 2),
        &json!({"sessionId": "s-2"}).to_string(),
    );
    write(
        &home.session_memory_path("r1", "two-1", 1),
        &json!({"files": ["memory/a.md"]}).to_string(),
    );
    write(
        &home.session_memory_path("r1", "two-1", 2),
        &json!({"files": ["memory/a.md", "projects/git-x/memory/b.md"]}).to_string(),
    );
    let events_1 = [
        command_event(1, "cargo test"),
        file_change_event(2, &home.root.join("memory/a.md")),
        file_read_event(3, &home.root.join("memory/a.md")),
        file_read_event(4, &repo.join("base.txt")),
        file_change_event(5, &repo.join("base.txt")),
        file_read_event(6, &home.root.join("memory/notes.txt")),
    ]
    .join("\n");
    write(&home.session_events("s-1"), &events_1);
    let events_2 = [
        file_read_event(1, &home.root.join("projects/git-x/memory/b.md")),
        file_change_event(2, &home.root.join("projects/git-x/memory/b.md")),
        file_change_event(3, &home.root.join("memory/a.md")),
    ]
    .join("\n");
    write(&home.session_events("s-2"), &events_2);
    let stats = temp.path().join("stats");
    write(
        &stats.join("1.json"),
        &stats_for("s-1", 1_000, 4_000, 1_000, 0.25).to_string(),
    );
    write(
        &stats.join("2.json"),
        &stats_for("s-2", 10_000, 11_000, 3_000, 0.5).to_string(),
    );

    let result = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "two-1".into(),
        stats_json: Some(stats),
        session: None,
    })
    .unwrap();
    assert_eq!(result.session_count, 2);
    assert_eq!(result.memory_mode, Some(MemoryMode::Keep));
    assert_eq!(result.session_id, "s-1");
    assert_eq!(result.sessions.len(), 2);
    let first = &result.sessions[0];
    assert_eq!(first.index, 1);
    assert_eq!(first.session_id, "s-1");
    assert_eq!(first.duration_ms, Some(3_000));
    assert_eq!(first.cost_usd, Some(0.25));
    assert_eq!(first.memory_writes, 1);
    assert_eq!(first.memory_reads, 1);
    assert_eq!(first.memory_files, 1);
    assert_eq!(first.memory_file_names, ["memory/a.md"]);
    let second = &result.sessions[1];
    assert_eq!(second.memory_writes, 2);
    assert_eq!(second.memory_reads, 1);
    assert_eq!(second.memory_files, 2);
    assert_eq!(result.metrics.duration_ms, Some(4_000));
    assert_eq!(result.metrics.requests, 8);
    assert_eq!(result.metrics.conversation_requests, 2);
    assert_eq!(result.metrics.context_peak_tokens, Some(3_000));
    assert_eq!(result.metrics.context_average_tokens, Some(2_000));
    assert_eq!(result.metrics.cost_usd, Some(0.75));
    assert_eq!(result.metrics.read_calls, 4);
    assert_eq!(result.metrics.tool_failures, 2);
    assert_eq!(result.metrics.event_parse_errors, 0);

    let saved = RunResult::load(&home, "r1", "two-1").unwrap();
    assert_eq!(saved.session_count, 2);
    assert_eq!(saved.sessions[1].memory_files, 2);

    // A session without its id stops instead of guessing.
    std::fs::remove_file(home.session_id_path("r1", "two-1", 2)).unwrap();
    let error = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "two-1".into(),
        stats_json: Some(temp.path().join("stats")),
        session: None,
    })
    .unwrap_err();
    assert!(error.contains("steer --session 2"), "{error}");
}

#[test]
fn report_shows_memory_mode_and_per_session_numbers() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let session = |index: usize, id: &str, writes: usize, reads: usize, files: usize| SessionRow {
        index,
        session_id: id.into(),
        duration_ms: Some(3_000),
        cost_usd: Some(0.1),
        cost_unknown: false,
        memory_writes: writes,
        memory_reads: reads,
        memory_files: files,
        memory_file_names: (0..files).map(|n| format!("memory/{n}.md")).collect(),
    };
    let mut kept = sample_result("mem", "memory-keep-1", 2, 6_000, 0.2);
    kept.task = "memory".into();
    kept.session_count = 2;
    kept.memory_mode = Some(MemoryMode::Keep);
    kept.sessions = vec![session(1, "s-k1", 2, 1, 1), session(2, "s-k2", 1, 2, 2)];
    let mut cleared = sample_result("mem", "memory-clear-1", 1, 4_000, 0.1);
    cleared.task = "memory".into();
    cleared.session_count = 2;
    cleared.memory_mode = Some(MemoryMode::Clear);
    cleared.sessions = vec![session(1, "s-c1", 0, 0, 0), session(2, "s-c2", 0, 0, 0)];
    for result in [&kept, &cleared] {
        canter_eval::paths::write_private(
            &home.run_dir("mem", &result.run).join("result.json"),
            serde_json::to_vec_pretty(result).unwrap().as_slice(),
        )
        .unwrap();
    }
    let out = temp.path().join("results/mem.md");
    let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
        home,
        round: "mem".into(),
        against: None,
        out,
    })
    .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("| 题 | 次 | 记忆 | 判分 |"));
    assert!(text.contains("| memory | memory-keep-1 | 保留 | 2/2 |"));
    assert!(text.contains("| memory | memory-clear-1 | 清空 | 1/2 |"));
    assert!(text.contains("## 通过次数"));
    assert!(text.contains("| 轮次 | 题 | 记忆 | 全部通过 | 平均判分 |"));
    assert!(text.contains("| mem | memory | 保留 | 1/1 | 2.0/2 |"));
    assert!(text.contains("| mem | memory | 清空 | 0/1 | 1.0/2 |"));
    assert!(text.contains("## 会话"));
    assert!(text.contains("| memory-keep-1 | 1 s-k1 | 3s | $0.10 | 2 | 1 | 1 |"));
    assert!(text.contains("| memory-clear-1 | 2 s-c2 | 3s | $0.10 | 0 | 0 | 0 |"));

    // A result without the new fields renders exactly like the first version.
    let new = sample_result("r1", "long-1", 2, 1_000, 0.1);
    let old = {
        let mut value = serde_json::to_value(&new).unwrap();
        let object = value.as_object_mut().unwrap();
        for key in ["sessionCount", "memoryMode", "sessions"] {
            object.remove(key).unwrap();
        }
        value
    };
    let old_home = Home {
        root: temp.path().join("old-home"),
    };
    let new_home = Home {
        root: temp.path().join("new-home"),
    };
    let single_home = Home {
        root: temp.path().join("single-home"),
    };
    let single = {
        let mut value = serde_json::to_value(&new).unwrap();
        value["memoryMode"] = json!("clear");
        value
    };
    for (home, result) in [
        (&old_home, old),
        (&new_home, serde_json::to_value(&new).unwrap()),
        (&single_home, single),
    ] {
        canter_eval::paths::write_private(
            &home.run_dir("r1", "long-1").join("result.json"),
            serde_json::to_vec_pretty(&result).unwrap().as_slice(),
        )
        .unwrap();
    }
    let mut texts = Vec::new();
    for (home, name) in [
        (&old_home, "old"),
        (&new_home, "new"),
        (&single_home, "single"),
    ] {
        let out = temp.path().join(format!("results/{name}.md"));
        let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
            home: home.clone(),
            round: "r1".into(),
            against: None,
            out,
        })
        .unwrap();
        texts.push(std::fs::read_to_string(path).unwrap());
    }
    assert_eq!(
        texts[0], texts[1],
        "a first-version result must render as before"
    );
    assert_eq!(
        texts[0], texts[2],
        "a one-session run with a memory mode must render as before"
    );
    let text = &texts[0];
    assert!(!text.contains("记忆"), "{text}");
    assert!(!text.contains("## 会话"), "{text}");
    assert!(text.contains("| 题 | 次 | 判分 | 未过 | 用时 |"));
    assert!(text.contains("| long | long-1 | 2/2 |"));

    // One-session runs keep the original (round, task) grouping: a first-version
    // result and a new one with a memory mode stay on one throughput row.
    let mixed_home = Home {
        root: temp.path().join("mixed-home"),
    };
    let old_style = {
        let mut value =
            serde_json::to_value(sample_result("mixed", "long-old", 2, 1_000, 0.1)).unwrap();
        let object = value.as_object_mut().unwrap();
        for key in ["sessionCount", "memoryMode", "sessions"] {
            object.remove(key).unwrap();
        }
        value
    };
    let mut new_style = sample_result("mixed", "long-new", 2, 1_000, 0.1);
    new_style.memory_mode = Some(MemoryMode::Clear);
    for (run, result) in [
        ("long-old", old_style),
        ("long-new", serde_json::to_value(&new_style).unwrap()),
    ] {
        canter_eval::paths::write_private(
            &mixed_home.run_dir("mixed", run).join("result.json"),
            serde_json::to_vec_pretty(&result).unwrap().as_slice(),
        )
        .unwrap();
    }
    let out = temp.path().join("results/mixed.md");
    let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
        home: mixed_home,
        round: "mixed".into(),
        against: None,
        out,
    })
    .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("| mixed | long | 2/2 | 2.0/2 |"), "{text}");
    assert_eq!(text.matches("| mixed | long |").count(), 1, "{text}");
}

// ---- 子 agent 题（P2-258）----

fn simple_repo(dir: &Path, subject: &str) -> String {
    init_repo(dir);
    write(&dir.join("marker.txt"), &format!("{subject}\n"));
    commit(dir, subject, None);
    git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

/// A single-session task without a main repository.
fn outside_task(temp: &Path, codex_base: &str, pi_base: &str) -> PathBuf {
    let dir = temp.join("tasks-outside");
    write(
        &dir.join("outside.toml"),
        &format!(
            r#"id = "outside"
name = "outside"
prompt = "在 {{repo:codex}} 和 {{repo:pi-mono}} 里看看"

[[repos]]
name = "codex"
base = "{codex_base}"

[[repos]]
name = "pi-mono"
base = "{pi_base}"

[[checks]]
id = "session-dir"
description = "runs in the session directory, with only the repositories"
command = '''test -z "${{REPO:-}}" && test -d codex && test -d pi-mono && test "$(ls -A | wc -l | tr -d ' ')" -eq 2'''

[[checks]]
id = "named-repo"
description = "runs in the named repository"
repo = "codex"
command = '''test -n "${{REPO}}" && test "$(pwd -P)" = "$(cd "$WORKTREE" && pwd -P)" && test -f "$WORKTREE/marker.txt"'''
"#
        ),
    );
    dir
}

#[test]
fn subagent_task_reads_as_written() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/subagent.toml");
    let task = Task::load(&path).unwrap();
    assert_eq!(task.id, "subagent");
    assert!(
        task.name.contains("子 agent") && task.name.contains("超时"),
        "{}",
        task.name
    );
    assert_eq!(task.repo, None);
    assert_eq!(task.base, None);
    assert_eq!(task.auto_compact_tokens, Some(60_000));
    assert_eq!(task.reasoning.as_deref(), Some("high"));
    assert_eq!(task.session_count(), 1);
    assert!(task.session_repo(1).unwrap().is_none());
    let repos = task.repositories();
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].name, "codex");
    assert_eq!(repos[0].base, "57a808a");
    assert_eq!(repos[1].name, "pi-mono");
    assert_eq!(repos[1].base, "085c84e");

    let prompt = task.prompt(1).unwrap();
    assert!(prompt.starts_with("对比 {repo:codex} 和 {repo:pi-mono} 这两个项目"));
    assert!(prompt.ends_with("超时后怎么告诉模型。"));
    assert_eq!(task.chats.len(), 5);
    let kinds: Vec<_> = task.chats.iter().map(|chat| chat.after).collect();
    assert_eq!(
        kinds,
        [
            canter_eval::task::ChatAfter::Turn,
            canter_eval::task::ChatAfter::Turn,
            canter_eval::task::ChatAfter::Turn,
            canter_eval::task::ChatAfter::Turn,
            canter_eval::task::ChatAfter::Report,
        ]
    );
    let texts: Vec<&str> = task.chats.iter().map(|chat| chat.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "好的，谢谢",
            "{repo:codex} 的最新提交是哪天的？",
            "你觉得这两个项目哪个代码更好读？",
            "等他们调查回来吧",
            "好的，继续等另外一个subagent回来",
        ]
    );
    assert!(task.steers.is_empty());
    let checks: Vec<&str> = task.checks.iter().map(|check| check.id.as_str()).collect();
    assert_eq!(
        checks,
        [
            "spawned-subagents",
            "dispatch-turn-quick",
            "no-wait-in-chat",
            "answer-facts",
            "dispatch-turn-seconds",
            "subagent-requests",
            "subagent-blocked",
            "subagent-rg-replace-blocked",
            "main-reads-and-commands",
            "fifth-chat-sent",
        ]
    );
    for check in &task.checks {
        assert!(task.check_repo(check).is_none(), "check {}", check.id);
        if check.id == "answer-facts"
            || matches!(
                check.id.as_str(),
                "spawned-subagents" | "dispatch-turn-quick" | "no-wait-in-chat"
            )
        {
            assert!(!check.record, "{} judges", check.id);
        } else {
            assert!(check.record, "{} records", check.id);
        }
    }

    // Placeholders become the repositories' absolute worktree paths.
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let state = State {
        task: "subagent".into(),
        task_path: path.display().to_string(),
        round: "r1".into(),
        run: "subagent-1".into(),
        repo: String::new(),
        repo_name: String::new(),
        base_commit: String::new(),
        worktree: temp.path().join("between").display().to_string(),
        session_file: home
            .session_id_path("r1", "subagent-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: vec![
            StateRepo {
                name: "codex".into(),
                repo: "/repos/codex".into(),
                base_commit: "abc".into(),
                worktree: temp.path().join("between/codex").display().to_string(),
            },
            StateRepo {
                name: "pi-mono".into(),
                repo: "/repos/pi-mono".into(),
                base_commit: "def".into(),
                worktree: temp.path().join("between/pi-mono").display().to_string(),
            },
        ],
    };
    let rendered = canter_eval::task::render(prompt, &state).unwrap();
    assert_eq!(
        rendered,
        format!(
            "对比 {} 和 {} 这两个项目是怎么处理工具调用超时的：各自在哪些文件、默认超时多少、超时后怎么告诉模型。",
            temp.path().join("between/codex").display(),
            temp.path().join("between/pi-mono").display()
        )
    );
    let second = canter_eval::task::render(&task.chats[1].text, &state).unwrap();
    assert_eq!(
        second,
        format!(
            "{} 的最新提交是哪天的？",
            temp.path().join("between/codex").display()
        )
    );
    assert!(canter_eval::task::render("{repo:missing}", &state).is_err());
}

#[tokio::test]
async fn prepare_trust_finish_grade_and_clean_a_task_without_a_main_repository() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("eval-home"),
    };
    let codex = temp.path().join("codex-repo");
    let codex_base = simple_repo(&codex, "codex base");
    let pi = temp.path().join("pi-mono-repo");
    let pi_base = simple_repo(&pi, "pi base");
    let source = source_config(temp.path());
    let worktrees = temp.path().join("worktrees");
    write_local_config_repos(
        &home,
        &[("codex", &codex), ("pi-mono", &pi)],
        &source,
        &worktrees,
    );
    let tasks = outside_task(temp.path(), &codex_base, &pi_base);

    let prepared =
        prepare_named(&home, &source, &tasks, "sub-1", 1, None, Some("outside")).unwrap();
    let session_dir = worktrees.join("r1").join("@repos").join("sub-1");
    assert_eq!(prepared.cwd, session_dir);
    assert_eq!(prepared.state.worktree, session_dir.display().to_string());
    assert!(!prepared.state.has_main_repo());
    assert!(prepared.state.repo_name.is_empty());
    assert!(prepared.repo.is_none());
    assert_eq!(prepared.state.all_repos().len(), 2);
    let codex_wt = session_dir.join("codex");
    let pi_wt = session_dir.join("pi-mono");
    assert_eq!(git(&codex_wt, &["rev-parse", "HEAD"]).trim(), codex_base);
    assert_eq!(git(&pi_wt, &["rev-parse", "HEAD"]).trim(), pi_base);
    assert!(
        !worktrees.join("r1").join("sub-1").exists(),
        "a task without a main repository must not make a main worktree"
    );

    let summary = prepared.summary(&home);
    assert!(
        summary.contains(&format!("--cwd '{}'", session_dir.display())),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("session cwd: {}", session_dir.display())),
        "{summary}"
    );
    assert!(
        summary.contains(&codex_wt.display().to_string()),
        "{summary}"
    );
    assert!(summary.contains(&pi_wt.display().to_string()), "{summary}");
    assert!(!summary.contains("{repo:"), "{summary}");

    // Trust covers both worktrees and the directory between them.
    let mut fake = fake_server(temp.path(), json!({"trusted": true, "warning": null})).await;
    register_fake(&home, &fake);
    canter_eval::server::trust(&home, &prepared.state)
        .await
        .unwrap();
    for expected in [&codex_wt, &pi_wt, &session_dir] {
        let (_, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
        assert_eq!(path, "/v1/folderTrust");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["cwd"],
            expected.display().to_string()
        );
    }
    assert!(fake.next(Duration::from_millis(200)).await.is_none());

    // finish records no HEAD: the session is in no repository.
    write(
        &home.session_id_path("r1", "sub-1", 1),
        &json!({"sessionId": "s-sub"}).to_string(),
    );
    write(&home.session_events("s-sub"), "");
    let finished = canter_eval::finish::finish(&home, "r1", "sub-1", 1).unwrap();
    assert_eq!(finished.head, None);
    assert!(!home.session_head_path("r1", "sub-1", 1).exists());
    assert!(home.session_memory_path("r1", "sub-1", 1).is_file());
    assert!(
        canter_eval::finish::finish(&home, "r1", "sub-1", 1)
            .unwrap_err()
            .contains("already recorded")
    );

    // Checks without a repo run in the session directory; named ones in theirs.
    let file = canter_eval::grade::grade(&home, "r1", "sub-1", &mut Vec::new()).unwrap();
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{} failed: {}",
            check.id,
            check.output_tail
        );
    }

    // clean removes this run's worktrees and the directory holding them.
    let kept = worktrees.join("r1").join("kept-by-hand");
    std::fs::create_dir_all(&kept).unwrap();
    canter_eval::prepare::clean(&home, &prepared.state).unwrap();
    assert!(!codex_wt.exists());
    assert!(!pi_wt.exists());
    assert!(!session_dir.exists());
    assert!(kept.is_dir());
}

/// A task whose chat lines are sent at the given moments.
fn chat_task(temp: &Path, base: &str, chats: &[(&str, &str)]) -> PathBuf {
    let dir = temp.join("tasks-chat");
    let mut text = format!(
        "id = \"chat\"\nname = \"chat task\"\nrepo = \"canter-test-ts\"\nbase = \"{base}\"\nprompt = \"开题\"\n"
    );
    for (after, line) in chats {
        text.push_str(&format!(
            "\n[[chat]]\nafter = \"{after}\"\ntext = '''{line}'''\n"
        ));
    }
    write(&dir.join("chat.toml"), &text);
    dir
}

fn chat_state(home: &Home, task_path: &Path, repo: &Path, run: &str) -> State {
    let base = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    let state = State {
        task: "chat".into(),
        task_path: task_path.display().to_string(),
        round: "r1".into(),
        run: run.into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home.session_id_path("r1", run, 1).display().to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(home).unwrap();
    state
}

fn started_event(seq: u64, ts: i64, session: &str, id: &str, text: &str, client: &str) -> String {
    let input = if text.is_empty() {
        json!([])
    } else {
        json!([{"type": "text", "text": text}])
    };
    json!({
        "seq": seq, "sessionId": session, "timestamp": ts, "type": "turn.started",
        "turn": {"id": id, "items": [], "status": "inProgress", "error": null,
                 "input": input, "clientName": client},
        "inputId": null
    })
    .to_string()
}

fn turn_done_event(seq: u64, ts: i64, session: &str, id: &str) -> String {
    json!({
        "seq": seq, "sessionId": session, "timestamp": ts, "type": "turn.completed",
        "turn": {"id": id, "items": [], "status": "completed", "error": null,
                 "input": [], "clientName": ""}
    })
    .to_string()
}

fn item_event(seq: u64, ts: i64, session: &str, turn: &str, item: Value) -> String {
    json!({
        "seq": seq, "sessionId": session, "timestamp": ts, "type": "item.completed",
        "turnId": turn, "item": item
    })
    .to_string()
}

fn spawn_event(seq: u64, ts: i64, session: &str, turn: &str, child: &str, status: &str) -> String {
    item_event(
        seq,
        ts,
        session,
        turn,
        json!({
            "type": "collabAgentToolCall", "id": format!("spawn-{child}"),
            "tool": "spawnAgent", "status": "completed",
            "agents": [{"id": child, "name": "probe", "status": status}]
        }),
    )
}

fn agent_updated_event(seq: u64, ts: i64, session: &str, child: &str, status: &str) -> String {
    json!({
        "seq": seq, "sessionId": session, "timestamp": ts, "type": "agent.updated",
        "agent": {"id": child, "task": "look", "status": status, "result": null,
                  "createdAt": ts, "updatedAt": ts}
    })
    .to_string()
}

fn flush_events(path: &Path, events: &[String]) {
    write(path, &events.join("\n"));
}

fn chat_progress(home: &Home, round: &str, run: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(home.steer_progress_path(round, run)).unwrap())
        .unwrap()
}

#[tokio::test]
async fn chat_steering_sends_in_order_waits_when_busy_and_skips_without_children() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(
        temp.path(),
        &base,
        &[
            ("turn", "好的，谢谢"),
            ("turn", "第二句"),
            ("turn", "第三句"),
            ("turn", "等他们调查回来吧"),
            ("report", "继续等"),
        ],
    );
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-chat"}).to_string(),
    );
    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": null})).await;
    register_fake(&home, &fake);
    let events_path = home.session_events("s-chat");

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "chat-1".into(),
                session: 1,
                poll: Duration::from_millis(20),
                timeout: Duration::from_secs(5),
            })
            .await
        }
    });

    // The opening turn ends: the first line goes out as ordinary input.
    let mut events = vec![
        started_event(1, 1_000, "s-chat", "t1", "开题", ""),
        turn_done_event(2, 16_000, "s-chat", "t1"),
    ];
    flush_events(&events_path, &events);
    let (method, path, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/v1/sessions/s-chat/input")
    );
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["mode"], "queue");
    assert_eq!(body["clientName"], "canter-eval");
    assert_eq!(body["input"][0]["text"], "好的，谢谢");

    // The first line's turn is still running: the second line waits.
    events.push(started_event(
        3,
        20_000,
        "s-chat",
        "t2",
        "好的，谢谢",
        "canter-eval",
    ));
    flush_events(&events_path, &events);
    assert!(fake.next(Duration::from_millis(300)).await.is_none());

    // Each line's turn ends before the next is sent.
    events.push(turn_done_event(4, 25_000, "s-chat", "t2"));
    flush_events(&events_path, &events);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"][0]["text"], "第二句");

    events.push(started_event(
        5,
        30_000,
        "s-chat",
        "t3",
        "第二句",
        "canter-eval",
    ));
    events.push(turn_done_event(6, 31_000, "s-chat", "t3"));
    flush_events(&events_path, &events);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"][0]["text"], "第三句");

    events.push(started_event(
        7,
        40_000,
        "s-chat",
        "t4",
        "第三句",
        "canter-eval",
    ));
    events.push(turn_done_event(8, 41_000, "s-chat", "t4"));
    flush_events(&events_path, &events);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["input"][0]["text"], "等他们调查回来吧");

    // No sub-agent was started, so no report can wake the session: the fifth
    // line is not sent and steering ends instead of waiting.
    events.push(started_event(
        9,
        50_000,
        "s-chat",
        "t5",
        "等他们调查回来吧",
        "canter-eval",
    ));
    events.push(turn_done_event(10, 51_000, "s-chat", "t5"));
    flush_events(&events_path, &events);
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let progress = chat_progress(&home, "r1", "chat-1");
    let chats = progress["chats"].as_array().unwrap();
    assert_eq!(chats.len(), 5, "{progress}");
    for (index, turn) in ["t2", "t3", "t4", "t5"].iter().enumerate() {
        assert_eq!(chats[index]["index"], index + 1);
        assert_eq!(chats[index]["turnId"], *turn);
    }
    assert_eq!(chats[4]["skipped"], "没有子 agent 的报告");
    assert!(chats[4].get("sentAt").is_none());
    assert_eq!(progress["timedOut"], false);

    // A restart sends nothing again.
    canter_eval::steer::steer(SteerOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "chat-1".into(),
        session: 1,
        poll: Duration::from_millis(20),
        timeout: Duration::from_secs(5),
    })
    .await
    .unwrap();
    assert!(fake.next(Duration::from_millis(300)).await.is_none());
}

#[tokio::test]
async fn chat_steering_sends_the_report_line_after_a_wake() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let session_dir = temp.path().join("worktrees/r1/@repos/chat-1");
    std::fs::create_dir_all(&session_dir).unwrap();
    let codex = session_dir.join("codex");
    let codex_base = simple_repo(&codex, "codex base");
    let task_path = temp.path().join("chat.toml");
    write(
        &task_path,
        &format!(
            "id = \"outside-chat\"\nname = \"outside chat\"\nprompt = \"开题\"\n\n[[repos]]\nname = \"codex\"\nbase = \"{codex_base}\"\n\n[[chat]]\nafter = \"turn\"\ntext = '''好的，谢谢'''\n\n[[chat]]\nafter = \"report\"\ntext = '''继续等'''\n"
        ),
    );
    let state = State {
        task: "outside-chat".into(),
        task_path: task_path.display().to_string(),
        round: "r1".into(),
        run: "chat-1".into(),
        repo: String::new(),
        repo_name: String::new(),
        base_commit: String::new(),
        worktree: session_dir.display().to_string(),
        session_file: home
            .session_id_path("r1", "chat-1", 1)
            .display()
            .to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: vec![StateRepo {
            name: "codex".into(),
            repo: codex.display().to_string(),
            base_commit: codex_base,
            worktree: codex.display().to_string(),
        }],
    };
    state.save(&home).unwrap();

    let other = temp.path().join("elsewhere");
    let list = json!({"data": [
        listed("s-other", &other, None),
        listed("s-child", &session_dir, Some("s-main")),
        listed("s-main", &session_dir, None),
    ]});
    let mut fake = fake_server_with(
        temp.path(),
        vec![("/v1/sessions", list)],
        json!({"eventSeq": 1, "turnId": null}),
    )
    .await;
    register_fake(&home, &fake);
    let events_path = home.session_events("s-main");
    let events = vec![
        started_event(1, 1_000, "s-main", "t1", "开题", ""),
        spawn_event(2, 2_000, "s-main", "t1", "c1", "running"),
        turn_done_event(3, 15_000, "s-main", "t1"),
        started_event(4, 20_000, "s-main", "t2", "好的，谢谢", "canter-eval"),
        turn_done_event(5, 23_000, "s-main", "t2"),
    ];
    flush_events(&events_path, &events);

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "chat-1".into(),
                session: 1,
                poll: Duration::from_millis(20),
                timeout: Duration::from_secs(5),
            })
            .await
        }
    });

    // The session is found by the directory between the repositories.
    let (method, path, _) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!((method.as_str(), path.as_str()), ("GET", "/v1/sessions"));

    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["input"][0]["text"],
        "好的，谢谢"
    );

    // No report has woken the session yet: the report line waits.
    assert!(fake.next(Duration::from_millis(300)).await.is_none());

    // A wake turn starts, still busy: nothing is sent during it.
    let mut events = events;
    events.push(started_event(6, 30_000, "s-main", "w1", "", "background"));
    flush_events(&events_path, &events);
    assert!(fake.next(Duration::from_millis(300)).await.is_none());

    // The wake turn ends while the child still runs: now the line goes out.
    events.push(turn_done_event(7, 35_000, "s-main", "w1"));
    flush_events(&events_path, &events);
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["input"][0]["text"],
        "继续等"
    );

    events.push(started_event(
        8,
        40_000,
        "s-main",
        "t4",
        "继续等",
        "canter-eval",
    ));
    events.push(turn_done_event(9, 42_000, "s-main", "t4"));
    flush_events(&events_path, &events);
    // A second report line waits for another wake, and there is none left: the
    // child comes back and steering ends without it.
    events.push(agent_updated_event(10, 50_000, "s-main", "c1", "completed"));
    flush_events(&events_path, &events);
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let progress = chat_progress(&home, "r1", "chat-1");
    let chats = progress["chats"].as_array().unwrap();
    assert_eq!(chats[1]["turnId"], "t4");
    assert!(chats[1].get("skipped").is_none(), "{progress}");
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(home.session_id_path("r1", "chat-1", 1)).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["sessionId"], "s-main");
}

#[tokio::test]
async fn chat_steering_skips_the_report_line_when_every_child_is_back() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(
        temp.path(),
        &base,
        &[("turn", "好的，谢谢"), ("report", "继续等")],
    );
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-chat"}).to_string(),
    );
    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": null})).await;
    register_fake(&home, &fake);
    let events_path = home.session_events("s-chat");
    let mut events = vec![
        started_event(1, 1_000, "s-chat", "t1", "开题", ""),
        spawn_event(2, 2_000, "s-chat", "t1", "c1", "running"),
        turn_done_event(3, 15_000, "s-chat", "t1"),
    ];
    flush_events(&events_path, &events);

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "chat-1".into(),
                session: 1,
                poll: Duration::from_millis(20),
                timeout: Duration::from_secs(5),
            })
            .await
        }
    });
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["input"][0]["text"],
        "好的，谢谢"
    );
    events.push(started_event(
        4,
        20_000,
        "s-chat",
        "t2",
        "好的，谢谢",
        "canter-eval",
    ));
    events.push(turn_done_event(5, 21_000, "s-chat", "t2"));
    events.push(agent_updated_event(6, 30_000, "s-chat", "c1", "completed"));
    flush_events(&events_path, &events);
    events.push(started_event(7, 35_000, "s-chat", "w1", "", "background"));
    events.push(turn_done_event(8, 36_000, "s-chat", "w1"));
    flush_events(&events_path, &events);
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let progress = chat_progress(&home, "r1", "chat-1");
    let chats = progress["chats"].as_array().unwrap();
    assert_eq!(chats[1]["skipped"], "所有子 agent 都回来了", "{progress}");
}

#[tokio::test]
async fn chat_steering_skips_the_report_line_when_children_finish_without_a_wake() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(
        temp.path(),
        &base,
        &[("turn", "好的，谢谢"), ("report", "继续等")],
    );
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-chat"}).to_string(),
    );
    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": null})).await;
    register_fake(&home, &fake);
    let events_path = home.session_events("s-chat");
    let mut events = vec![
        started_event(1, 1_000, "s-chat", "t1", "开题", ""),
        spawn_event(2, 2_000, "s-chat", "t1", "c1", "running"),
        turn_done_event(3, 15_000, "s-chat", "t1"),
    ];
    flush_events(&events_path, &events);

    let handle = tokio::spawn({
        let home = home.clone();
        async move {
            canter_eval::steer::steer(SteerOptions {
                home,
                round: "r1".into(),
                run: "chat-1".into(),
                session: 1,
                poll: Duration::from_millis(20),
                timeout: Duration::from_millis(300),
            })
            .await
        }
    });
    let (_, _, body) = fake.next(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["input"][0]["text"],
        "好的，谢谢"
    );

    // The child comes back and the chat turn handles its report; no separate
    // background turn ever appears, so the report line is skipped, not waited
    // for until the time limit.
    events.push(started_event(
        4,
        20_000,
        "s-chat",
        "t2",
        "好的，谢谢",
        "canter-eval",
    ));
    events.push(agent_updated_event(5, 21_000, "s-chat", "c1", "completed"));
    events.push(turn_done_event(6, 22_000, "s-chat", "t2"));
    flush_events(&events_path, &events);
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let progress = chat_progress(&home, "r1", "chat-1");
    let chats = progress["chats"].as_array().unwrap();
    assert_eq!(chats[1]["skipped"], "所有子 agent 都回来了", "{progress}");
    assert_eq!(progress["timedOut"], false, "{progress}");
}

#[tokio::test]
async fn chat_steering_ignores_a_failed_spawn_placeholder() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(temp.path(), &base, &[("report", "继续等")]);
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-chat"}).to_string(),
    );
    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": null})).await;
    register_fake(&home, &fake);
    let events_path = home.session_events("s-chat");
    // A failed spawn: the tool call completed but returned no agent id.
    let events = vec![
        started_event(1, 1_000, "s-chat", "t1", "开题", ""),
        item_event(
            2,
            2_000,
            "s-chat",
            "t1",
            json!({"type": "collabAgentToolCall", "id": "spawn-failed",
                   "tool": "spawnAgent", "status": "completed", "error": "bad role",
                   "agents": [{"id": "", "name": "", "status": null}]}),
        ),
        turn_done_event(3, 15_000, "s-chat", "t1"),
    ];
    flush_events(&events_path, &events);

    tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home: home.clone(),
            round: "r1".into(),
            run: "chat-1".into(),
            session: 1,
            poll: Duration::from_millis(20),
            timeout: Duration::from_millis(300),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(fake.next(Duration::from_millis(200)).await.is_none());
    let progress = chat_progress(&home, "r1", "chat-1");
    let chats = progress["chats"].as_array().unwrap();
    assert_eq!(chats[0]["skipped"], "没有子 agent 的报告", "{progress}");
    assert_eq!(progress["timedOut"], false, "{progress}");
}

#[tokio::test]
async fn chat_steering_stops_at_the_time_limit() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(temp.path(), &base, &[("turn", "好的，谢谢")]);
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-chat"}).to_string(),
    );
    let mut fake = fake_server(temp.path(), json!({"eventSeq": 1, "turnId": null})).await;
    register_fake(&home, &fake);

    // The session never starts its opening turn: the schedule gives up.
    tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home: home.clone(),
            round: "r1".into(),
            run: "chat-1".into(),
            session: 1,
            poll: Duration::from_millis(20),
            timeout: Duration::from_millis(300),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let progress = chat_progress(&home, "r1", "chat-1");
    assert_eq!(progress["timedOut"], true, "{progress}");
    assert!(
        progress["chats"]
            .as_array()
            .is_none_or(|chats| chats.is_empty())
    );
    assert!(fake.next(Duration::from_millis(200)).await.is_none());
}

#[tokio::test]
async fn chat_steering_stops_at_the_time_limit_before_the_session_appears() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(temp.path(), &base, &[("turn", "好的，谢谢")]);
    chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    // No session id yet, and the eval server lists no session in the run's
    // directory: the whole schedule is still due to stop at its time limit.
    let fake = fake_server(temp.path(), json!({"data": []})).await;
    register_fake(&home, &fake);
    tokio::time::timeout(
        Duration::from_secs(3),
        canter_eval::steer::steer(SteerOptions {
            home: home.clone(),
            round: "r1".into(),
            run: "chat-1".into(),
            session: 1,
            poll: Duration::from_millis(20),
            timeout: Duration::from_millis(300),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let progress = chat_progress(&home, "r1", "chat-1");
    assert_eq!(progress["timedOut"], true, "{progress}");
}

/// The ninth test's final answer, without the wrapper around the code fence.
fn ninth_answer() -> String {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/subagent-answer-9.md"),
    )
    .unwrap();
    let start = text.find("````markdown\n").unwrap() + "````markdown\n".len();
    let end = text.rfind("\n````").unwrap();
    text[start..end].to_string()
}

fn turn_row(index: usize, id: &str, duration: u64, opened: Value, tools: Value) -> Value {
    json!({
        "index": index, "id": id, "startedAt": 1_000 * index as i64,
        "endedAt": 1_000 * index as i64 + duration as i64, "durationMs": duration,
        "openedBy": opened, "tools": tools
    })
}

fn subagent_session_data(reply: &str) -> Value {
    let mut data = json!({
        "sessionId": "s-main",
        "cwd": "/work",
        "turns": [
            turn_row(1, "t1", 15_000, json!({"kind": "prompt"}),
                     json!({"spawn_agent": 2, "shell": 5})),
            turn_row(2, "t2", 3_000, json!({"kind": "steer", "index": 1}),
                     json!({"read": 2, "shell": 1})),
            turn_row(3, "t3", 4_000, json!({"kind": "steer", "index": 2}), json!({})),
            turn_row(4, "t4", 25_000, json!({"kind": "steer", "index": 3}),
                     json!({"read": 1})),
            turn_row(5, "t5", 32_000, json!({"kind": "report"}), json!({})),
        ],
        "finalReply": reply,
        "replies": [reply],
        "subAgents": [
            {"id": "c1", "name": "probe-timeouts", "agentType": "explorer",
             "status": "completed", "startedAt": 2_000, "endedAt": 94_000,
             "durationMs": 92_000, "requestCount": 44, "blockedCommands": 4,
             "blockedRgReplace": 2, "cachedInputTokens": 100,
             "uncachedInputTokens": 200, "outputTokens": 300,
             "costUsd": 0.01, "costUnknown": false},
            {"id": "c2", "name": "probe-paths", "agentType": "explorer",
             "status": "completed", "startedAt": 2_500, "endedAt": 90_000,
             "durationMs": 87_500, "requestCount": 73, "blockedCommands": 0,
             "blockedRgReplace": 0, "cachedInputTokens": 0,
             "uncachedInputTokens": 0, "outputTokens": 0,
             "costUsd": 0.02, "costUnknown": false},
        ],
        "chats": [
            {"index": 1, "sentAt": 20_000, "turnId": "t2"},
            {"index": 5, "sentAt": 200_000, "turnId": "t5"},
        ],
        "timedOut": false
    });
    data["turns"][0]["spawnedAgents"] = json!(2);
    data
}

fn run_check_in(task: &Task, id: &str, cwd: &Path, run_dir: &Path) -> (bool, String) {
    let check = task
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no check {id}"));
    let output = Command::new("sh")
        .arg("-c")
        .arg(&check.command)
        .current_dir(cwd)
        .env("RUN_DIR", run_dir)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn subagent_checks_read_the_session_data() {
    let temp = TempDir::new().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/subagent.toml");
    let task = Task::load(&path).unwrap();
    let data = subagent_session_data(&ninth_answer());
    let data_path = run_dir.join("session-data.json");
    write(&data_path, &serde_json::to_string_pretty(&data).unwrap());

    for id in [
        "spawned-subagents",
        "dispatch-turn-quick",
        "no-wait-in-chat",
        "answer-facts",
    ] {
        let (passed, error) = run_check_in(&task, id, &run_dir, &run_dir);
        assert!(passed, "{id} should pass: {error}");
    }
    let record = |id: &str| {
        let output = Command::new("sh")
            .arg("-c")
            .arg(&task.checks.iter().find(|c| c.id == id).unwrap().command)
            .current_dir(&run_dir)
            .env("RUN_DIR", &run_dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    assert_eq!(record("dispatch-turn-seconds"), "15");
    assert_eq!(record("subagent-requests"), "117");
    assert_eq!(record("subagent-blocked"), "4");
    assert_eq!(record("subagent-rg-replace-blocked"), "2");
    assert_eq!(record("main-reads-and-commands"), "4");
    assert_eq!(record("fifth-chat-sent"), "1");

    // A run that spawned nothing fails the dispatch check.
    let mut quiet = data.clone();
    quiet["turns"][0]["tools"]["spawn_agent"] = json!(0);
    quiet["turns"][0]["spawnedAgents"] = json!(0);
    write(&data_path, &quiet.to_string());
    assert!(!run_check_in(&task, "spawned-subagents", &run_dir, &run_dir).0);

    // A spawn that failed returned no agent id: the attempt counts as a tool
    // call, but the run did not start a sub-agent.
    let mut failed = data.clone();
    failed["turns"][0]["tools"]["spawn_agent"] = json!(1);
    failed["turns"][0]["spawnedAgents"] = json!(0);
    write(&data_path, &failed.to_string());
    assert!(!run_check_in(&task, "spawned-subagents", &run_dir, &run_dir).0);

    // A first turn over 60 seconds fails the quick dispatch check.
    let mut slow = data.clone();
    slow["turns"][0]["durationMs"] = json!(61_000);
    write(&data_path, &slow.to_string());
    assert!(!run_check_in(&task, "dispatch-turn-quick", &run_dir, &run_dir).0);

    // A chat turn that waited fails the no-wait check.
    let mut waiting = data.clone();
    waiting["turns"][1]["tools"]["wait_agent"] = json!(1);
    write(&data_path, &waiting.to_string());
    assert!(!run_check_in(&task, "no-wait-in-chat", &run_dir, &run_dir).0);

    // A run in which no turn was opened by chat cannot pass the no-wait check:
    // an empty set of chat turns must not pass by accident.
    let mut no_chat = data.clone();
    for (index, turn) in no_chat["turns"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        turn["openedBy"] = json!({"kind": "other", "clientName": format!("client-{index}")});
    }
    write(&data_path, &no_chat.to_string());
    assert!(!run_check_in(&task, "no-wait-in-chat", &run_dir, &run_dir).0);

    // Every one of the six facts is required; the real answer has them all.
    let answer = ninth_answer();
    for (label, removed) in [
        (
            "10 seconds",
            answer
                .replace("10 秒", "十几秒")
                .replace("10 000 ms", "若干 ms"),
        ),
        (
            "300 seconds",
            answer.replace("300 s", "几分钟").replace("300s", "很久"),
        ),
        ("exit 124", answer.replace("124", "125")),
        (
            "no default",
            answer
                .replace("没有默认超时", "有默认超时")
                .replace("无默认", "有默认")
                .replace("无超时", "有超时"),
        ),
        (
            "timeout message",
            answer
                .replace("Command timed out after", "Command aborted")
                .replace("command timed out after", "command aborted"),
        ),
        (
            "60 seconds",
            answer
                .replace("60 s", "一分钟")
                .replace("60000ms", "若干ms"),
        ),
    ] {
        let data = subagent_session_data(&removed);
        write(&data_path, &data.to_string());
        let (passed, error) = run_check_in(&task, "answer-facts", &run_dir, &run_dir);
        assert!(!passed, "removing the {label} fact must fail the check");
        assert!(error.contains("missing:"), "{label}: {error}");
    }
}

#[test]
fn answer_facts_reads_earlier_main_session_replies() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    let base = simple_repo(&repo, "base");
    let tasks = chat_task(temp.path(), &base, &[("turn", "哪个代码更好读？")]);
    let state = chat_state(&home, &tasks.join("chat.toml"), &repo, "chat-1");
    let replies = [
        "codex shell 默认 10 秒；codex MCP 默认 300 秒；超时退出码 124",
        "pi bash 没有默认超时；Command timed out after N seconds；pi MCP 默认 60 秒",
        "我觉得 pi 更好读。",
    ];
    let mut events = Vec::new();
    for (index, reply) in replies.iter().enumerate() {
        let seq = index as u64 * 3 + 1;
        let turn = format!("t{index}");
        events.push(started_event(seq, seq as i64, "s-main", &turn, "问题", ""));
        events.push(item_event(
            seq + 1,
            seq as i64 + 1,
            "s-main",
            &turn,
            json!({"type": "agentMessage", "id": format!("a{index}"),
                   "text": reply, "status": "completed"}),
        ));
        events.push(turn_done_event(seq + 2, seq as i64 + 2, "s-main", &turn));
    }
    flush_events(&home.session_events("s-main"), &events);
    let stats = serde_json::from_value(stats_for("s-main", 1, 9, 0, 0.0)).unwrap();
    let data = canter_eval::session_data::build(&home, &state, &stats).unwrap();
    assert_eq!(data.final_reply.as_deref(), Some(replies[2]));
    assert_eq!(data.replies, replies);
    let run_dir = home.run_dir("r1", "chat-1");
    write(
        &run_dir.join("session-data.json"),
        &serde_json::to_string(&data).unwrap(),
    );
    let task = Task::load(&task_dir().join("subagent.toml")).unwrap();
    let (passed, error) = run_check_in(&task, "answer-facts", &run_dir, &run_dir);
    assert!(passed, "facts in earlier replies must count: {error}");
}

#[test]
fn answer_facts_checks_number_boundaries_and_millisecond_forms() {
    let temp = TempDir::new().unwrap();
    let run_dir = temp.path().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/subagent.toml");
    let task = Task::load(&path).unwrap();
    let data_path = run_dir.join("session-data.json");
    let check = |reply: &str| {
        let data = subagent_session_data(reply);
        write(&data_path, &data.to_string());
        run_check_in(&task, "answer-facts", &run_dir, &run_dir)
    };
    let rest = "codex MCP 300 秒；exit 124；pi bash 没有默认超时；\
                Command timed out after N seconds；pi MCP 60 秒";

    // A longer number that ends in the fact's digits is not that fact.
    let (passed, error) = check(&format!("codex shell 110 秒；{rest}"));
    assert!(!passed, "110 秒 is not 10 秒: {error}");
    let (passed, error) = check(
        "codex shell 10 秒；codex MCP 300 秒；exit 1124；\
         pi bash 没有默认超时；Command timed out after N seconds；pi MCP 60 秒",
    );
    assert!(!passed, "1124 is not exit code 124: {error}");

    // Every millisecond form the task lists stands for the fact.
    for written in ["10 000 ms", "10_000", "10000 ms", "10000ms"] {
        let (passed, error) = check(&format!("codex shell {written}；{rest}"));
        assert!(passed, "{written} was not recognised: {error}");
    }
}

#[test]
fn grade_writes_the_session_data_for_chat_tasks() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tasks = chat_task(temp.path(), &base, &[("turn", "好的，谢谢")]);
    let task_path = tasks.join("chat.toml");
    // One check reads what grade gathered.
    let text = std::fs::read_to_string(&task_path).unwrap()
        + "\n[[checks]]\nid = \"has-data\"\ndescription = \"session data\"\ncommand = '''jq -e '.turns[0].tools.spawn_agent == 1 and .subAgents[0].blockedRgReplace == 1 and .finalReply == \"汇总\"' \"$RUN_DIR/session-data.json\"'''\n";
    write(&task_path, &text);
    let state = chat_state(&home, &task_path, &repo, "chat-1");
    write(
        &home.session_id_path("r1", "chat-1", 1),
        &json!({"sessionId": "s-main"}).to_string(),
    );
    let stats = json!({
        "sessionId": "s-main", "eventSeq": 9,
        "overview": {"cwd": repo.display().to_string(), "model": "m",
                     "startedAt": 1_000, "updatedAt": 60_000, "tags": {},
                     "turnCount": 1, "requestCount": 1,
                     "retryCount": 0, "recordedRetryCount": 0},
        "subAgents": [{"id": "c1", "name": "probe", "agentType": "explorer",
                       "status": "completed", "requestCount": 12,
                       "toolCount": 4, "durationMs": 9_000,
                       "usage": {"cachedInputTokens": 1, "uncachedInputTokens": 2,
                                 "outputTokens": 3, "cost": {"usd": 0.01, "unknown": false}}}],
        "requests": [], "compactions": [], "tools": [], "toolCalls": [],
        "approvals": {"count": 0, "data": []}
    });
    let canter = fake_canter(temp.path(), &stats);
    let source = source_config(temp.path());
    write_local_config_canter(
        &home,
        &[("canter-test-ts", &repo)],
        &source,
        &temp.path().join("worktrees"),
        &canter.display().to_string(),
    );
    let _ = state;
    let events = vec![
        started_event(1, 1_000, "s-main", "t1", "开题", ""),
        spawn_event(2, 2_000, "s-main", "t1", "c1", "completed"),
        item_event(
            3,
            3_000,
            "s-main",
            "t1",
            json!({"type": "agentMessage", "id": "a1", "text": "汇总", "status": "completed"}),
        ),
        turn_done_event(4, 16_000, "s-main", "t1"),
    ];
    flush_events(&home.session_events("s-main"), &events);
    let child = vec![item_event(
        1,
        2_000,
        "c1",
        "ct",
        json!({"type": "commandExecution", "id": "x", "command": "rg -rn foo src",
               "cwd": "/w", "status": "blocked", "aggregatedOutput": "",
               "exitCode": null, "durationMs": null}),
    )];
    flush_events(&home.session_events("c1"), &child);

    let mut out = Vec::new();
    let file = canter_eval::grade::grade(&home, "r1", "chat-1", &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("session data:"), "{out}");
    for check in &file.results {
        assert_eq!(
            check.passed,
            Some(true),
            "{}: {}",
            check.id,
            check.output_tail
        );
    }
    assert!(
        home.run_dir("r1", "chat-1")
            .join("session-data.json")
            .is_file()
    );
}

/// A canter executable that answers `stats` with the given JSON.
fn fake_canter(dir: &Path, stats: &Value) -> PathBuf {
    let stats_path = dir.join("stats.json");
    write(&stats_path, &stats.to_string());
    let path = dir.join("fake-canter");
    write(
        &path,
        &format!("#!/bin/sh\ncat '{}'\n", stats_path.display()),
    );
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

#[test]
fn collect_and_report_time_sub_agents_until_the_last_report() {
    let temp = TempDir::new().unwrap();
    let home = Home {
        root: temp.path().join("home"),
    };
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write(&repo.join("base.txt"), "base\n");
    commit(&repo, "base", None);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let state = State {
        task: "long".into(),
        task_path: long_task_path(),
        round: "r1".into(),
        run: "subagent-1".into(),
        repo: repo.display().to_string(),
        repo_name: "canter-test-ts".into(),
        base_commit: base,
        worktree: repo.display().to_string(),
        session_file: home
            .session_id_path("r1", "subagent-1", 1)
            .display()
            .to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
        session_count: 1,
        memory_mode: None,
        repos: Vec::new(),
    };
    state.save(&home).unwrap();
    write(
        &home.session_id_path("r1", "subagent-1", 1),
        &json!({"sessionId": "s-main"}).to_string(),
    );
    canter_eval::paths::write_private(
        &home.run_dir("r1", "subagent-1").join("checks.json"),
        serde_json::to_vec_pretty(&CheckFile {
            ran_at: 0,
            results: vec![CheckResult {
                id: "dispatch".into(),
                description: "spawned".into(),
                record: false,
                passed: Some(true),
                value: None,
                output_tail: String::new(),
            }],
        })
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    let mut stats = stats_for("s-main", 1_000, 400_000, 1_000, 0.05);
    stats["subAgents"] = json!([{
        "id": "c1", "name": "probe", "agentType": "explorer", "status": "closed",
        "requestCount": 44, "toolCount": 9, "durationMs": 92_000,
        "usage": {"cachedInputTokens": 100, "uncachedInputTokens": 200,
                  "outputTokens": 300, "cost": {"usd": 0.01, "unknown": false}}
    }]);
    let stats_path = temp.path().join("stats.json");
    write(&stats_path, &stats.to_string());
    let child = vec![
        item_event(
            1,
            2_000,
            "c1",
            "ct",
            json!({"type": "commandExecution", "id": "x",
            "command": "rg -rn foo src", "cwd": "/w", "status": "blocked",
            "aggregatedOutput": "", "exitCode": null, "durationMs": null}),
        ),
        item_event(
            2,
            3_000,
            "c1",
            "ct",
            json!({"type": "commandExecution", "id": "y",
            "command": "ls", "cwd": "/w", "status": "declined",
            "aggregatedOutput": "Blocked", "exitCode": null, "durationMs": null}),
        ),
        item_event(
            3,
            4_000,
            "c1",
            "ct",
            json!({"type": "commandExecution", "id": "z",
            "command": "rg -n foo src", "cwd": "/w", "status": "completed",
            "aggregatedOutput": "", "exitCode": 0, "durationMs": 1}),
        ),
        turn_done_event(4, 4_000, "c1", "ct"),
        started_event(5, 5_000, "c1", "ct2", "补查", ""),
        turn_done_event(6, 6_000, "c1", "ct2"),
        json!({"seq": 7, "sessionId": "c1", "timestamp": 20_000,
               "type": "session.status", "status": "closed"})
        .to_string(),
    ];
    flush_events(&home.session_events("c1"), &child);
    write(&home.session_events("s-main"), "");

    let data =
        canter_eval::session_data::build(&home, &state, &serde_json::from_value(stats).unwrap())
            .unwrap();
    let data = serde_json::to_value(data).unwrap();
    assert_eq!(data["subAgents"][0]["startedAt"], 2_000);
    assert_eq!(data["subAgents"][0]["endedAt"], 6_000);
    assert_eq!(data["subAgents"][0]["durationMs"], 4_000);

    let result = canter_eval::collect::collect(CollectOptions {
        home: home.clone(),
        round: "r1".into(),
        run: "subagent-1".into(),
        stats_json: Some(stats_path),
        session: None,
    })
    .unwrap();
    assert_eq!(result.sub_agents.len(), 1);
    let agent = &result.sub_agents[0];
    assert_eq!(agent.name.as_deref(), Some("probe"));
    assert_eq!(agent.agent_type, "explorer");
    assert_eq!(agent.status, "closed");
    assert_eq!(agent.request_count, 44);
    assert_eq!(agent.blocked_commands, 2);
    assert_eq!(agent.blocked_rg_replace, 1);
    assert_eq!(agent.duration_ms, Some(4_000));
    assert_eq!(agent.cost_usd, Some(0.01));

    let out = temp.path().join("results/r1.md");
    let path = canter_eval::report::report(&canter_eval::report::ReportOptions {
        home,
        round: "r1".into(),
        against: None,
        out,
    })
    .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("## 子 agent"), "{text}");
    assert!(
        text.contains(
            "| subagent-1 | 1 | 主 6m39s · 子 4s（各会话相加 6m43s） | 48（主 4 · 子 44） | $0.06（主 $0.05 · 子 $0.01） |"
        ),
        "{text}"
    );
    assert!(
        text.contains("| subagent-1 | 1 probe | explorer | 关闭 | 4s | 44 |"),
        "{text}"
    );
}
