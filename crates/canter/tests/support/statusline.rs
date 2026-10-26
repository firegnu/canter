//! Real terminal-process shutdown, with an isolated server, PTY and script.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use super::*;

struct PtyReader {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PtyReader {
    fn start(mut master: fs::File) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
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
            thread: Some(thread),
        }
    }

    fn close(&mut self) {
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
        let mut reader = PtyReader::start(fs::File::from(pty.master));
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"));
        command
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
            command.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut terminal = command.spawn().unwrap();
        drop(command);
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
