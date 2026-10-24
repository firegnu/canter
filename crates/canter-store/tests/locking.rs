use canter_protocol::{PermissionMode, SessionSettings};
use canter_store::{PrivateSessionConfig, Store, StoreError};
use std::collections::BTreeMap;
use std::process::Command;

#[test]
fn a_second_writer_cannot_lock_the_same_session() {
    const CHILD_ROOT: &str = "CANTER_TEST_LOCK_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let store = Store::new(root).unwrap();
        assert!(matches!(store.open("locked"), Err(StoreError::Locked)));
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let writer = store
        .create(
            "locked",
            SessionSettings {
                cwd: temp.path().to_string_lossy().into(),
                name: None,
                model: "fake".into(),
                approval_policy: PermissionMode::AlwaysAsk,
            },
            PrivateSessionConfig {
                env: BTreeMap::new(),
            },
            1,
        )
        .unwrap();
    assert!(matches!(store.open("locked"), Err(StoreError::Locked)));
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_second_writer_cannot_lock_the_same_session",
            "--nocapture",
        ])
        .env(CHILD_ROOT, temp.path())
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "child lock check failed: {}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    drop(writer);
    assert!(store.open("locked").is_ok());
}
