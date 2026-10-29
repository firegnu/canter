use std::{collections::BTreeMap, path::PathBuf, sync::OnceLock};

pub fn executable() -> PathBuf {
    static FIXTURE: OnceLock<tempfile::TempDir> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let output = std::process::Command::new("rustc")
                .args([
                    "--edition=2024",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lsp.rs"),
                    "-o",
                ])
                .arg(dir.path().join("fake-lsp"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            dir
        })
        .path()
        .join("fake-lsp")
}
pub fn environment(dir: &std::path::Path, mode: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".into(), dir.to_string_lossy().into_owned()),
        (
            "CANTER_HOME".into(),
            dir.join("state").to_string_lossy().into_owned(),
        ),
        (
            "PATH".into(),
            executable()
                .parent()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        ("LSP_TEST_DIR".into(), dir.to_string_lossy().into_owned()),
        ("LSP_TEST_MODE".into(), mode.into()),
    ])
}
pub async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture condition timed out");
}
pub fn gone(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None)
        == Err(nix::errno::Errno::ESRCH)
}
