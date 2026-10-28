use canter_tools::{CommandHandle, Executor, LocalExecutor};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;

// The nofile benchmark changes a process limit. Serialize test cases even under
// cargo test; the commands within the concurrency regression still run together.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn executor(dir: &tempfile::TempDir) -> Arc<LocalExecutor> {
    Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::from([
                ("HOME".into(), dir.path().to_string_lossy().into_owned()),
                (
                    "CANTER_HOME".into(),
                    dir.path().join("state").to_string_lossy().into_owned(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]),
            vec![],
        )
        .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_commands_do_not_inherit_each_others_pipes() {
    let _test = TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let executor = executor(&dir);
    let barrier = Arc::new(tokio::sync::Barrier::new(300));
    let mut commands = tokio::task::JoinSet::new();
    for _ in 0..300 {
        let executor = executor.clone();
        let barrier = barrier.clone();
        commands.spawn(async move {
            barrier.wait().await;
            let mut command = executor.spawn("for fd in /dev/fd/*; do if [ -p \"$fd\" ]; then printf '%s\\n' \"$fd\"; fi; done", &CancellationToken::new()).await.unwrap();
            let mut output = String::new();
            command.stdout.read_to_string(&mut output).await.unwrap();
            let status = command.process.wait().await.unwrap();
            command.process.kill().await.unwrap();
            assert!(status.success());
            output
        });
    }
    let mut outputs = Vec::new();
    while let Some(output) = commands.join_next().await {
        outputs.push(output.unwrap());
    }
    for output in outputs {
        assert_eq!(output, "/dev/fd/1\n/dev/fd/2\n", "inherited server pipes");
    }
}

async fn sleeping_command(executor: &LocalExecutor) -> (CommandHandle, i32) {
    let mut handle = executor
        .spawn("echo $$; exec sleep 30", &CancellationToken::new())
        .await
        .unwrap();
    let mut pid = String::new();
    BufReader::new(&mut handle.stdout)
        .read_line(&mut pid)
        .await
        .unwrap();
    (handle, pid.trim().parse().unwrap())
}

#[tokio::test]
async fn command_has_no_children_it_did_not_start() {
    let _test = TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (mut handle, pid) = sleeping_command(&executor(&dir)).await;
    let children = tokio::process::Command::new("/usr/bin/pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .await
        .unwrap();
    handle.process.kill().await.unwrap();
    handle.process.wait().await.unwrap();
    assert_eq!(
        children.status.code(),
        Some(1),
        "unexpected children: {}",
        String::from_utf8_lossy(&children.stdout)
    );
}

#[tokio::test]
async fn guardian_execs_a_small_program_and_sigterm_stops_it() {
    let _test = TEST_LOCK.lock().await;
    // A real server installs this handler; exec must reset it in the watchdog.
    let _term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut command, leader) = sleeping_command(&executor(&dir)).await;
    let found = tokio::process::Command::new("/usr/bin/pgrep")
        .args(["-f", &format!("canter-shell-guardian {leader}$")])
        .output()
        .await
        .unwrap();
    let guardian = String::from_utf8(found.stdout)
        .unwrap()
        .trim()
        .parse::<i32>();
    if guardian.is_err() {
        command.process.kill().await.unwrap();
        command.process.wait().await.unwrap();
    }
    let guardian = nix::unistd::Pid::from_raw(
        guardian.expect("watchdog must exec under its own identifiable command name"),
    );
    let name = tokio::process::Command::new("/bin/ps")
        .args(["-p", &guardian.to_string(), "-o", "comm="])
        .output()
        .await
        .unwrap();
    nix::sys::signal::kill(guardian, nix::sys::signal::Signal::SIGTERM).unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        while nix::sys::signal::kill(guardian, None) != Err(nix::errno::Errno::ESRCH) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    command.process.kill().await.unwrap();
    command.process.wait().await.unwrap();
    assert!(
        String::from_utf8(name.stdout)
            .unwrap()
            .trim()
            .ends_with("sh")
    );
    assert!(
        stopped.is_ok(),
        "watchdog retained the server's SIGTERM handler"
    );
}

#[tokio::test]
async fn true_startup_cost_does_not_scale_with_nofile() {
    let _test = TEST_LOCK.lock().await;
    use nix::libc;
    struct Restore(libc::rlimit);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::setrlimit(libc::RLIMIT_NOFILE, &self.0);
            }
        }
    }
    let mut original = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
        0
    );
    let _restore = Restore(original);
    let dir = tempfile::tempdir().unwrap();
    let executor = executor(&dir);
    let mut measurements = Vec::new();
    for limit in [256, original.rlim_max.min(1_048_576)] {
        let limit = libc::rlimit {
            rlim_cur: limit,
            rlim_max: original.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let started = Instant::now();
        for _ in 0..20 {
            let mut command = executor
                .spawn("true", &CancellationToken::new())
                .await
                .unwrap();
            assert!(command.process.wait().await.unwrap().success());
            command.process.kill().await.unwrap();
        }
        let average = started.elapsed() / 20;
        println!(
            "nofile={} true mean over 20 runs: {:.3} ms",
            limit.rlim_cur,
            average.as_secs_f64() * 1000.0
        );
        measurements.push(average);
    }
    // Scheduling noise has room; scanning a million descriptors does not.
    assert!(
        measurements[1] <= measurements[0] * 3 + Duration::from_millis(15),
        "startup grew with nofile: {measurements:?}"
    );
}
