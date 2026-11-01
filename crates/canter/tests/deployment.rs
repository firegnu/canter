use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture {
    root: tempfile::TempDir,
    binary: PathBuf,
    state: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-deploy-")
            .tempdir_in("/tmp")
            .unwrap();
        let binary = root.path().join("bin/canter");
        let state = root.path().join("state");
        let repo = root.path().join("repo with spaces");
        for path in [
            binary.parent().unwrap(),
            &state,
            &root.path().join("home"),
            &repo.join("crates/canter"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::copy(env!("CARGO_BIN_EXE_canter"), &binary).unwrap();
        fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(
            repo.join("crates/canter/Cargo.toml"),
            "[package]\nname = 'canter'\n",
        )
        .unwrap();
        fs::write(state.join("config.json"), serde_json::json!({"base_url":"http://127.0.0.1:9", "model":"fake", "api_key":"test-only", "context_window":1048576}).to_string()).unwrap();
        let canonical = fs::canonicalize(&repo).unwrap();
        fs::write(root.path().join("metadata.json"), serde_json::json!({
            "workspace_root":canonical,
            "packages":[{"name":"canter", "manifest_path":canonical.join("crates/canter/Cargo.toml"), "targets":[{"name":"canter","kind":["bin"]}]}]
        }).to_string()).unwrap();
        let cargo = root.path().join("bin/cargo");
        fs::write(
            &cargo,
            r#"#!/bin/sh
case "$1" in
metadata) /bin/cat "$FIXTURE_ROOT/metadata.json" ;;
install)
    pwd > "$FIXTURE_ROOT/build-cwd"
    [ ! -e "$FIXTURE_ROOT/fail-build" ] || exit 42
    while [ "$#" -gt 0 ]; do
        if [ "$1" = --root ]; then shift; dest="$1"; fi
        shift
    done
    /bin/mkdir -p "$dest/bin"
    /bin/cp "$FIXTURE_BINARY" "$dest/bin/canter"
    ;;
*) exit 43 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(cargo, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            root,
            binary,
            state,
            repo,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("CANTER_HOME", &self.state)
            .env("PATH", self.root.path().join("bin"))
            .env("FIXTURE_ROOT", self.root.path())
            .env("FIXTURE_BINARY", env!("CARGO_BIN_EXE_canter"))
            .current_dir(self.root.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn backups(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(self.state.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("canter-")
            })
            .collect();
        files.sort();
        files
    }
}

#[test]
fn upgrade_keeps_five_backups_and_rollback_can_switch_back_again() {
    let test = Fixture::new();
    fs::write(test.state.join("canter-manual-backup"), "keep").unwrap();
    let mut previous = None;
    for index in 0..8 {
        let output = if index < 6 {
            test.run(&["upgrade", test.repo.to_str().unwrap(), "--now"])
        } else {
            test.run(&["rollback", "--now"])
        };
        assert!(output.status.success(), "{output:?}");
        let registration = fs::read(test.state.join("server.json")).unwrap();
        assert_ne!(
            Some(&registration),
            previous.as_ref(),
            "each operation replaces the daemon"
        );
        previous = Some(registration);
        assert_eq!(test.backups().len(), (index + 1).min(5));
        assert_eq!(
            fs::read_to_string(test.state.join("canter-manual-backup")).unwrap(),
            "keep"
        );
    }
}

#[test]
fn rollback_chooses_latest_backup_and_saves_current_version() {
    let test = Fixture::new();
    let backups = test.state.join("backups");
    fs::create_dir(&backups).unwrap();
    for (sequence, commit) in [(1, "aaaaaaa"), (2, "bbbbbbb")] {
        let path = backups.join(format!("canter-{sequence:039}-{commit}"));
        // Only --version is replaced; serving still uses the real isolated daemon.
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'canter 0.0.0 · {commit}'; else exec '{}' \"$@\"; fi\n",
            env!("CARGO_BIN_EXE_canter")
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = test.run(&["rollback", "--now"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("New: canter 0.0.0 · bbbbbbb"),
        "{output:?}"
    );
    assert_eq!(
        String::from_utf8(test.run(&["--version"]).stdout).unwrap(),
        "canter 0.0.0 · bbbbbbb\n"
    );
    assert_eq!(
        fs::read(test.backups().last().unwrap()).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_canter")).unwrap()
    );
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // This private state directory belongs solely to this test's child daemon.
        if let Ok(bytes) = fs::read(self.state.join("server.json"))
            && let Ok(registration) = serde_json::from_slice::<canter_server::Registration>(&bytes)
        {
            if registration.instance == "fixture-server" {
                return;
            }
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(registration.pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
            for _ in 0..500 {
                if !self.state.join("server.json").exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

struct FakeServer {
    state: PathBuf,
    lock: Option<fs::File>,
    task: tokio::task::JoinHandle<()>,
    turns: Arc<Mutex<Vec<serde_json::Value>>>,
    queries: Arc<AtomicUsize>,
}

impl FakeServer {
    async fn new(test: &Fixture, supported: bool) -> Self {
        let socket = test.state.join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let lock = fs::File::create(test.state.join("server.lock")).unwrap();
        lock.lock().unwrap();
        fs::write(
            test.state.join("server.json"),
            serde_json::to_vec(&canter_server::Registration {
                pid: std::process::id(),
                version: canter_server::VERSION.into(),
                protocol_version: canter_protocol::PROTOCOL_VERSION,
                socket_path: socket,
                instance: "fixture-server".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let info = serde_json::json!({"serverVersion":canter_server::VERSION,"protocolVersion":canter_protocol::PROTOCOL_VERSION,"sessions":[],"features":if supported { Some(vec!["runningTurns"]) } else { None }});
        let turns = Arc::new(Mutex::new(vec![
            serde_json::json!({"id":"first","name":"First task","turnId":"turn-one","activity":"Waiting for an answer"}),
        ]));
        let queries = Arc::new(AtomicUsize::new(0));
        let observed = queries.clone();
        let current = turns.clone();
        let router = axum::Router::new()
            .route(
                "/v1/info",
                axum::routing::get(move || {
                    let info = info.clone();
                    async move { axum::Json(info) }
                }),
            )
            .route(
                "/v1/runningTurns",
                axum::routing::get(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                    let data = current.lock().unwrap().clone();
                    async move { axum::Json(serde_json::json!({"data":data})) }
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            state: test.state.clone(),
            lock: Some(lock),
            task,
            turns,
            queries,
        }
    }

    async fn stop(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
        for name in ["server.json", "server.sock"] {
            let _ = fs::remove_file(self.state.join(name));
        }
        self.lock.take();
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
        if self.lock.is_some() {
            for name in ["server.json", "server.sock"] {
                let _ = fs::remove_file(self.state.join(name));
            }
        }
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the isolated command must make progress");
}

fn start_upgrade(test: &Fixture, now: bool) -> tokio::process::Child {
    let mut command = tokio::process::Command::from(test.command());
    command.arg("upgrade").arg(&test.repo);
    if now {
        command.arg("--now");
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn output(child: tokio::process::Child) -> Output {
    tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn upgrade_waits_for_existing_and_new_turns_before_replacing() {
    let test = Fixture::new();
    let mut server = FakeServer::new(&test, true).await;
    let child = start_upgrade(&test, false);
    until(|| server.queries.load(Ordering::SeqCst) > 0 || test.state.join("replace.json").exists())
        .await;
    if test.state.join("replace.json").exists() {
        server.stop().await;
        let result = output(child).await;
        panic!("replacement began without waiting for running turns: {result:?}");
    }
    let count = server.queries.load(Ordering::SeqCst);
    *server.turns.lock().unwrap() = vec![
        serde_json::json!({"id":"second","name":"New task","turnId":"turn-two","activity":"Waiting for approval: write"}),
    ];
    until(|| server.queries.load(Ordering::SeqCst) >= count + 2).await;
    assert!(!test.state.join("replace.json").exists());
    server.turns.lock().unwrap().clear();
    until(|| test.state.join("replace.json").exists()).await;
    server.stop().await;
    let result = output(child).await;
    assert!(result.status.success(), "{result:?}");
    let printed = String::from_utf8(result.stdout).unwrap();
    for text in [
        "First task",
        "Waiting for an answer",
        "New task",
        "Waiting for approval: write",
    ] {
        assert!(printed.contains(text), "{printed}");
    }
}

#[tokio::test]
async fn ctrl_c_keeps_installed_program_and_running_server() {
    let test = Fixture::new();
    let mut server = FakeServer::new(&test, true).await;
    let child = start_upgrade(&test, false);
    until(|| server.queries.load(Ordering::SeqCst) > 0 || test.state.join("replace.json").exists())
        .await;
    if test.state.join("replace.json").exists() {
        server.stop().await;
        let result = output(child).await;
        panic!("no cancellable wait before replacement: {result:?}");
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let result = output(child).await;
    assert!(!result.status.success());
    let message = String::from_utf8(result.stderr).unwrap();
    for text in ["installed", "not been replaced", "canter serve --replace"] {
        assert!(message.contains(text), "{message}");
    }
    assert!(!test.state.join("replace.json").exists());
    assert_eq!(test.backups().len(), 1);
    assert_eq!(
        fs::read(&test.binary).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_canter")).unwrap()
    );
    assert_eq!(
        serde_json::from_slice::<canter_server::Registration>(
            &fs::read(test.state.join("server.json")).unwrap()
        )
        .unwrap()
        .instance,
        "fixture-server"
    );
    let count = server.queries.load(Ordering::SeqCst);
    let mut rollback = tokio::process::Command::from(test.command());
    let rollback = rollback
        .arg("rollback")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    until(|| server.queries.load(Ordering::SeqCst) > count).await;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(rollback.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let result = output(rollback).await;
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("not been replaced"),
        "{result:?}"
    );
    assert!(!test.state.join("replace.json").exists());
    server.stop().await;
}

#[tokio::test]
async fn legacy_server_requires_now_after_install_and_now_skips_the_query() {
    let test = Fixture::new();
    let mut server = FakeServer::new(&test, false).await;
    let mut child = start_upgrade(&test, false);
    until(|| child.try_wait().unwrap().is_some() || test.state.join("replace.json").exists()).await;
    if test.state.join("replace.json").exists() {
        server.stop().await;
        let result = output(child).await;
        panic!("legacy server was replaced without --now: {result:?}");
    }
    let result = output(child).await;
    assert!(!result.status.success());
    let message = String::from_utf8(result.stderr).unwrap();
    for text in [
        "cannot report running turns",
        "--now",
        "installed",
        "not been replaced",
    ] {
        assert!(message.contains(text), "{message}");
    }
    let mut rollback = tokio::process::Command::from(test.command());
    let result = rollback
        .arg("rollback")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("cannot report running turns"),
        "{result:?}"
    );
    assert!(!test.state.join("replace.json").exists());
    let child = start_upgrade(&test, true);
    until(|| test.state.join("replace.json").exists()).await;
    assert_eq!(server.queries.load(Ordering::SeqCst), 0);
    server.stop().await;
    let result = output(child).await;
    assert!(result.status.success(), "{result:?}");
}

#[test]
fn upgrade_without_directory_uses_the_repository_recorded_at_build_time() {
    let test = Fixture::new();
    let source = fs::canonicalize(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
    )
    .unwrap();
    fs::write(test.root.path().join("metadata.json"), serde_json::json!({
        "workspace_root":source,
        "packages":[{"name":"canter", "manifest_path":source.join("crates/canter/Cargo.toml"), "targets":[{"name":"canter","kind":["bin"]}]}]
    }).to_string()).unwrap();
    let output = test.run(&["upgrade", "--now"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(test.root.path().join("build-cwd"))
            .unwrap()
            .trim(),
        source.to_str().unwrap()
    );
}

#[test]
fn upgrade_builds_before_installing_and_preserves_program_and_server_on_failure() {
    let test = Fixture::new();
    let original = fs::read(&test.binary).unwrap();
    let invalid = test.run(&["upgrade", test.root.path().to_str().unwrap(), "--now"]);
    assert!(!invalid.status.success());
    assert!(!test.root.path().join("build-cwd").exists());
    fs::write(test.root.path().join("fail-build"), "").unwrap();
    let failed = test.run(&["upgrade", test.repo.to_str().unwrap(), "--now"]);
    assert!(!failed.status.success());
    assert!(
        test.root.path().join("build-cwd").exists(),
        "the build must actually be attempted: {failed:?}"
    );
    assert_eq!(fs::read(&test.binary).unwrap(), original);
    assert!(!test.state.join("backups").exists());
    assert!(!test.state.join("server.json").exists());
    fs::remove_file(test.root.path().join("fail-build")).unwrap();
    let upgraded = test.run(&["upgrade", test.repo.to_str().unwrap(), "--now"]);
    assert!(upgraded.status.success(), "{upgraded:?}");
    let output = String::from_utf8(upgraded.stdout).unwrap();
    assert!(
        output.contains(&format!("Old: canter {}", canter::VERSION)),
        "{output}"
    );
    assert!(
        output.contains(&format!("New: canter {}", canter::VERSION)),
        "{output}"
    );
    let backups = test.backups();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    assert!(
        backups[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains(canter::VERSION.split(" · ").last().unwrap())
    );
    let registration = fs::read(test.state.join("server.json")).unwrap();
    fs::write(test.root.path().join("fail-build"), "").unwrap();
    assert!(
        !test
            .run(&["upgrade", test.repo.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(
        fs::read(test.state.join("server.json")).unwrap(),
        registration
    );
    assert_eq!(test.backups(), backups);
}

#[test]
fn rollback_without_backups_explains_and_changes_nothing() {
    let test = Fixture::new();
    let output = test.run(&["rollback"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No backups"),
        "{output:?}"
    );
    assert!(!test.state.join("server.json").exists());
    assert_eq!(
        fs::read(&test.binary).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_canter")).unwrap()
    );
    let help = test.run(&["--help"]);
    let help = String::from_utf8(help.stdout).unwrap();
    for command in [
        "service install",
        "service uninstall",
        "upgrade",
        "rollback",
        "--now",
    ] {
        assert!(help.contains(command), "missing {command}");
    }
    for args in [
        vec!["rollback", "extra"],
        vec!["upgrade", "--unknown"],
        vec!["upgrade", "one", "two"],
    ] {
        assert!(!test.run(&args).status.success());
    }
}
