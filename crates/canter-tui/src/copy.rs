//! Putting text on the clipboard without holding up the screen. A thread of
//! its own writes it to this machine's clipboard (arboard). In tmux and over
//! SSH the text also goes to the terminal as OSC 52, which puts it on the
//! clipboard of the machine the terminal runs on; in tmux it is wrapped so
//! that tmux passes it on. The status line then says what was copied, or why
//! it failed.
//!
//! Follows how Codex `rust-v0.163.0` decides between the two
//! (`codex-rs/tui/src/clipboard_copy.rs`); no code is taken from it.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread;

use ratatui::text::{Line, Span};

use crate::text::sanitize;
use crate::theme;

/// How long the status line says what was copied.
pub(crate) const COPIED_MS: i64 = 2000;
/// How long it says a copy failed.
pub(crate) const FAILED_MS: i64 = 5000;
/// A copy the clipboard has not taken after this long has failed.
const TIMEOUT_MS: i64 = 3000;
/// The most text OSC 52 carries, in bytes, as in Codex: terminals refuse or
/// cut longer ones.
const OSC52_MAX: usize = 100_000;

type Writer = Box<dyn FnMut(&str) -> Result<(), String> + Send>;

pub(crate) struct Copier {
    /// Writes to the clipboard; moves to the thread when the first copy starts it.
    writer: Option<Writer>,
    thread: Option<Worker>,
    /// `Some(tmux)` when copies also go to the terminal as OSC 52.
    terminal: Option<bool>,
    /// OSC 52 waiting to be written to the terminal.
    sequence: Option<String>,
    copies: u64,
    pending: Option<Pending>,
    /// What the status line shows instead, and until when (Unix ms).
    note: Option<(Line<'static>, i64)>,
}

struct Worker {
    jobs: mpsc::Sender<(u64, String)>,
    results: mpsc::Receiver<(u64, Result<(), String>)>,
}

/// A copy the clipboard has not answered yet.
struct Pending {
    id: u64,
    lines: usize,
    chars: usize,
    /// A link copied in place of opening it: what the note says then.
    link: Option<&'static str>,
    /// OSC 52 was queued for the terminal.
    sent: bool,
    /// Why OSC 52 could not be sent, in tmux or over SSH.
    terminal_error: Option<String>,
    /// Starting the thread or handing it the text failed.
    failed: Option<String>,
    /// When the screen first waited for it (Unix ms).
    since: Option<i64>,
}

impl Default for Copier {
    /// This machine's clipboard, and the terminal in tmux or over SSH.
    fn default() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let terminal = if set("TMUX") || set("TMUX_PANE") {
            Some(true)
        } else if set("SSH_TTY") || set("SSH_CONNECTION") {
            Some(false)
        } else {
            None
        };
        Self::new(Box::new(native), terminal)
    }
}

impl Copier {
    /// Copies only with `writer`, never to the terminal (for tests).
    pub fn with_writer(writer: impl FnMut(&str) -> Result<(), String> + Send + 'static) -> Self {
        Self::new(Box::new(writer), None)
    }

    fn new(writer: Writer, terminal: Option<bool>) -> Self {
        Self {
            writer: Some(writer),
            thread: None,
            terminal,
            sequence: None,
            copies: 0,
            pending: None,
            note: None,
        }
    }

    /// Starts copying `text`; [`Copier::poll`] finds out how it went. A copy
    /// started before this one is no longer waited for. Returns which copy
    /// this is.
    pub fn copy(&mut self, text: String) -> u64 {
        self.copies += 1;
        let mut pending = Pending {
            id: self.copies,
            lines: text.lines().count(),
            chars: text.chars().count(),
            link: None,
            sent: false,
            terminal_error: None,
            failed: None,
            since: None,
        };
        if let Some(tmux) = self.terminal {
            match osc52(&text, tmux) {
                Ok(sequence) => {
                    self.sequence = Some(sequence);
                    pending.sent = true;
                }
                Err(error) => pending.terminal_error = Some(error),
            }
        }
        match self.worker() {
            Ok(worker) => {
                if worker.jobs.send((pending.id, text)).is_err() {
                    pending.failed = Some("the clipboard thread stopped".into());
                }
            }
            Err(error) => pending.failed = Some(error),
        }
        self.pending = Some(pending);
        self.copies
    }

    /// [`Copier::copy`] for a link copied in place of opening it: the note
    /// says `done` (`Link copied`, `Path copied`).
    pub fn copy_link(&mut self, text: String, done: &'static str) {
        self.copy(text);
        if let Some(pending) = &mut self.pending {
            pending.link = Some(done);
        }
    }

    /// Shows `line` in the status line until `until` (Unix ms), in place of
    /// the note before it: that a link opened, or why not.
    pub fn show(&mut self, line: Line<'static>, until: i64) {
        self.note = Some((line, until));
    }

