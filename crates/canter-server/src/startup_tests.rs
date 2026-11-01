use super::*;
use canter_protocol::{EventPayload, SessionSnapshot};
use std::fs;

fn settings() -> canter_protocol::SessionSettings {
    SessionSnapshot::empty("s1").settings
}

fn write_config(home: &Path) {
    fs::write(home.join("config.json"), serde_json::to_vec(&serde_json::json!({
        "base_url":"http://127.0.0.1:1", "model":"fake", "api_key":"test-only", "context_window":1048576
    })).unwrap()).unwrap();
}

#[tokio::test]
async fn replacement_warms_before_the_old_lock_is_released_and_refreshes_afterward() {
    use std::time::Duration;
    let temp = tempfile::tempdir().unwrap();
    write_config(temp.path());
    let old = Lease::acquire(temp.path().to_owned(), false).unwrap();
    old.publish().unwrap();
    let source = Store::new(temp.path().join("sessions")).unwrap();
    let mut writer = source.create("s1", settings(), 1).unwrap();
    let (warmed, mut ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    // Gate the real disk warmup at its completion while the old lease is held.
    let starting = tokio::spawn(initialize_with(temp.path().to_owned(), true, move |home| {
        let store = open_and_warm(home)?;
        let _ = warmed.send(());
        let _ = wait.recv();
        Ok(store)
    }));
    let overlapped = tokio::time::timeout(Duration::from_secs(2), &mut ready)
        .await
        .is_ok();
    let requested = tokio::time::timeout(Duration::from_secs(2), async {
        while !lifecycle::replacement_requested(temp.path(), &old.registration) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    writer
        .append(
            EventPayload::Warning {
                message: "old daemon's final event".into(),
            },
            2,
        )
        .unwrap();
    drop(old);
    drop(release);
    let (_lease, _, store) = starting.await.unwrap().unwrap();
    assert!(
        overlapped,
        "warming must finish even while the daemon lock is held"
    );
    assert!(
        requested,
        "replacement must start waiting while warmup is still gated"
    );
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
}

#[test]
fn prewarm_reads_under_old_daemon_and_writer_locks_then_refreshes_changes() {
    let temp = tempfile::tempdir().unwrap();
    let old = Lease::acquire(temp.path().to_owned(), false).unwrap();
    let source = Store::new(temp.path().join("sessions")).unwrap();
    let mut writer = source.create("s1", settings(), 1).unwrap();
    let warmed = open_and_warm(temp.path()).unwrap();
    assert_eq!(warmed.list().unwrap(), vec![writer.snapshot().summary()]);
    writer
        .append(
            EventPayload::Warning {
                message: "during lock wait".into(),
            },
            2,
        )
        .unwrap();
    drop(source.create("new", settings(), 3).unwrap());
    drop(old);
    let _successor = Lease::acquire(temp.path().to_owned(), false).unwrap();
    warm_summaries(&warmed);
    assert_eq!(warmed.list().unwrap(), source.list().unwrap());
    assert_eq!(warmed.list().unwrap()[1].last_event_seq, 2);
}

#[test]
fn root_read_failure_during_warmup_is_retried_later() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let store = Store::new(&root).unwrap();
    fs::remove_dir(&root).unwrap();
    assert!(store.list().is_err());
    warm_summaries(&store);
    fs::create_dir(&root).unwrap();
    let writer = store.create("s1", settings(), 1).unwrap();
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
}

#[tokio::test]
async fn initialization_skips_broken_and_empty_histories() {
    let temp = tempfile::tempdir().unwrap();
    write_config(temp.path());
    let source = Store::new(temp.path().join("sessions")).unwrap();
    let writer = source.create("s1", settings(), 1).unwrap();
    for (id, bytes) in [
        ("broken", b"not JSON\n".as_slice()),
        ("empty", b"".as_slice()),
    ] {
        let dir = source.session_dir(id).unwrap();
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), bytes).unwrap();
    }
    let (_lease, _, store) = initialize(temp.path().to_owned(), false).await.unwrap();
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
}

#[tokio::test]
async fn initialized_store_keeps_the_resolved_home_like_the_lease() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("original");
    fs::create_dir(&home).unwrap();
    write_config(&home);
    let source = Store::new(home.join("sessions")).unwrap();
    let writer = source.create("s1", settings(), 1).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let (_lease, _, store) = initialize(alias.clone(), false).await.unwrap();
    let other = temp.path().join("other");
    Store::new(other.join("sessions")).unwrap();
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(other, alias).unwrap();
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
}
