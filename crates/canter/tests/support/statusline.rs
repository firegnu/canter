//! Real terminal-process shutdown, with an isolated server, PTY and script.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use super::*;

struct PtyReader {
    stop: Arc<AtomicBool>,
    input: Option<fs::File>,
    output: Arc<Mutex<Vec<u8>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PtyReader {
    fn start(mut master: fs::File) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let input = Some(master.try_clone().unwrap());
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let thread = std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            let mut pending = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                let mut poll = nix::libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: nix::libc::POLLIN,
                    revents: 0,
                };
                // The descriptor stays open and belongs to this reader thread.
                if unsafe { nix::libc::poll(&mut poll, 1, 50) } <= 0 {
                    continue;
                }
                let count = match master.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                captured.lock().unwrap().extend_from_slice(&buffer[..count]);
                pending.extend_from_slice(&buffer[..count]);
                // Answer only the normal terminal capability/cursor queries.
                for (query, answer) in [
                    (b"\x1b[6n".as_slice(), b"\x1b[1;1R".as_slice()),
                    (b"\x1b[?u".as_slice(), b"\x1b[?0u".as_slice()),
                    (b"\x1b[c".as_slice(), b"\x1b[?1;2c".as_slice()),
                ] {
                    if pending.windows(query.len()).any(|bytes| bytes == query) {
                        let _ = master.write_all(answer);
                    }
                }
                // A query can straddle reads; don't keep the whole screen.
                pending.drain(..pending.len().saturating_sub(3));
            }
        });
        Self {
            stop,
            input,
            output,
            thread: Some(thread),
        }
    }

    async fn wait_for(&self, text: &str) {
        tokio::time::timeout(WAIT, async {
            loop {
                if self
                    .output
                    .lock()
                    .unwrap()
                    .windows(text.len())
                    .any(|bytes| bytes == text.as_bytes())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "terminal did not show {text:?}: {}",
                String::from_utf8_lossy(&self.output.lock().unwrap())
            )
        });
    }

    fn type_line(&mut self, text: &str) {
        let input = self.input.as_mut().unwrap();
        input.write_all(text.as_bytes()).unwrap();
        input.write_all(b"\r").unwrap();
    }

    fn close(&mut self) {
        self.input.take();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        self.close();
    }
}

struct ScriptGroup(Pid);
impl Drop for ScriptGroup {
    fn drop(&mut self) {
        // Clean up the reproduced orphan even when an assertion fails.
        let _ = nix::sys::signal::killpg(self.0, Signal::SIGKILL);
    }
}

#[tokio::test]
async fn terminal_signals_stop_statusline_descendants_without_panicking_on_lost_stdout() {
    for (signal, close_pty) in [
        (Signal::SIGHUP, false),
        (Signal::SIGTERM, false),
        (Signal::SIGHUP, true),
    ] {
        let mut world = World::new(Vec::new()).await;
        let config_path = world.path("state/config.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["statusLine"] = json!("sh ./status.sh");
        fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        fs::write(
            world.path("work/status.sh"),
            "cat >/dev/null\nsleep 30 &\necho $! > worker.pid\nwait\n",
        )
        .unwrap();
        world.start_server().await;

        let (mut reader, mut terminal) = spawn_terminal(&world, &[], true);
        let worker = tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(text) = fs::read_to_string(world.path("work/worker.pid"))
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break Pid::from_raw(pid);
                }
                assert!(
                    terminal.try_wait().unwrap().is_none(),
                    "terminal exited: {}",
                    fs::read_to_string(world.path("terminal.log")).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("status command starts");
        let group = ScriptGroup(nix::unistd::getpgid(Some(worker)).unwrap());
        let detached = nix::unistd::getsid(Some(worker)).unwrap() == group.0;
        assert!(kill(worker, None).is_ok());
        let client = Pid::from_raw(terminal.id().unwrap() as i32);
        if close_pty {
            reader.close();
        } else {
            kill(client, signal).unwrap();
        }
        let status = tokio::time::timeout(WAIT, terminal.wait())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{signal:?}, close_pty={close_pty}: {}",
                    fs::read_to_string(world.path("terminal.log")).unwrap()
                )
            })
            .unwrap();
        let stopped = tokio::time::timeout(Duration::from_millis(500), async {
            while kill(worker, None).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        drop(group);
        reader.close();
        world.stop_server(Signal::SIGTERM).await;
        let log = fs::read_to_string(world.path("terminal.log")).unwrap();
        assert!(
            stopped,
            "{signal:?}: statusline descendant survived terminal exit ({status}); {log}"
        );
        assert!(
            !log.contains("panicked"),
            "lost terminal must not cause a panic: {log}"
        );
        assert!(
            detached,
            "statusline must have its own session, without the client's controlling terminal"
        );
        if !close_pty {
            assert!(status.success(), "{signal:?}: {status}; {log}");
        }
        assert!(world.requests.lock().unwrap().is_empty(), "no model calls");
    }
}