    fn worker(&mut self) -> Result<&Worker, String> {
        if self.thread.is_none() {
            let mut writer = self
                .writer
                .take()
                .ok_or("the clipboard thread could not start")?;
            let (jobs, inbox) = mpsc::channel::<(u64, String)>();
            let (outbox, results) = mpsc::channel();
            thread::Builder::new()
                .name("canter-clipboard".into())
                .spawn(move || {
                    while let Ok(mut job) = inbox.recv() {
                        // Copies that piled up meanwhile: only the latest matters.
                        while let Ok(newer) = inbox.try_recv() {
                            job = newer;
                        }
                        let result = writer(&job.1);
                        if outbox.send((job.0, result)).is_err() {
                            return;
                        }
                    }
                })
                .map_err(|error| format!("the clipboard thread could not start: {error}"))?;
            self.thread = Some(Worker { jobs, results });
        }
        Ok(self.thread.as_ref().expect("started above"))
    }

    /// OSC 52 to write to the terminal between frames, once.
    pub fn take_sequence(&mut self) -> Option<String> {
        self.sequence.take()
    }

    /// Finds out how the latest copy went and lets an old note go; at `now`
    /// (Unix ms), before each frame. Returns the copy that answered now, if
    /// one did, and whether it worked or why not.
    pub fn poll(&mut self, now: i64) -> Option<(u64, Result<(), String>)> {
        if self.note.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.note = None;
        }
        let mut answer = None;
        if let Some(worker) = &self.thread {
            loop {
                match worker.results.try_recv() {
                    Ok((id, result)) => {
                        if self
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.id == id)
                        {
                            answer = Some(result);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        answer.get_or_insert(Err("the clipboard thread stopped".into()));
                        break;
                    }
                }
            }
        }
        let Some(pending) = &mut self.pending else {
            return None;
        };
        let since = *pending.since.get_or_insert(now);
        let result = if pending.sent {
            // The terminal does not say whether it took it.
            Ok(())
        } else if let Some(error) = pending.failed.take() {
            Err(error)
        } else if let Some(result) = answer {
            result
        } else if now - since >= TIMEOUT_MS {
            Err(format!(
                "the clipboard did not answer in {}s",
                TIMEOUT_MS / 1000
            ))
        } else {
            return None;
        };
        let reason = result.map_err(|error| match &pending.terminal_error {
            Some(terminal) => format!("{error}; OSC 52: {terminal}"),
            None => error,
        });
        let note = match &reason {
            Ok(()) => match pending.link {
                Some(done) => (link_copied(done), now + COPIED_MS),
                None => (copied(pending.lines, pending.chars), now + COPIED_MS),
            },
            Err(reason) => (failed(reason), now + FAILED_MS),
        };
        let answer = (pending.id, reason);
        self.note = Some(note);
        self.pending = None;
        Some(answer)
    }

    /// The copy waiting for the clipboard, if one is.
    pub fn pending(&self) -> Option<u64> {
        self.pending.as_ref().map(|pending| pending.id)
    }

    /// What the status line shows in place of what it would.
    pub fn note(&self) -> Option<&Line<'static>> {
        self.note.as_ref().map(|(line, _)| line)
    }
}

/// ` ✓ Copied 3 lines · 120 chars`; newlines count as characters.
fn copied(lines: usize, chars: usize) -> Line<'static> {
    let plural = |n: usize, word: &str| {
        if n == 1 {
            format!("{n} {word}")
        } else {
            format!("{n} {word}s")
        }
    };
    Line::from(vec![
        Span::styled(" ✓ ", theme::success()),
        Span::raw(format!(
            "Copied {} · {}",
            plural(lines, "line"),
            plural(chars, "char")
        )),
    ])
}

/// ` ✓ ` and `done`, such as `Link copied`.
fn link_copied(done: &'static str) -> Line<'static> {
    Line::from(vec![Span::styled(" ✓ ", theme::success()), Span::raw(done)])
}

fn failed(reason: &str) -> Line<'static> {
    let reason = sanitize(reason).replace('\n', " ");
    Line::from(Span::styled(
        format!(" ✗ Copy failed: {reason}"),
        theme::failure(),
    ))
}

/// Writes to this machine's clipboard. The clipboard stays open on the
/// thread that writes: on Linux, what was copied is gone once it closes.
fn native(text: &str) -> Result<(), String> {
    thread_local! {
        static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    }
    CLIPBOARD.with_borrow_mut(|clipboard| {
        let clipboard = match clipboard {
            Some(clipboard) => clipboard,
            None => clipboard.insert(arboard::Clipboard::new().map_err(|error| error.to_string())?),
        };
        clipboard.set_text(text).map_err(|error| error.to_string())
    })
}

