use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use canter_server::{Connection, ensure_server};
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::json;
use tokio::process::Command;

// Exercise the client API in a separate process with only synthetic environment values.
// This avoids mutating the parallel test harness's environment or inheriting user credentials.
async fn isolated(name: &str) -> bool {
    if std::env::var("CANTER_TEST_CASE").as_deref() == Ok(name) {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("canter-discovery-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(root.path().join("home")).unwrap();
    fs::create_dir(root.path().join("state")).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("CANTER_HOME", root.path().join("state"))
        .env("CANTER_TEST_CASE", name)
        .env("CANTER_TEST_SECRET", "synthetic-client-only-value")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "isolated test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("CANTER_HOME").unwrap())
}

fn configure() {
    fs::write(home().join("config.json"), serde_json::to_vec(&json!({
        "base_url":"http://127.0.0.1:9", "model":"fake-model", "context_window":1048576, "api_key":"test-only-key"
    })).unwrap()).unwrap();
    fs::set_permissions(
        home().join("config.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

struct Daemon(Pid);
impl Daemon {
    fn new(connection: &Connection) -> Self {
        Self(Pid::from_raw(connection.registration.pid as i32))
    }
    async fn stop(self) {
        kill(self.0, Signal::SIGTERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while kill(self.0, None) != Err(Errno::ESRCH) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon exits and is reaped");
        std::mem::forget(self);
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = kill(self.0, Signal::SIGKILL);
    }
}

#[tokio::test]
async fn discovery_recovers_from_a_reused_pid() {
    if isolated("discovery_recovers_from_a_reused_pid").await {
        return;
    }
    configure();
    fs::write(home().join("server.lock"), "").unwrap();
    let socket = home().join("server.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    fs::write(home().join("server.json"), serde_json::to_vec(&json!({
        "pid":std::process::id(), "version":canter_server::VERSION,
        "protocolVersion":canter_protocol::PROTOCOL_VERSION, "socketPath":socket, "instance":"stale-instance"
    })).unwrap()).unwrap();
    let connection = ensure_server(&home(), std::path::Path::new(env!("CARGO_BIN_EXE_canter")))
        .await
        .expect("an unrelated live PID must not prevent starting a daemon");
    let daemon = Daemon::new(&connection);
    assert_ne!(connection.registration.pid, std::process::id());
    assert_ne!(connection.registration.instance, "stale-instance");
    assert!(
        connection
            .client
            .get("http://localhost/v1/info")
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    daemon.stop().await;
}

#[tokio::test]
async fn startup_failure_returns_the_log_error_without_waiting_for_the_deadline() {
    if isolated("startup_failure_returns_the_log_error_without_waiting_for_the_deadline").await {
        return;
    }
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        ensure_server(&home(), std::path::Path::new(env!("CARGO_BIN_EXE_canter"))),
    )
    .await
    .expect("a child that has exited without a lock owner must fail immediately");
    let error = match result {
        Err(error) => error,
        Ok(connection) => {
            Daemon::new(&connection).stop().await;
            panic!("missing config must fail");
        }
    };
    let log = fs::read_to_string(home().join("server.log")).unwrap();
    let last = log.lines().last().unwrap();
    assert!(last.contains("cannot read config.json"));
    assert!(
        error.to_string().contains(last),
        "startup error must contain the last log line"
    );
}

#[tokio::test]
async fn background_daemon_does_not_inherit_client_environment() {
    if isolated("background_daemon_does_not_inherit_client_environment").await {
        return;
    }
    configure();
    let connection = ensure_server(&home(), std::path::Path::new(env!("CARGO_BIN_EXE_canter")))
        .await
        .unwrap();
    let daemon = Daemon::new(&connection);
    // Inspect only this test-owned PID. Its inherited environment is entirely synthetic.
    let observation = Command::new("/bin/ps")
        .args([
            "eww",
            "-p",
            &connection.registration.pid.to_string(),
            "-o",
            "command=",
        ])
        .output()
        .await
        .unwrap();
    daemon.stop().await;
    assert!(observation.status.success());
    let environment = String::from_utf8(observation.stdout).unwrap();
    assert!(
        environment.contains("CANTER_HOME="),
        "ps must report the daemon environment"
    );
    assert!(
        !environment.contains("CANTER_TEST_SECRET="),
        "the daemon must not inherit the client environment"
    );
}

#[tokio::test]
async fn concurrent_clients_start_one_daemon_without_duplicate_start_errors() {
    if isolated("concurrent_clients_start_one_daemon_without_duplicate_start_errors").await {
        return;
    }
    configure();
    let home = home();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_canter"));
    let (a, b, c) = tokio::join!(
        ensure_server(&home, binary),
        ensure_server(&home, binary),
        ensure_server(&home, binary)
    );
    let a = a.unwrap();
    let daemon = Daemon::new(&a);
    assert_eq!(a.registration, b.unwrap().registration);
    assert_eq!(a.registration, c.unwrap().registration);
    daemon.stop().await;
    let log = fs::read_to_string(home.join("server.log")).unwrap();
    assert!(
        !log.contains("a server is already running"),
        "concurrent client discovery must not log misleading startup errors"
    );
}