fn spawn_terminal(world: &World, args: &[&str], controlling_tty: bool) -> (PtyReader, Child) {
    let size = nix::pty::Winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = nix::pty::openpty(Some(&size), None).unwrap();
    // openpty does not set CLOEXEC. Inheriting the master into the client
    // would keep the PTY alive when the test closes its own master.
    for fd in [pty.master.as_raw_fd(), pty.slave.as_raw_fd()] {
        assert_ne!(
            unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFD, nix::libc::FD_CLOEXEC) },
            -1
        );
    }
    let slave = fs::File::from(pty.slave);
    let reader = PtyReader::start(fs::File::from(pty.master));
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"));
    command
        .args(args)
        .env_clear()
        .env("HOME", world.path("home"))
        .env("CANTER_HOME", world.path("state"))
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TERM", "xterm-256color")
        .env("CANTER_THEME", "dark")
        .current_dir(world.path("work"))
        .stdin(slave.try_clone().unwrap())
        .stdout(slave)
        .stderr(fs::File::create(world.path("terminal.log")).unwrap())
        .kill_on_drop(true);
    // Give the test client its own controlling terminal, as a terminal
    // emulator would. No user terminal or process group is involved.
    unsafe {
        command.pre_exec(move || {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            if controlling_tty && nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let terminal = command.spawn().unwrap();
    drop(command);
    (reader, terminal)
}

async fn picker_exit(startup: bool, signal: Option<Signal>, controlling_tty: bool) {
    let mut world = World::new(Vec::new()).await;
    world.start_server().await;
    let args = if startup { vec!["--resume"] } else { vec![] };
    let (mut reader, mut terminal) = spawn_terminal(&world, &args, controlling_tty);
    if !startup {
        reader.wait_for("fake-model").await;
        reader.type_line("/resume");
    }
    reader.wait_for("Which session to resume?").await;
    if let Some(signal) = signal {
        kill(Pid::from_raw(terminal.id().unwrap() as i32), signal).unwrap();
    } else {
        reader.close();
    }
    let result = tokio::time::timeout(Duration::from_secs(2), terminal.wait()).await;
    // Reap the reproduced stuck client before failing the assertion.
    if result.is_err() {
        terminal.kill().await.unwrap();
    }
    reader.close();
    world.stop_server(Signal::SIGTERM).await;
    let log = fs::read_to_string(world.path("terminal.log")).unwrap();
    let status = result
        .unwrap_or_else(|_| {
            panic!("picker startup={startup}, signal={signal:?} did not exit; {log}")
        })
        .unwrap();
    assert!(
        status.success(),
        "picker startup={startup}, signal={signal:?}: {status}; {log}"
    );
    assert!(!log.contains("panicked"), "{log}");
    assert!(world.requests.lock().unwrap().is_empty(), "no model calls");
}

#[tokio::test]
async fn startup_picker_exits_on_sighup() {
    picker_exit(true, Some(Signal::SIGHUP), true).await;
}

#[tokio::test]
async fn startup_picker_exits_on_sigterm() {
    picker_exit(true, Some(Signal::SIGTERM), true).await;
}

#[tokio::test]
async fn startup_picker_exits_on_pty_close() {
    picker_exit(true, None, true).await;
}

#[tokio::test]
async fn slash_resume_picker_exits_on_sighup() {
    picker_exit(false, Some(Signal::SIGHUP), true).await;
}

#[tokio::test]
async fn slash_resume_picker_exits_on_sigterm() {
    picker_exit(false, Some(Signal::SIGTERM), true).await;
}

#[tokio::test]
async fn slash_resume_picker_exits_on_pty_close() {
    picker_exit(false, None, true).await;
}

#[tokio::test]
async fn startup_picker_exits_on_input_end_without_sighup() {
    // No controlling terminal means closing the master sends no SIGHUP.
    // This checks input EOF independently of the shared signal listener.
    picker_exit(true, None, false).await;
}

#[tokio::test]
async fn slash_resume_picker_exits_on_input_end_without_sighup() {
    picker_exit(false, None, false).await;
}
