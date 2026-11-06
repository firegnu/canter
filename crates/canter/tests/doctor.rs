use serde_json::json;
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

async fn cli(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_canter"))
        .arg("doctor")
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CANTER_HOME", root.join("state"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn fixture(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(root.path().join("home")).unwrap();
    fs::create_dir(root.path().join("state")).unwrap();
    root
}

#[tokio::test]
async fn doctor_falls_back_to_read_only_local_checks_without_starting_a_server() {
    let root = fixture("canter-doctor-cli-");
    let config = json!({
        "base_url":"http://127.0.0.1:9",
        "api_key":"test-only-cli-key",
        "headers":{"X-Test":"test-only-cli-token"},
        "model":"fake",
        "context_window":1048576,
        "sandbox":false
    });
    fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
    fs::write(
        root.path().join("state/server.json"),
        json!({
            "pid": 123,
            "version": "0.0.0 · old",
            "protocolVersion": 99,
            "socketPath": root.path().join("state/missing.sock"),
            "instance": "offline"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.path().join("state/server.log"),
        "INFO ordinary test-only-cli-key\nERROR first failed\nWARN ignored\nERROR authentication failed test-only-cli-key\nERROR authorization failed test-only-cli-token\nERROR model list failed\n",
    )
    .unwrap();
    let config_before = fs::read(root.path().join("state/config.json")).unwrap();
    let registration_before = fs::read(root.path().join("state/server.json")).unwrap();
    let log_before = fs::read(root.path().join("state/server.log")).unwrap();
    let output = cli(root.path()).await;
    let shown = text(&output);
    assert!(!output.status.success(), "{shown}");
    for expected in [
        "fail Server connection",
        "ok config.json",
        "fail Client/server versions",
        "server.log",
        "first failed",
        "model list failed",
        "problem",
    ] {
        assert!(shown.contains(expected), "missing {expected}: {shown}");
    }
    for secret in ["test-only-cli-key", "test-only-cli-token"] {
        assert!(!shown.contains(secret), "secret leaked: {shown}");
    }
    assert!(!shown.contains("ordinary"), "{shown}");
    assert!(!root.path().join("state/server.sock").exists());
    assert_eq!(
        fs::read(root.path().join("state/config.json")).unwrap(),
        config_before
    );
    assert_eq!(
        fs::read(root.path().join("state/server.json")).unwrap(),
        registration_before
    );
    assert_eq!(
        fs::read(root.path().join("state/server.log")).unwrap(),
        log_before
    );
}

#[tokio::test]
async fn doctor_falls_back_locally_when_the_running_server_is_too_old() {
    let root = fixture("canter-doctor-old-server-");
    let socket = root.path().join("state/server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let infos = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let doctors = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info_count = infos.clone();
    let doctor_count = doctors.clone();
    let server = axum::Router::new()
        .route(
            "/v1/info",
            axum::routing::get(move || {
                let info_count = info_count.clone();
                async move {
                    info_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(json!({
                        "serverVersion": canter::VERSION,
                        "protocolVersion": canter_protocol::PROTOCOL_VERSION,
                        "sessions": [],
                        "features": []
                    }))
                }
            }),
        )
        .route(
            "/v1/doctor",
            axum::routing::get(move || {
                let doctor_count = doctor_count.clone();
                async move {
                    doctor_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(json!({}))
                }
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    use std::os::fd::AsRawFd;
    let lock = fs::File::create(root.path().join("state/server.lock")).unwrap();
    assert_eq!(
        unsafe { nix::libc::flock(lock.as_raw_fd(), nix::libc::LOCK_EX) },
        0
    );
    let config = json!({
        "base_url":"http://127.0.0.1:9",
        "api_key":"test-only-old-server-key",
        "model":"fake",
        "context_window":1048576,
        "sandbox":false
    });
    fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
    let registration = json!({
        "pid": std::process::id(),
        "version": canter::VERSION,
        "protocolVersion": canter_protocol::PROTOCOL_VERSION,
        "socketPath": socket,
        "instance": "old-server"
    });
    fs::write(
        root.path().join("state/server.json"),
        registration.to_string(),
    )
    .unwrap();
    let before = fs::read(root.path().join("state/config.json")).unwrap();
    let output = cli(root.path()).await;
    let shown = text(&output);
    assert!(!output.status.success(), "{shown}");
    assert!(shown.contains("fail Server connection"), "{shown}");
    assert!(
        shown.contains("This server is too old for /doctor"),
        "{shown}"
    );
    assert!(shown.contains("ok config.json"), "{shown}");
    assert!(!shown.contains("test-only-old-server-key"), "{shown}");
    assert_eq!(infos.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(doctors.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!server.is_finished());
    assert_eq!(
        fs::read(root.path().join("state/config.json")).unwrap(),
        before
    );
    server.abort();
}

#[tokio::test]
async fn doctor_uses_the_running_server_and_succeeds_when_checks_pass() {
    let root = fixture("canter-doctor-online-");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = requests.clone();
    let provider = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(json!({"data":[]}))
            }
        }),
    );
    let provider_task = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    let config = json!({
        "base_url":base_url,
        "api_key":"test-only-online-key",
        "model":"fake",
        "context_window":1048576,
        "sandbox":false
    });
    fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
    let log = fs::File::create(root.path().join("server.log")).unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_canter"))
        .arg("serve")
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("CANTER_HOME", root.path().join("state"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if canter_server::discover(&root.path().join("state"))
                .await
                .ok()
                .flatten()
                .is_some()
            {
                break;
            }
            assert!(server.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = fs::read(root.path().join("state/config.json")).unwrap();
    let output = cli(root.path()).await;
    let shown = text(&output);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains("ok Provider default"), "{shown}");
    assert!(shown.contains("ok Client/server versions"), "{shown}");
    assert!(shown.ends_with("0 problems\n"), "{shown}");
    assert!(!shown.contains("test-only-online-key"), "{shown}");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(root.path().join("state/config.json")).unwrap(),
        before
    );
    server.kill().await.unwrap();
    let logs = fs::read_to_string(root.path().join("server.log")).unwrap();
    assert!(!logs.contains("test-only-online-key"), "{logs}");
    provider_task.abort();
}
