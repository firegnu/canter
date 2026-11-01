use super::*;
use std::{fs, io::Write, time::Instant};

fn app(home: PathBuf) -> Arc<App> {
    Arc::new(App {
        reload: None,
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        store: Arc::new(Store::new(home.join("sessions")).unwrap()),
        home,
        model: Arc::new(PanicOnce {
            panicked: AtomicBool::new(false),
            reached: Notify::new(),
        }),
        default_model: "fake-model".into(),
        models: vec!["fake-model".into()],
        options: CoreOptions::default(),
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    })
}

#[tokio::test]
async fn info_and_usage_see_created_changed_and_deleted_histories() {
    let root = tempfile::Builder::new()
        .prefix("canter-info-")
        .tempdir_in("/tmp")
        .unwrap();
    let app = app(root.path().join("state"));
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let served = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(served))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    // Independent writers ensure invalidation relies on files, not notifications.
    let store = Store::new(app.home.join("sessions")).unwrap();
    let mut writer = store
        .create("s1", SessionSnapshot::empty("s1").settings, 1)
        .unwrap();
    for step in 0..5 {
        match step {
            2 => {
                drop(
                    store
                        .create("s2", SessionSnapshot::empty("s2").settings, 2)
                        .unwrap(),
                );
            }
            3 => {
                let mut settings = writer.snapshot().settings.clone();
                settings.name = Some("changed".into());
                settings.cwd = "/new-cwd".into();
                writer
                    .append(EventPayload::SessionUpdated { settings }, 3)
                    .unwrap();
                writer
                    .append(
                        EventPayload::SessionStatus {
                            status: SessionStatus::Working,
                            close_reason: None,
                        },
                        4,
                    )
                    .unwrap();
            }
            4 => {
                fs::remove_dir_all(store.session_dir("s2").unwrap()).unwrap();
            }
            _ => {}
        }
        let expected = Store::new(app.home.join("sessions"))
            .unwrap()
            .list()
            .unwrap();
        // usage is the other Store::list caller; deletion must not leave it
        // trying to open a removed session's ledger.
        client
            .get("http://localhost/v1/usage")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let actual: InfoResponse = client
            .get("http://localhost/v1/info")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(actual.sessions, expected);
        let listed: SessionListResponse = client
            .get("http://localhost/v1/sessions")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.data, expected);
    }
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "manual 800 MiB history benchmark; creates and removes temporary data"]
async fn large_history_read_benchmark() {
    let root = tempfile::Builder::new()
        .prefix("canter-perf-")
        .tempdir_in("/tmp")
        .unwrap();
    let app = app(root.path().join("state"));
    let last = generate_history(&app.store);
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let served = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(served))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap();
    for attempt in 0..4 {
        let start = Instant::now();
        let response: InfoResponse = client
            .get("http://localhost/v1/info")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response.sessions.len(), 16);
        eprintln!(
            "info_{attempt}_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    let mut reader = app.store.event_reader("s00").unwrap();
    assert!(reader.read_after(last, last).unwrap().is_empty());
    let path = app.store.session_dir("s00").unwrap().join("events.jsonl");
    for attempt in 1..=3 {
        let seq = last + attempt;
        let mut out = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(out, "{{\"sessionId\":\"s00\",\"seq\":{seq},\"timestamp\":{seq},\"type\":\"warning\",\"message\":\"new\"}}").unwrap();
        let start = Instant::now();
        let events = reader.read_after(seq - 1, seq).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, seq);
        eprintln!(
            "catch_up_{attempt}_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    stop.cancel();
    server.await.unwrap();
}

fn generate_history(store: &Store) -> u64 {
    let payload = "x".repeat(900);
    let mut total = 0;
    let mut last = 0;
    for index in 0..16 {
        let id = format!("s{index:02}");
        let settings = SessionSnapshot::empty(&id).settings;
        drop(store.create(&id, settings, 1).unwrap());
        let path = store.session_dir(&id).unwrap().join("events.jsonl");
        let mut size = fs::metadata(&path).unwrap().len();
        let mut out =
            std::io::BufWriter::new(fs::OpenOptions::new().append(true).open(&path).unwrap());
        let mut seq = 1;
        while size < 50 * 1024 * 1024 {
            seq += 1;
            let line = format!(
                "{{\"sessionId\":\"{id}\",\"seq\":{seq},\"timestamp\":{seq},\"type\":\"warning\",\"message\":\"{payload}\"}}\n"
            );
            out.write_all(line.as_bytes()).unwrap();
            size += line.len() as u64;
        }
        out.flush().unwrap();
        total += size;
        if index == 0 {
            last = seq;
        }
    }
    eprintln!("history_bytes={total}, long_session_events={last}");
    last
}

// A child test process runs the complete production startup and shutdown paths.
#[tokio::test]
async fn history_startup_server_process() {
    if std::env::var_os("CANTER_HISTORY_STARTUP_PROCESS").is_some() {
        crate::serve(false, "history-benchmark").await.unwrap();
    }
}

#[tokio::test]
#[ignore = "manual 800 MiB startup benchmark; run with --release"]
async fn large_history_startup_benchmark() {
    let root = tempfile::Builder::new()
        .prefix("canter-startup-perf-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("state");
    let source = Store::new(home.join("sessions")).unwrap();
    let last = generate_history(&source);
    fs::write(
        home.join("config.json"),
        serde_json::to_vec(&json!({
            "base_url":"http://127.0.0.1:1", "model":"fake-model", "api_key":"test-only",
            "context_window":1048576
        }))
        .unwrap(),
    )
    .unwrap();
    let old = crate::lifecycle::Lease::acquire(home.clone(), false).unwrap();
    let warmed = Store::new(home.join("sessions")).unwrap();
    let start = Instant::now();
    crate::startup::warm_summaries(&warmed);
    let prewarm = start.elapsed();
    eprintln!("prewarm_ms={:.3}", prewarm.as_secs_f64() * 1000.0);
    // The successor keeps this same cache across the daemon lock handoff.
    drop(old);
    let successor = crate::lifecycle::Lease::acquire(home.clone(), false).unwrap();
    let start = Instant::now();
    crate::startup::warm_summaries(&warmed);
    let unchanged = start.elapsed();
    eprintln!(
        "post_lock_unchanged_ms={:.3}",
        unchanged.as_secs_f64() * 1000.0
    );
    append_warning(&source, "s00", last + 1);
    let start = Instant::now();
    crate::startup::warm_summaries(&warmed);
    eprintln!(
        "post_lock_one_changed_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    // Also account for the conservative case where every session changed.
    for index in 0..16 {
        append_warning(
            &source,
            &format!("s{index:02}"),
            last + if index == 0 { 2 } else { 1 },
        );
    }
    let start = Instant::now();
    crate::startup::warm_summaries(&warmed);
    let all_changed = start.elapsed();
    eprintln!(
        "post_lock_all_changed_ms={:.3}",
        all_changed.as_secs_f64() * 1000.0
    );
    assert_eq!(warmed.list().unwrap().len(), 16);
    drop(successor);
    // Use a fresh process/cache. Poll only the registration, so the measured
    // request really is the first /v1/info after production startup.
    let startup = Instant::now();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "http::tests::history_read::history_startup_server_process",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", root.path())
        .env("CANTER_HOME", &home)
        .env("CANTER_HISTORY_STARTUP_PROCESS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let registration = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "server exited during startup"
            );
            if let Some(registration) = crate::lifecycle::read_registration(&home).unwrap() {
                break registration;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    eprintln!(
        "production_startup_ms={:.3}",
        startup.elapsed().as_secs_f64() * 1000.0
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(registration.socket_path)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let start = Instant::now();
    let response: InfoResponse = client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let elapsed = start.elapsed();
    eprintln!("first_info_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(response.sessions.len(), 16);
    assert!(
        elapsed < Duration::from_secs(2),
        "first request must fit discovery's deadline"
    );
    if !cfg!(debug_assertions) {
        // Include the existing default MCP and discovery allowances; keep the
        // ordinary startup's optional two-second probe-lock wait as well.
        assert!(
            prewarm.max(Duration::from_secs(2)) + unchanged + Duration::from_millis(12050)
                < Duration::from_secs(15)
        );
        assert!(
            prewarm.max(Duration::from_millis(85400)) + all_changed + Duration::from_millis(12050)
                < Duration::from_secs(105)
        );
        assert!(
            prewarm.max(Duration::from_secs(90)) + all_changed + Duration::from_millis(12050)
                < Duration::from_secs(105)
        );
    }
}

fn append_warning(store: &Store, id: &str, seq: u64) {
    let mut out = fs::OpenOptions::new()
        .append(true)
        .open(store.session_dir(id).unwrap().join("events.jsonl"))
        .unwrap();
    writeln!(out, "{{\"sessionId\":\"{id}\",\"seq\":{seq},\"timestamp\":{seq},\"type\":\"warning\",\"message\":\"changed\"}}").unwrap();
}
