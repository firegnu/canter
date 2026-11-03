use super::*;

// Per-App gates pause real operations at their I/O boundaries. They never
// replace the operation or share state between parallel tests.
#[derive(Clone, Default)]
pub(crate) struct Pauses(Arc<std::sync::Mutex<Option<Arc<Pause>>>>);

struct Pause {
    id: String,
    point: &'static str,
    reached: Notify,
    release: CancellationToken,
}

pub(super) struct Block(Arc<Pause>);

impl Drop for Block {
    fn drop(&mut self) {
        self.0.release.cancel();
    }
}

impl Block {
    pub(super) async fn reached(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.0.reached.notified())
            .await
            .expect("operation did not reach the I/O boundary");
    }
}

impl Pauses {
    pub(super) fn block(&self, id: &str, point: &'static str) -> Block {
        let pause = Arc::new(Pause {
            id: id.into(),
            point,
            reached: Notify::new(),
            release: CancellationToken::new(),
        });
        *self.0.lock().unwrap() = Some(pause.clone());
        Block(pause)
    }

    pub(crate) async fn hit(&self, id: &str, point: &'static str) {
        let pause = {
            let mut current = self.0.lock().unwrap();
            if current
                .as_ref()
                .is_some_and(|p| (p.id == id || p.id == "*") && p.point == point)
            {
                current.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.cancelled().await;
        }
    }
}

#[tokio::test]
async fn slow_disk_snapshot_does_not_block_another_session() {
    let test = Harness::new().await;
    let slow = test.create().await;
    let other = test.create().await;
    test.client
        .delete(&slow)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let block = test
        .app
        .pauses
        .block(slow.rsplit('/').next().unwrap(), "snapshot");
    let request = test.client.get(&slow);
    let reading = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    let result =
        tokio::time::timeout(Duration::from_millis(300), test.client.get(&other).send()).await;
    drop(block);
    reading.await.unwrap().error_for_status().unwrap();
    test.stop().await;
    assert!(
        result.is_ok(),
        "another session's snapshot waited for the slow disk read"
    );
    result.unwrap().unwrap().error_for_status().unwrap();
}

async fn other_operations(test: &Harness, url: &str, dormant: &str) {
    let id = url.rsplit('/').next().unwrap();
    test.read(url).await;
    test.patch(url, json!({"name":"still responsive"})).await;
    test.client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"hello"}],"clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let snapshot = loop {
        let snapshot = test.read(url).await.session;
        if !snapshot.pending_requests.is_empty() {
            break snapshot;
        }
        tokio::task::yield_now().await;
    };
    test.client
        .post(format!(
            "{url}/requests/{}",
            snapshot.pending_requests[0].id
        ))
        .json(&json!({"decision":"accept","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/interrupt"))
        .json(&json!({"turnId":snapshot.turns.last().unwrap().id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let reader = test
        .client
        .get(format!("http://localhost/v1/events?session={id}&after=0"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for endpoint in [
        format!("{url}/usage"),
        "http://localhost/v1/sessions".into(),
    ] {
        test.client
            .get(endpoint)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    test.client
        .post(format!("{dormant}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.create().await;
    // The exact enumeration used by reload must also stay available.
    assert!(!test.app.opened_sessions().await.is_empty());
    drop(reader);
    test.client
        .delete(dormant)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

#[tokio::test]
async fn slow_operations_do_not_block_other_sessions() {
    let mut blocked = Vec::new();
    for (operation, point, closed) in [
        ("read", "snapshot", true),
        ("resume", "snapshot", true),
        ("resume", "trust", true),
        ("resume", "open", true),
        ("resume", "configure", true),
        ("resume", "trust", false),
        ("create", "create", false),
        ("create", "configure", false),
        ("lookup", "snapshot", true),
        ("usage", "events", true),
        ("events_new", "events", true),
        ("events_new", "subscribe", false),
        ("events_recv", "events", true),
        ("events_recv", "subscribe", false),
        ("trash", "snapshot", true),
        ("trash", "trash_prepare", false),
        ("trash", "trash_close", false),
        ("trash", "trash_move", false),
    ] {
        let test = Harness::with_model(Arc::new(PausedModel(Some(canter_model::ToolCall {
            id: "write".into(),
            name: "write".into(),
            arguments: json!({"path":"file.txt","content":"changed"}).to_string(),
        }))))
        .await;
        let slow = test.create().await;
        let other = test.create().await;
        let dormant = test.create().await;
        test.client
            .delete(&dormant)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let id = slow.rsplit('/').next().unwrap().to_owned();
        if closed || operation == "events_recv" {
            test.client
                .delete(&slow)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let reader = if operation == "events_recv" {
            let cursor = test.read(&slow).await.event_seq;
            let reader = EventReader::new(test.app.clone(), id.clone(), cursor, false)
                .await
                .map_err(|e| e.0)
                .unwrap();
            if !closed {
                test.client
                    .post(format!("{slow}/resume"))
                    .json(&json!({"env":{}}))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
            }
            Some(reader)
        } else {
            None
        };
        let block = test
            .app
            .pauses
            .block(if operation == "create" { "*" } else { &id }, point);
        let request = match operation {
            "read" => test.client.get(&slow),
            "resume" => test.client.post(format!("{slow}/resume")).json(&json!({"env":{}})),
            "create" => test.client.post("http://localhost/v1/sessions").json(&json!({
                "cwd":test.root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"test"
            })),
            "lookup" => test.client.get(format!("{slow}/context")),
            "usage" => test.client.get(format!("{slow}/usage")),
            "trash" => test.client.post(format!("{slow}/trash")),
            _ => test.client.get(format!("http://localhost/v1/events?session={id}&after=0")),
        };
        let slow_task = tokio::spawn(async move {
            if let Some(mut reader) = reader {
                reader.recv().await.map_err(|e| e.0).unwrap();
                None
            } else {
                Some(request.send().await.unwrap())
            }
        });
        block.reached().await;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            other_operations(&test, &other, &dormant),
        )
        .await;
        if result.is_err() {
            blocked.push(format!("{operation}/{point}/closed={closed}"));
        }
        drop(block);
        if operation == "events_recv" {
            slow_task.abort();
            let _ = slow_task.await;
        } else {
            let response = slow_task.await.unwrap().unwrap();
            assert_eq!(
                response.status(),
                match operation {
                    "lookup" => StatusCode::CONFLICT,
                    "trash" => StatusCode::NO_CONTENT,
                    _ => StatusCode::OK,
                }
            );
        }
        test.stop().await;
    }
    assert!(
        blocked.is_empty(),
        "unrelated operations were blocked by: {blocked:?}"
    );
}

fn resume_request(test: &Harness, url: &str) -> reqwest::RequestBuilder {
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
}

async fn stays_pending<T>(task: &mut tokio::task::JoinHandle<T>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .is_err(),
        "same-session operation overtook the operation in progress"
    );
}

#[tokio::test]
async fn unopened_reads_exclude_resume_and_trash_on_the_same_session() {
    for endpoint in ["", "/usage", "/context", "events"] {
        for next in ["resume", "trash"] {
            let test = Harness::new().await;
            let url = test.create().await;
            test.client
                .delete(&url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            let id = url.rsplit('/').next().unwrap();
            let point = if matches!(endpoint, "/usage" | "events") {
                "events"
            } else {
                "snapshot"
            };
            let block = test.app.pauses.block(id, point);
            let request = if endpoint == "events" {
                test.client
                    .get(format!("http://localhost/v1/events?session={id}&after=0"))
            } else {
                test.client.get(format!("{url}{endpoint}"))
            };
            let reading = tokio::spawn(async move { request.send().await.unwrap() });
            block.reached().await;
            let request = if next == "resume" {
                resume_request(&test, &url)
            } else {
                test.client.post(format!("{url}/trash"))
            };
            let mut changing = tokio::spawn(async move { request.send().await.unwrap() });
            stays_pending(&mut changing).await;
            drop(block);
            let response = reading.await.unwrap();
            assert_eq!(
                response.status(),
                if endpoint == "/context" {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::OK
                }
            );
            drop(response);
            assert_eq!(
                changing.await.unwrap().status(),
                if next == "resume" {
                    StatusCode::OK
                } else {
                    StatusCode::NO_CONTENT
                }
            );
            test.stop().await;
        }
    }
}

#[tokio::test]
async fn simultaneous_resumes_share_one_instance_and_other_ids_can_open() {
    let test = Harness::new().await;
    let url = test.create().await;
    let other = test.create().await;
    for target in [&url, &other] {
        test.client
            .delete(target)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    let id = url.rsplit('/').next().unwrap();
    let block = test.app.pauses.block(id, "configure");
    let request = resume_request(&test, &url);
    let first = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    let request = resume_request(&test, &url);
    let mut second = tokio::spawn(async move { request.send().await.unwrap() });
    stays_pending(&mut second).await;
    tokio::time::timeout(Duration::from_secs(1), resume_request(&test, &other).send())
        .await
        .unwrap()
        .unwrap()
        .error_for_status()
        .unwrap();
    drop(block);
    let first: SessionResumeResponse = first
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let instance = test.app.opened_session(id).await.unwrap();
    let second: SessionResumeResponse = second
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first.event_seq, second.event_seq);
    assert!(instance.same_instance(&test.app.opened_session(id).await.unwrap()));
    test.stop().await;
}

#[tokio::test]
async fn trash_excludes_resume_and_attachment_until_the_directory_has_moved() {
    let test = Harness::new().await;
    let url = test.create().await;
    let other = test.create().await;
    test.client
        .delete(&other)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let id = url.rsplit('/').next().unwrap();
    let stale = test.app.opened_session(id).await.unwrap();
    let block = test.app.pauses.block(id, "trash_move");
    let request = test.client.post(format!("{url}/trash"));
    let deleting = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    let request = resume_request(&test, &url);
    let mut opening = tokio::spawn(async move { request.send().await.unwrap() });
    let request = test
        .client
        .get(format!("http://localhost/v1/events?session={id}&after=0"));
    let mut attaching = tokio::spawn(async move { request.send().await.unwrap() });
    stays_pending(&mut opening).await;
    stays_pending(&mut attaching).await;
    tokio::time::timeout(Duration::from_secs(1), resume_request(&test, &other).send())
        .await
        .unwrap()
        .unwrap()
        .error_for_status()
        .unwrap();
    drop(block);
    assert_eq!(deleting.await.unwrap().status(), StatusCode::NO_CONTENT);
    assert_eq!(opening.await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(attaching.await.unwrap().status(), StatusCode::NOT_FOUND);
    assert!(stale.attach_client().await.is_none());
    assert!(
        stale
            .update(SessionUpdateParams {
                name: Some("late".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert!(!test.app.store.session_dir(id).unwrap().exists());
    assert!(test.app.home.join("trash").join(id).exists());
    test.stop().await;
}

#[tokio::test]
async fn event_attachment_paths_exclude_trash_only_for_ordinary_clients() {
    for (catch_up, observe) in [(false, false), (true, false), (false, true), (true, true)] {
        let test = Harness::new().await;
        let url = test.create().await;
        let id = url.rsplit('/').next().unwrap().to_owned();
        let reader = if catch_up {
            test.client
                .delete(&url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            let cursor = test.read(&url).await.event_seq;
            let reader = EventReader::new(test.app.clone(), id.clone(), cursor, observe)
                .await
                .map_err(|e| e.0)
                .unwrap();
            resume_request(&test, &url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            Some(reader)
        } else {
            None
        };
        let block = test.app.pauses.block(&id, "subscribe");
        let app = test.app.clone();
        let attaching = tokio::spawn(async move {
            if let Some(mut reader) = reader {
                assert!(reader.recv().await.map_err(|e| e.0).unwrap().is_some());
                reader
            } else {
                EventReader::new(app, id, 0, observe)
                    .await
                    .map_err(|e| e.0)
                    .unwrap()
            }
        });
        block.reached().await;
        let request = test.client.post(format!("{url}/trash"));
        let mut deleting = tokio::spawn(async move { request.send().await.unwrap() });
        stays_pending(&mut deleting).await;
        drop(block);
        let reader = attaching.await.unwrap();
        assert_eq!(
            deleting.await.unwrap().status(),
            if observe {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::CONFLICT
            }
        );
        if !observe {
            assert_eq!(test.read(&url).await.session.status, SessionStatus::Idle);
        }
        drop(reader);
        test.stop().await;
    }
}

#[tokio::test]
async fn a_late_close_does_not_remove_a_newer_instance() {
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let old = test.app.opened_session(id).await.unwrap();
    let block = test.app.pauses.block(id, "close_remove");
    let request = test.client.delete(&url);
    let first = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    resume_request(&test, &url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let new = test.app.opened_session(id).await.unwrap();
    assert!(!old.same_instance(&new));
    drop(block);
    first.await.unwrap().error_for_status().unwrap();
    assert!(new.same_instance(&test.app.opened_session(id).await.unwrap()));
    test.patch(&url, json!({"name":"still open"})).await;
    test.stop().await;
}

#[tokio::test]
async fn shutdown_includes_accepted_opens_and_rejects_later_opens() {
    for creating in [false, true] {
        let test = Harness::new().await;
        let existing = test.create().await;
        let url = test.create().await;
        test.client
            .delete(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let id = url.rsplit('/').next().unwrap();
        let block = test.app.pauses.block(
            if creating { "*" } else { id },
            if creating { "create" } else { "configure" },
        );
        let request = if creating {
            test.client.post("http://localhost/v1/sessions").json(&json!({
                "cwd":test.root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"test"
            }))
        } else {
            resume_request(&test, &url)
        };
        let opening = tokio::spawn(async move { request.send().await.unwrap() });
        block.reached().await;
        let app = test.app.clone();
        let mut draining = tokio::spawn(async move { app.begin_drain().await });
        stays_pending(&mut draining).await;
        let request = resume_request(&test, &existing);
        let late = tokio::spawn(async move { request.send().await.unwrap() });
        drop(block);
        let response: serde_json::Value = opening
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let accepted_id = response["session"]["id"].as_str().unwrap();
        let sessions = tokio::time::timeout(Duration::from_secs(2), draining)
            .await
            .unwrap()
            .unwrap();
        let accepted = test.app.opened_session(accepted_id).await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .any(|session| session.same_instance(&accepted))
        );
        assert_eq!(late.await.unwrap().status(), StatusCode::CONFLICT);
        for session in sessions {
            session.close_for_shutdown().await.unwrap();
        }
        assert_eq!(
            accepted.snapshot().await.session.close_reason,
            Some(SessionCloseReason::ServerShutdown)
        );
        test.server.abort();
        assert!(test.server.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn shutdown_does_not_wait_for_an_unopened_disk_reader() {
    let test = Harness::new().await;
    let url = test.create().await;
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.create().await;
    let block = test
        .app
        .pauses
        .block(url.rsplit('/').next().unwrap(), "snapshot");
    let request = test.client.get(&url);
    let reading = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    let result = tokio::time::timeout(Duration::from_secs(1), test.app.begin_drain()).await;
    drop(block);
    reading.await.unwrap().error_for_status().unwrap();
    let sessions = result.expect("shutdown waited for an unopened session's reader");
    assert_eq!(sessions.len(), 1);
    for session in sessions {
        session.close_for_shutdown().await.unwrap();
    }
    test.server.abort();
    assert!(test.server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn cancelled_readers_keep_exclusion_until_the_blocking_read_finishes() {
    for kind in ["snapshot", "ledger", "events"] {
        let test = Harness::new().await;
        let url = test.create().await;
        test.client
            .delete(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let other = test.create().await;
        let id = url.rsplit('/').next().unwrap().to_owned();
        let block = test.app.pauses.block(&id, "disk_io");
        let app = test.app.clone();
        // Cancel the handler itself, as a disconnected HTTP consumer may do.
        let reading = tokio::spawn(async move {
            match kind {
                "snapshot" => read(State(app), ApiPath(id)).await.map(|_| ()),
                "ledger" => ledger(&app, &id).await.map(|_| ()),
                _ => EventReader::new(app, id, 0, false).await.map(|_| ()),
            }
            .map_err(|e| e.0)
        });
        block.reached().await;
        reading.abort();
        assert!(reading.await.unwrap_err().is_cancelled());
        let request = resume_request(&test, &url);
        let mut opening = tokio::spawn(async move { request.send().await.unwrap() });
        stays_pending(&mut opening).await;
        tokio::time::timeout(Duration::from_secs(1), test.read(&other))
            .await
            .unwrap();
        drop(block);
        opening.await.unwrap().error_for_status().unwrap();
        test.stop().await;
    }
}

#[tokio::test]
async fn trash_waiting_for_a_real_session_end_hook_keeps_other_sessions_responsive() {
    let hook = tempfile::tempdir().unwrap();
    let reached = hook.path().join("reached");
    let release = hook.path().join("release");
    let armed = hook.path().join("armed");
    let options = CoreOptions {
        hooks: serde_json::from_value(json!({"SessionEnd":[{"hooks":[{"type":"command","command":format!(
            "if [ -f '{}' ] && /bin/mkdir '{}' 2>/dev/null; then /bin/cat > '{}'; while [ ! -f '{}' ]; do /bin/sleep 0.01; done; fi",
            armed.display(), hook.path().join("claimed").display(), reached.display(), release.display()
        )}]}]})).unwrap(),
        ..Default::default()
    };
    let test = Harness::with_options(
        Arc::new(PausedModel(Some(canter_model::ToolCall {
            id: "write".into(),
            name: "write".into(),
            arguments: json!({"path":"file.txt","content":"changed"}).to_string(),
        }))),
        options,
    )
    .await;
    let slow = test.create().await;
    let other = test.create().await;
    let dormant = test.create().await;
    test.client
        .delete(&dormant)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    std::fs::write(armed, "go").unwrap();
    let session = test
        .app
        .opened_session(slow.rsplit('/').next().unwrap())
        .await
        .unwrap();
    drop(session.attach_client().await.unwrap());
    let request = test.client.post(format!("{slow}/trash"));
    let deleting = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !reached.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SessionEnd hook did not start");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        other_operations(&test, &other, &dormant),
    )
    .await;
    let still_waiting = !deleting.is_finished();
    std::fs::write(release, "go").unwrap();
    assert_eq!(deleting.await.unwrap().status(), StatusCode::NO_CONTENT);
    test.stop().await;
    assert!(still_waiting, "deletion did not wait for the hook");
    assert!(
        result.is_ok(),
        "another session waited for the SessionEnd hook"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual 50 MiB registry contention benchmark; creates and removes temporary data"]
async fn long_resume_other_session_input_benchmark() {
    use std::{fs, io::Write, time::Instant};
    let test = Harness::new().await;
    let other = test.create().await;
    let settings = test.read(&other).await.session.settings;
    drop(test.app.store.create("long", settings, 1).unwrap());
    let path = test
        .app
        .store
        .session_dir("long")
        .unwrap()
        .join("events.jsonl");
    let mut size = fs::metadata(&path).unwrap().len();
    let mut out = std::io::BufWriter::new(fs::OpenOptions::new().append(true).open(&path).unwrap());
    let (mut seq, mut rounds) = (1, 0);
    while size < 50 * 1024 * 1024 {
        rounds += 1;
        size += super::super::history_read::write_round(&mut out, "long", &mut seq, rounds);
    }
    out.flush().unwrap();
    drop(out);
    eprintln!("history_bytes={size}, events={seq}, rounds={rounds}");
    for attempt in 0..3 {
        let block = test.app.pauses.block("long", "snapshot");
        let request = test
            .client
            .post("http://localhost/v1/sessions/long/resume")
            .json(&json!({"env":{}}));
        let opening = tokio::spawn(async move { request.send().await.unwrap() });
        block.reached().await;
        let start = Instant::now();
        drop(block);
        test.client
            .post(format!("{other}/input"))
            .json(&json!({"input":[{"type":"text","text":"hello"}],"clientName":"benchmark"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        eprintln!(
            "other_input_{attempt}_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        opening
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        eprintln!(
            "long_resume_{attempt}_ms={:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        test.client
            .delete("http://localhost/v1/sessions/long")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    test.stop().await;
}

#[tokio::test]
async fn agents_list_skips_a_child_deleted_after_enumeration() {
    let (test, fake) = super::agents::harness(false).await;
    let parent = super::agents::parent(&test).await;
    let child = super::agents::spawn(&test, &parent, "Inspect code").await;
    super::agents::idle(&test, &child).await;
    let block = test
        .app
        .pauses
        .block(parent.rsplit('/').next().unwrap(), "agents.listed");
    let request = test.client.get(format!("{parent}/agents"));
    let listing = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    test.client
        .post(format!("{child}/trash"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    drop(block);
    let response: AgentListResponse = listing
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(response.data.is_empty());
    test.stop().await;
    fake.stop().await;
}
