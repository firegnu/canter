//! Local tests: temporary git repositories, a fake unix-socket server, and a
//! fake stats report. No real model, no `~/.canter`, no real server.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use canter_eval::{
    collect::{CollectOptions, Metrics, RunResult},
    grade::{CheckFile, CheckResult},
    paths::{Home, State},
    prepare::PrepareOptions,
    steer::SteerOptions,
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
        session_file: home
            .run_dir(round, run)
            .join("session.json")
            .display()
            .to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
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
        task: "long".into(),
        round: "r1".into(),
        run: "long-1".into(),
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
        task: "long".into(),
        round: "r1".into(),
        run: "long-1".into(),
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
        session_file: home.root.join("session.json").display().to_string(),
        model: "m".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
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
        poll: Duration::from_millis(50),
    })
    .await
    .unwrap();
    assert!(fake.next(Duration::from_millis(300)).await.is_none());
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
        session_file: home.root.join("session.json").display().to_string(),
        model: "m".into(),
        auto_compact_tokens: None,
        created_at: 0,
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
        session_file: home.root.join("session.json").display().to_string(),
        model: "opencode-go/deepseek-v4.1-flash".into(),
        auto_compact_tokens: Some(60_000),
        created_at: 0,
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
            compactions: vec![canter_eval::collect::CompactionRow {
                trigger: Some("auto".into()),
                before_tokens: Some(60_000),
                after_tokens: Some(12_000),
                succeeded: Some(true),
                reread_files: 2,
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
            tool_failures: 3,
            tool_failures_by_name: Default::default(),
            guard_reminders: 1,
            blocked_commands: 1,
            event_parse_errors: 0,
        },
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
    assert!(text.contains("readme-in-late-commits=4"));
    assert!(text.contains("## 通过次数"));
    assert!(text.contains("| old | long | 0/1 | 1.0/2 |"));
    assert!(text.contains("| new | long | 1/1 | 2.0/2 |"));
    assert!(text.contains("## 压缩"));
    assert!(text.contains("60k → 12k（成功）"));
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
    let socket = dir.join("fake.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let reply_body = response.to_string();
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
