use super::*;
use canter_tools::lsp::{FileContext, LspService, Round};
use nix::sys::signal::{Signal, kill};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path, process::Stdio};
use tokio::process::{Child, Command};

fn fixture_executable() -> PathBuf {
    static FIXTURE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let root = tempfile::tempdir().unwrap();
            let status = std::process::Command::new("rustc")
                .args([
                    "--edition=2024",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../canter-tools/tests/fixtures/lsp.rs"
                    ),
                    "-o",
                ])
                .arg(root.path().join("fake-lsp"))
                .status()
                .unwrap();
            assert!(status.success());
            root
        })
        .path()
        .join("fake-lsp")
}
fn gone(pid: u32) -> bool {
    kill(nix::unistd::Pid::from_raw(pid as i32), None) == Err(nix::errno::Errno::ESRCH)
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

// Isolated test process, running the production server lifecycle. The factory
// supplies an instance through the same narrow API P2-133 will call after writes.
#[tokio::test]
async fn server_process() {
    let Ok(directory) = std::env::var("LSP_SERVER_TEST_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let replace = std::env::var("LSP_SERVER_TEST_REPLACE").as_deref() == Ok("true");
    let fixture = std::env::var("LSP_SERVER_TEST_EXECUTABLE").unwrap();
    serve_with_lsp(replace, "lsp-test", move |config| {
        let service = LspService::new(config)?;
        let startup = service.clone();
        tokio::spawn(async move {
            let context = FileContext {
                cwd: directory.clone(),
                additional_directories: vec![],
                round: Round::Modify,
                environment: BTreeMap::from([
                    ("HOME".into(), directory.to_string_lossy().into_owned()),
                    (
                        "CANTER_HOME".into(),
                        directory.join("private").to_string_lossy().into_owned(),
                    ),
                    (
                        "PATH".into(),
                        Path::new(&fixture)
                            .parent()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    (
                        "LSP_TEST_DIR".into(),
                        directory.to_string_lossy().into_owned(),
                    ),
                    ("LSP_TEST_MODE".into(), "child".into()),
                ]),
            };
            let instance = startup
                .for_file(
                    &directory.join("main.fake"),
                    &context,
                    &CancellationToken::new(),
                )
                .await
                .unwrap()
                .unwrap();
            fs::write(directory.join("instance"), instance.pid.to_string()).unwrap();
            while instance.is_running() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        Ok(service)
    })
    .await
    .unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
    executable: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("lsp-server-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::create_dir(root.path().join("state")).unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        fs::write(root.path().join("state/config.json"),json!({
            "base_url":"http://127.0.0.1:1","model":"fake","api_key":"test-only-key","context_window":1048576,
            "lsp":{"enabled":true,"servers":{"fake":{"command":"fake-lsp","args":[],"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}}
        }).to_string()).unwrap();
        Self {
            root,
            executable: fixture_executable(),
        }
    }
    fn start(&self, name: &str, replace: bool) -> Child {
        let directory = self.root.path().join(name);
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("project.marker"), "").unwrap();
        fs::write(directory.join("main.fake"), "").unwrap();
        let log = fs::File::create(directory.join("server.log")).unwrap();
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lsp_tests::server_process", "--nocapture"])
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("CANTER_HOME", self.root.path().join("state"))
            .env("LSP_SERVER_TEST_DIR", directory)
            .env("LSP_SERVER_TEST_REPLACE", replace.to_string())
            .env("LSP_SERVER_TEST_EXECUTABLE", &self.executable)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }
    async fn ready(&self, name: &str, child: &mut Child) -> (u32, u32) {
        let directory = self.root.path().join(name);
        until(|| {
            assert!(
                child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(directory.join("server.log")).unwrap()
            );
            directory.join("instance").exists()
                && fs::read_to_string(directory.join("server.log"))
                    .unwrap()
                    .contains("canter server ready")
        })
        .await;
        let pid: u32 = fs::read_to_string(directory.join("instance"))
            .unwrap()
            .parse()
            .unwrap();
        let log = fs::read_to_string(directory.join(format!("{pid}.log"))).unwrap();
        let descendant = log
            .lines()
            .find_map(|line| line.strip_prefix("CHILD "))
            .unwrap()
            .parse()
            .unwrap();
        (pid, descendant)
    }
    async fn exited(&self, name: &str, child: &mut Child, pids: (u32, u32), graceful: bool) {
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        if graceful {
            assert!(status.success());
        }
        until(|| gone(pids.0) && gone(pids.1)).await;
        if graceful {
            let log =
                fs::read_to_string(self.root.path().join(name).join(format!("{}.log", pids.0)))
                    .unwrap();
            assert!(
                log.contains("\"method\":\"shutdown\""),
                "server must await LSP shutdown"
            );
            assert!(log.contains("\"method\":\"exit\""));
        }
    }
}

#[tokio::test]
async fn server_stop_and_replace_close_lsp_before_releasing_the_lease() {
    let fixture = Fixture::new();
    let mut old = fixture.start("old", false);
    let old_pids = fixture.ready("old", &mut old).await;
    let mut new = fixture.start("new", true);
    let new_pids = fixture.ready("new", &mut new).await;
    fixture.exited("old", &mut old, old_pids, true).await;
    kill(
        nix::unistd::Pid::from_raw(new.id().unwrap() as i32),
        Signal::SIGTERM,
    )
    .unwrap();
    fixture.exited("new", &mut new, new_pids, true).await;
}

#[tokio::test]
async fn sigkill_of_server_removes_lsp_process_group() {
    let fixture = Fixture::new();
    let mut server = fixture.start("killed", false);
    let pids = fixture.ready("killed", &mut server).await;
    server.kill().await.unwrap();
    fixture.exited("killed", &mut server, pids, false).await;
}
