use super::*;
use std::os::fd::FromRawFd;

#[tokio::test]
async fn picker_exits_on_hangup_while_the_event_reader_keeps_its_sender() {
    let (mut master, mut slave) = (-1, -1);
    // Both output pointers are valid; optional name, termios and size are unused.
    assert_eq!(
        unsafe {
            nix::libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // openpty succeeded and transferred ownership of these two descriptors.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        assert_ne!(
            unsafe { nix::libc::fcntl(fd.as_raw_fd(), nix::libc::F_SETFD, nix::libc::FD_CLOEXEC) },
            -1
        );
    }
    assert!(terminal_input_open(slave.as_fd(), 0).unwrap());
    let (sender, mut input) = input();
    input.tty = Some(slave);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let picker = Picker::new(Vec::new(), "/work".into(), None);
    let mut shutdown = Shutdown::listen().unwrap();
    let mut tab = TabTitle::new(&BTreeMap::new());

    // Hang up after the list is shown. Keep the sender alive, just as when
    // crossterm races the native poll and loops on EOF inside its reader.
    let hangup = async {
        while sender.frames() == 0 {
            tokio::task::yield_now().await;
        }
        drop(master);
    };
    let picked = tokio::time::timeout(
        Duration::from_secs(2),
        pick(
            &mut terminal,
            &mut input,
            picker,
            &mut shutdown,
            (&mut tab, "work"),
        ),
    );
    let ((), result) = tokio::join!(hangup, picked);
    assert_eq!(
        result
            .expect("hangup must not wait for the event reader")
            .unwrap(),
        Pick::Exit
    );
}