/// OSC 52 that sets the clipboard to `text`; in tmux, wrapped so that tmux
/// passes it to the terminal.
fn osc52(text: &str, tmux: bool) -> Result<String, String> {
    if text.len() > OSC52_MAX {
        return Err(format!(
            "too long for the terminal ({} bytes, at most {OSC52_MAX})",
            text.len()
        ));
    }
    let encoded = base64(text.as_bytes());
    Ok(if tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    })
}

/// Standard base64 with padding (RFC 4648).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | u32::from(*byte) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn note_text(copier: &Copier) -> Option<String> {
        copier.note().map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
    }

    /// Polls until the note shows, at the same time `now`.
    fn wait(copier: &mut Copier, now: i64) -> String {
        for _ in 0..1000 {
            copier.poll(now);
            if let Some(text) = note_text(copier) {
                return text;
            }
            thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no answer from the clipboard thread");
    }

    #[test]
    fn base64_matches_the_rfc_examples() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (text, encoded) in cases {
            assert_eq!(base64(text.as_bytes()), encoded, "{text:?}");
        }
        assert_eq!(base64("中".as_bytes()), "5Lit");
    }

    #[test]
    fn osc52_is_wrapped_for_tmux_and_refused_when_too_long() {
        assert_eq!(osc52("hi", false).unwrap(), "\x1b]52;c;aGk=\x07");
        assert_eq!(
            osc52("hi", true).unwrap(),
            "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
        assert!(osc52(&"a".repeat(OSC52_MAX), false).is_ok());
        let error = osc52(&"a".repeat(OSC52_MAX + 1), false).unwrap_err();
        assert!(error.contains("too long"), "{error}");
    }

    #[test]
    fn a_copy_says_how_much_it_copied_and_the_note_goes() {
        let copied = Arc::new(Mutex::new(Vec::new()));
        let seen = copied.clone();
        let mut copier = Copier::with_writer(move |text| {
            seen.lock().unwrap().push(text.to_owned());
            Ok(())
        });
        copier.copy("one\ntwo".into());
        assert_eq!(copier.take_sequence(), None, "no terminal in tests");
        assert_eq!(wait(&mut copier, 100), " ✓ Copied 2 lines · 7 chars");
        assert_eq!(*copied.lock().unwrap(), ["one\ntwo"]);
        copier.poll(100 + COPIED_MS - 1);
        assert!(copier.note().is_some());
        copier.poll(100 + COPIED_MS);
        assert!(copier.note().is_none());

        copier.copy("x".into());
        assert_eq!(wait(&mut copier, 0), " ✓ Copied 1 line · 1 char");
    }

    #[test]
    fn in_tmux_or_over_ssh_the_terminal_gets_it_too() {
        let mut copier = Copier::new(Box::new(|_| Err("no display".into())), Some(true));
        copier.copy("hi".into());
        assert_eq!(
            copier.take_sequence().as_deref(),
            Some("\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\")
        );
        assert_eq!(copier.take_sequence(), None, "written once");
        copier.poll(0);
        assert_eq!(
            note_text(&copier).as_deref(),
            Some(" ✓ Copied 1 line · 2 chars")
        );

        let mut copier = Copier::new(Box::new(|_| Err("no display".into())), Some(false));
        copier.copy("a".repeat(OSC52_MAX + 1));
        assert_eq!(copier.take_sequence(), None);
        let note = wait(&mut copier, 0);
        assert!(
            note.starts_with(" ✗ Copy failed: no display; OSC 52: too long"),
            "{note}"
        );
    }

    #[test]
    fn a_clipboard_that_does_not_answer_fails_after_a_while() {
        let (unblock, blocked) = mpsc::channel::<()>();
        let mut copier = Copier::with_writer(move |_| {
            let _ = blocked.recv();
            Ok(())
        });
        copier.copy("text".into());
        copier.poll(1000);
        copier.poll(1000 + TIMEOUT_MS - 1);
        assert!(copier.note().is_none());
        copier.poll(1000 + TIMEOUT_MS);
        assert_eq!(
            note_text(&copier).as_deref(),
            Some(" ✗ Copy failed: the clipboard did not answer in 3s")
        );
        drop(unblock);
    }

    #[test]
    fn only_the_latest_copy_is_waited_for() {
        let (unblock, blocked) = mpsc::channel::<()>();
        let blocked = Mutex::new(blocked);
        let mut copier = Copier::with_writer(move |text| {
            if text == "first" {
                let _ = blocked.lock().unwrap().recv();
                return Err("first failed".into());
            }
            Ok(())
        });
        copier.copy("first".into());
        copier.copy("second".into());
        drop(unblock);
        assert_eq!(wait(&mut copier, 0), " ✓ Copied 1 line · 6 chars");
    }
}
