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
    paths::{Home, MemoryMode, State},
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
            "scripts": {"test": "node --test tests/*.test.js"},
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
    assert_eq!(task.repo, "canter-test-rust");
    assert_eq!(task.base, "e9ee31cd4e4a0bb671374313cb9699354affa1b1");
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
    assert_eq!(finished.head, head);
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
