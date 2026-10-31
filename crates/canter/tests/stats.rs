use std::ffi::OsString;

#[test]
fn stats_selects_sessions_and_rejects_ambiguous_options() {
    for args in [
        vec!["stats"],
        vec!["stats", "--all", "--json"],
        vec!["stats", "1"],
        vec!["stats", "session-id", "--json"],
        vec!["stats", "--last", "--html"],
    ] {
        let command = canter::parse(args.iter().map(OsString::from)).unwrap();
        assert!(
            format!("{command:?}").starts_with("Stats"),
            "{args:?}: {command:?}"
        );
    }
    for args in [
        vec!["stats", "--all", "1"],
        vec!["stats", "--last", "1"],
        vec!["stats", "--json", "--html", "1"],
        vec!["stats", "--html"],
        vec!["stats", "1", "2"],
        vec!["stats", "--no-such-option"],
    ] {
        assert!(
            canter::parse(args.iter().map(OsString::from)).is_err(),
            "{args:?}"
        );
    }
}

use serde_json::{Value, json};
use std::{
    fs, io::Write, os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration,
};
use tokio::process::Command;

async fn cli(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_canter"))
        .args(args)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CANTER_HOME", root.join("state"))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("STATS_BROWSER_LOG", root.join("opened"))
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}
fn succeeded(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn stats_cli_lists_selects_and_exports_offline_reports_and_rejects_old_servers() {
    let root = tempfile::Builder::new()
        .prefix("canter-stats-")
        .tempdir_in("/tmp")
        .unwrap();
    for directory in ["home", "state/sessions/s1", "state/sessions/older", "bin"] {
        fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    fs::write(root.path().join("state/config.json"), r#"{"base_url":"http://127.0.0.1:9","api_key":"test-only","model":"fake-model","context_window":1048576}"#).unwrap();
    let settings = json!({"cwd":"/work/<example>","model":"fake-model","name":null,"approvalPolicy":"allowAll","tags":{"task":"<script>alert(1)</script>"}});
    let mut events = vec![json!({"type":"session.updated","settings":settings})];
    events.push(json!({"type":"turn.started","turn":{"id":"t1","status":"inProgress","items":[],"input":[{"type":"text","text":"Inspect telemetry safely"}],"clientName":"test","error":null,"usage":null}}));
    for index in 0..3 {
        events.push(json!({"type":"telemetry.modelRequest", "requestId":format!("r{index}"),"turnId":"t1","purpose":"conversation","outcome":"completed","retryCount":index,
            "startedAt":2000 + index * 1000,"durationMs":500,"firstResponseMs":100,
            "settings":{"model":"fake-model","approvalPolicy":"allowAll","planMode":false},
            "context":{"estimated":true,"systemTokens":10,"toolDefinitionTokens":20,"conversationTokens":30+index*10,"toolResultTokens":40},"toolCallIds":[]}));
        if index == 1 {
            events.push(json!({"type":"item.completed","turnId":"t1","item":{"type":"contextCompaction","id":"c1","summary":"<script>local text</script>"}}));
        }
    }
    let mut file = fs::File::create(root.path().join("state/sessions/s1/events.jsonl")).unwrap();
    for (index, mut event) in events.into_iter().enumerate() {
        event["sessionId"] = json!("s1");
        event["seq"] = json!(index + 1);
        event["timestamp"] = json!(1000 + index * 1000);
        writeln!(file, "{event}").unwrap();
    }
    fs::write(
        root.path().join("state/sessions/s1/model-history.jsonl"),
        "",
    )
    .unwrap();
    fs::write(root.path().join("state/sessions/older/events.jsonl"), format!("{}\n", json!({"sessionId":"older","seq":1,"timestamp":1,"type":"session.updated","settings":settings}))).unwrap();
    for program in ["open", "xdg-open"] {
        let path = root.path().join("bin").join(program);
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$STATS_BROWSER_LOG\"\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut server = Command::new(env!("CARGO_BIN_EXE_canter"))
        .arg("serve")
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("CANTER_HOME", root.path().join("state"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(root.path().join("server.log")).unwrap())
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
            assert!(
                server.try_wait().unwrap().is_none(),
                "server exited before readiness: {}",
                fs::read_to_string(root.path().join("server.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let list: Value =
        serde_json::from_str(&succeeded(cli(root.path(), &["stats", "--json"]).await)).unwrap();
    assert_eq!(list["data"][0]["sessionId"], "s1");
    assert_eq!(list["data"][0]["number"], 1);
    assert_eq!(list["data"][0]["firstMessage"], "Inspect telemetry safely");
    assert!(succeeded(cli(root.path(), &["stats"]).await).contains("Inspect telemetry safely"));
    let by_number = succeeded(cli(root.path(), &["stats", "1", "--json"]).await);
    assert_eq!(
        succeeded(cli(root.path(), &["stats", "--last", "--json"]).await),
        by_number
    );
    assert_eq!(
        succeeded(cli(root.path(), &["stats", "s1", "--json"]).await),
        by_number
    );
    let json: Value = serde_json::from_str(&by_number).unwrap();
    assert_eq!(json["overview"]["requestCount"], 3);
    let table = succeeded(cli(root.path(), &["stats", "--last"]).await);
    for text in [
        "Requests",
        "Compaction",
        "Tools",
        "Approvals",
        "unknown",
        "No plan before compaction",
    ] {
        assert!(table.contains(text), "missing {text}: {table}");
    }
    let html_path = succeeded(cli(root.path(), &["stats", "--last", "--html"]).await);
    let path = Path::new(html_path.trim());
    assert!(path.starts_with(root.path().join("state/stats")));
    let html = fs::read_to_string(path).unwrap();
    for text in [
        "<svg",
        "<polygon",
        "Compaction",
        "Requests",
        "Tools",
        "Approvals",
        "&lt;script&gt;",
    ] {
        assert!(html.contains(text), "missing {text}");
    }
    assert!(!html.contains("<script"));
    assert!(!html.contains("src=\"http"));
    assert!(!html.contains("href=\"http"));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_to_string(root.path().join("opened"))
            .unwrap()
            .trim(),
        path.to_str().unwrap()
    );
    server.kill().await.unwrap();
    server.wait().await.unwrap();

    // Replace only this test's stopped server with an old protocol peer.
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let router = axum::Router::new().route("/v1/info", axum::routing::get(|| async {
        axum::Json(json!({"serverVersion":canter_server::VERSION,"protocolVersion":1,"sessions":[]}))
    })).fallback(move || { observed.fetch_add(1, Ordering::SeqCst); async { axum::http::StatusCode::NOT_FOUND } });
    let socket = root.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("state/server.lock"))
        .unwrap();
    lock.lock().unwrap();
    fs::write(
        root.path().join("state/server.json"),
        serde_json::to_vec(&canter_server::Registration {
            pid: std::process::id(),
            version: canter_server::VERSION.into(),
            protocol_version: 1,
            socket_path: socket,
            instance: "test-old".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let old_server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    for args in [vec!["stats"], vec!["stats", "s1", "--json"]] {
        let output = cli(root.path(), &args).await;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("canter serve --replace"));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "unsupported stats must not be requested"
    );
    stop.send(()).unwrap();
    old_server.await.unwrap();
}
