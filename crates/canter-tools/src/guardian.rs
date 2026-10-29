//! A pipe owned only by the executor keeps each command's watchdog alive.
//! A double fork detaches the watchdog from the command; exec releases the server's
//! address space, resets its signal handlers and closes CLOEXEC descriptors.
use nix::libc;
use std::{io, os::fd::AsRawFd};

pub(crate) fn attach(command: &mut tokio::process::Command) -> io::Result<io::PipeWriter> {
    let (watch, owner) = io::pipe()?;
    let (ready, signal) = io::pipe()?;
    let owner_fd = owner.as_raw_fd();
    // All strings are static. The only dynamic argument is the group ID, formatted
    // into a stack buffer before the forks, without allocation or Rust locks.
    let script =
        c"printf .; exec 1>&- 2>&-; if IFS= read -r _; then exit 0; fi; kill -KILL -- -\"$1\"";
    // SAFETY: after fork both branches use only async-signal-safe syscalls. The
    // watchdog exits with _exit, never unwinding or returning into the runtime.
    unsafe {
        command.pre_exec(move || {
            let group = libc::getpid();
            let mut digits = [0 as libc::c_char; 12];
            let mut index = digits.len() - 1;
            let mut number = group as u32;
            loop {
                index -= 1;
                digits[index] = (b'0' + (number % 10) as u8) as libc::c_char;
                number /= 10;
                if number == 0 {
                    break;
                }
            }
            let argv = [
                c"/bin/sh".as_ptr(),
                c"-c".as_ptr(),
                script.as_ptr(),
                c"canter-shell-guardian".as_ptr(),
                digits[index..].as_ptr(),
                std::ptr::null(),
            ];
            let intermediate = libc::fork();
            if intermediate < 0 {
                return Err(io::Error::last_os_error());
            }
            if intermediate == 0 {
                libc::close(owner_fd);
                let detached = libc::fork();
                if detached < 0 {
                    libc::_exit(1);
                }
                if detached > 0 {
                    libc::close(signal.as_raw_fd());
                    libc::_exit(wait_ready(ready.as_raw_fd(), detached));
                }
                if libc::setpgid(0, 0) != 0
                    || libc::dup2(watch.as_raw_fd(), 0) < 0
                    || libc::dup2(signal.as_raw_fd(), 1) < 0
                {
                    libc::_exit(1);
                }
                libc::close(2);
                // The shared process launcher serializes pipe creation and spawn. All other
                // descriptors (including the spawn error pipe and ownership
                // writers) are CLOEXEC; no scan up to RLIMIT_NOFILE is needed.
                libc::execv(c"/bin/sh".as_ptr(), argv.as_ptr());
                libc::_exit(1);
            }
            libc::close(signal.as_raw_fd());
            libc::close(owner_fd);
            let mut status = 0;
            loop {
                if libc::waitpid(intermediate, &mut status, 0) == intermediate {
                    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                        return Ok(());
                    }
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error);
                }
            }
        });
    }
    Ok(owner)
}

// Only called between fork and _exit in the intermediate process.
unsafe fn wait_ready(fd: libc::c_int, guardian: libc::pid_t) -> libc::c_int {
    // SAFETY: valid inherited descriptors and child PID; only signal-safe syscalls.
    unsafe {
        loop {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let available = libc::poll(&mut poll, 1, 100);
            if available > 0 {
                let mut byte = 0_u8;
                if libc::read(fd, (&mut byte as *mut u8).cast(), 1) == 1 {
                    return 0;
                }
            }
            // The server also holds the readiness writer until spawn returns.
            // Detect failure here rather than waiting for that writer's EOF.
            if libc::waitpid(guardian, std::ptr::null_mut(), libc::WNOHANG) == guardian {
                return 1;
            }
            if available < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return 1;
            }
        }
    }
}
