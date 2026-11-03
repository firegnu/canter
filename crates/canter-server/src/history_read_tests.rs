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
        session_locks: Default::default(),
        registry_changed: Default::default(),
        managed: Default::default(),
        #[cfg(test)]
        pauses: Default::default(),
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

pub(super) fn write_round(out: &mut impl Write, id: &str, seq: &mut u64, number: u64) -> u64 {
    let mut bytes = 0;
    let mut line = Vec::new();
    let mut emit = |payload| {
        *seq += 1;
        line.clear();
        serde_json::to_writer(
            &mut line,
            &Event {
                session_id: id.into(),
                seq: *seq,
                timestamp: *seq as i64,
                payload,
            },
        )
        .unwrap();
        line.push(b'\n');
        out.write_all(&line).unwrap();
        bytes += line.len() as u64;
    };
    let mut turn = Turn {
        id: format!("t{number}"),
        review: None,
        items: vec![],
        status: TurnStatus::InProgress,
        input: vec![],
        client_name: "benchmark".into(),
        error: None,
        usage: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    emit(EventPayload::TurnStarted {
        turn: turn.clone(),
        input_id: None,
    });
    let text = "进度 \"结果\"：中文与换行\n下一行。\n";
    let output = "stdout: \"结果\" ok\n下一行\n";
    for item in 0..6 {
        let message_id = format!("a{item}");
        emit(EventPayload::ItemStarted {
            turn_id: turn.id.clone(),
            item: Item::AgentMessage {
                id: message_id.clone(),
                text: String::new(),
                status: Some(ItemStatus::InProgress),
            },
        });
        for _ in 0..40 {
            emit(EventPayload::ItemDelta {
                turn_id: turn.id.clone(),
                item_id: message_id.clone(),
                delta: ItemDelta::AgentMessage { delta: text.into() },
            });
        }
        emit(EventPayload::ItemCompleted {
            turn_id: turn.id.clone(),
            item: Item::AgentMessage {
                id: message_id,
                text: text.repeat(40),
                status: Some(ItemStatus::Completed),
            },
        });
        let command_id = format!("c{item}");
        let mut command = Item::CommandExecution {
            id: command_id.clone(),
            source: ExecCommandSource::Agent,
            command: "printf '\"结果\"\\n'".into(),
            cwd: "/benchmark".into(),
            status: ItemStatus::InProgress,
            aggregated_output: String::new(),
            exit_code: None,
            duration_ms: None,
        };
        emit(EventPayload::ItemStarted {
            turn_id: turn.id.clone(),
            item: command.clone(),
        });
        for _ in 0..10 {
            emit(EventPayload::ItemDelta {
                turn_id: turn.id.clone(),
                item_id: command_id.clone(),
                delta: ItemDelta::CommandExecution {
                    delta: output.into(),
                },
            });
        }
        if let Item::CommandExecution {
            status,
            aggregated_output,
            exit_code,
            duration_ms,
            ..
        } = &mut command
        {
            *status = ItemStatus::Completed;
            *aggregated_output = output.repeat(10);
            *exit_code = Some(0);
            *duration_ms = Some(1);
        }
        emit(EventPayload::ItemCompleted {
            turn_id: turn.id.clone(),
            item: command,
        });
    }
    turn.status = TurnStatus::Completed;
    emit(EventPayload::TurnCompleted { turn });
    bytes
}

fn generate_history(store: &Store) -> u64 {
    let mut total = 0;
    let mut last = 0;
    let mut rounds = 0;
    for index in 0..16 {
        let id = format!("s{index:02}");
        drop(
            store
                .create(&id, SessionSnapshot::empty(&id).settings, 1)
                .unwrap(),
        );
        let path = store.session_dir(&id).unwrap().join("events.jsonl");
        let mut size = fs::metadata(&path).unwrap().len();
        let mut out =
            std::io::BufWriter::new(fs::OpenOptions::new().append(true).open(&path).unwrap());
        let mut seq = 1;
        let mut round = 0;
        while size < 50 * 1024 * 1024 {
            round += 1;
            size += write_round(&mut out, &id, &mut seq, round);
        }
        out.flush().unwrap();
        total += size;
        if index == 0 {
            last = seq;
            rounds = round;
        }
    }
    eprintln!("history_bytes={total}, long_session_events={last}, turns_per_session={rounds}");
    last
}

fn append_round(store: &Store, id: &str, after: u64) -> u64 {
    let mut out = std::io::BufWriter::new(
        fs::OpenOptions::new()
            .append(true)
            .open(store.session_dir(id).unwrap().join("events.jsonl"))
            .unwrap(),
    );
    let mut seq = after;
    write_round(&mut out, id, &mut seq, after);
    out.flush().unwrap();
    seq
}

#[test]
fn benchmark_rounds_replay_complete_text_and_commands() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path()).unwrap();
    drop(
        store
            .create("s1", SessionSnapshot::empty("s1").settings, 1)
            .unwrap(),
    );
    let seq = append_round(&store, "s1", 1);
    let last = append_round(&store, "s1", seq);
    let writer = store.open("s1").unwrap();
    let snapshot = writer.snapshot();
    assert_eq!(snapshot.last_event_seq, last);
    assert_eq!(snapshot.turns.len(), 2);
    for turn in &snapshot.turns {
        assert_eq!(turn.status, TurnStatus::Completed);
        assert_eq!(turn.items.len(), 12);
        assert!(
            matches!(&turn.items[0], Item::AgentMessage { text, .. } if text.contains("中文") && text.matches('\n').count() == 80)
        );
        assert!(
            matches!(&turn.items[1], Item::CommandExecution { aggregated_output, exit_code: Some(0), .. } if aggregated_output.matches('\n').count() == 20 && aggregated_output.contains('"'))
        );
    }
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
            "context_window":1048576,
            "mcp_servers":{"slow":{"transport":"stdio","command":"/bin/sleep","args":["30"]}}
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
    let after_one = append_round(&source, "s00", last);
    let start = Instant::now();
    crate::startup::warm_summaries(&warmed);
    eprintln!(
        "post_lock_one_changed_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    // Also account for the conservative case where every session changed.
    for index in 0..16 {
        append_round(
            &source,
            &format!("s{index:02}"),
            if index == 0 { after_one } else { last },
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
    .await;
    if registration.is_err() {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        panic!("startup with turn history and default-timeout MCP exceeded 15 seconds");
    }
    let registration = registration.unwrap();
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
        let mcp = Duration::from_secs(10);
        let discovery = Duration::from_millis(2050);
        // Ordinary startup takes the lock first; its first replay overlaps MCP.
        assert!(Duration::from_secs(2) + prewarm.max(mcp) + discovery < Duration::from_secs(15));
        // A short replacement wait can leave part of the initial replay still
        // running. That remainder and the refresh both overlap MCP after locking.
        for lock_wait in [
            Duration::ZERO,
            Duration::from_millis(85400),
            Duration::from_secs(90),
        ] {
            let history = prewarm.saturating_sub(lock_wait) + all_changed;
            assert!(lock_wait + history.max(mcp) + discovery < Duration::from_secs(105));
        }
    }
}
