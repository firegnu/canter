//! A pipe owned only by the executor keeps each command's watchdog alive.
//! The watchdog is forked before exec, so a server death during spawn is covered too.
use nix::libc;
use std::{io, os::fd::AsRawFd};

pub(crate) fn attach(command: &mut tokio::process::Command) -> io::Result<io::PipeWriter> {
    let (watch, owner) = io::pipe()?;
    let (ready, signal) = io::pipe()?;
    let owner_fd = owner.as_raw_fd();
    // Compute this before fork; the child must not allocate or acquire Rust locks.
    let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if max_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: after fork both branches use only async-signal-safe syscalls. The
    // watchdog exits with _exit, never unwinding or returning into the runtime.
    unsafe {
        command.pre_exec(move || {
            let group = libc::getpid();
            let guardian = libc::fork();
            if guardian < 0 {
                return Err(io::Error::last_os_error());
            }
            if guardian == 0 {
                if libc::setpgid(0, 0) != 0
                    || libc::dup2(watch.as_raw_fd(), 0) < 0
                    || libc::dup2(signal.as_raw_fd(), 1) < 0
                {
                    libc::_exit(1);
                }
                // In particular, close the spawn error pipe, stdout/stderr and
                // every other command's ownership pipe. No command can keep a
                // different command alive after the server disappears.
                for fd in 2..max_fd {
                    libc::close(fd as libc::c_int);
                }
                let byte = 1_u8;
                if libc::write(1, (&byte as *const u8).cast(), 1) != 1 {
                    libc::_exit(1);
                }
                libc::close(1);
                let mut byte = 0_u8;
                loop {
                    let count = libc::read(0, (&mut byte as *mut u8).cast(), 1);
                    if count == 1 {
                        // Normal cleanup already killed the group.
                        libc::_exit(0);
                    }
                    if count == 0 {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(0);
                    }
                    if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(1);
                    }
                }
            }
            libc::close(signal.as_raw_fd());
            libc::close(owner_fd);
            let mut byte = 0_u8;
            loop {
                let mut poll = libc::pollfd {
                    fd: ready.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let available = libc::poll(&mut poll, 1, 100);
                if available > 0 {
                    let count = libc::read(ready.as_raw_fd(), (&mut byte as *mut u8).cast(), 1);
                    if count == 1 {
                        return Ok(());
                    }
                }
                // The server still holds a copy of the readiness writer until
                // spawn returns. Detect an early watchdog death without waiting
                // for EOF on that pipe.
                if libc::waitpid(guardian, std::ptr::null_mut(), libc::WNOHANG) == guardian {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                if available < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                }
            }
        });
    }
    Ok(owner)
}
