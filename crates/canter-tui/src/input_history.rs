//! What was sent from the input box, for ↑ and ↓ in the empty input box.
//! Kept in `input_history.jsonl` beside `tui.json`: one JSON line a message
//! with the directory, the text and the time, readable only by the user.
//! Only the terminal reads and writes it, never the server; it is read when
//! canter opens. Sent from this terminal, a message comes back as it was,
//! folded pastes and images too; read from the file, as text only.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::input::InputBox;

/// The file keeps the newest this many lines.
pub(crate) const MAX_ENTRIES: usize = 2000;

const UNREADABLE: &str = "Could not read the input history; it starts empty";
const DAMAGED: &str = "Skipped damaged lines in the input history";
const UNSAVED: &str = "Could not save the input history";

#[derive(Debug, Default)]
pub(crate) struct InputHistory {
    /// `None` without a home directory: nothing is read or saved.
    file: Option<PathBuf>,
    /// Oldest first.
    entries: Vec<Entry>,
    /// While browsing: the entry recalled and the text it put in the box.
    browsing: Option<(usize, String)>,
    /// Said once in the status line.
    warning: Option<&'static str>,
}

#[derive(Debug)]
struct Entry {
    cwd: String,
    /// The text as the input box showed it, with paste and image markers;
    /// for entries from the file, the text put back in the box.
    shown: String,
    /// Sent from this terminal: the draft with its pastes and images.
    draft: Option<InputBox>,
}

/// One line of the file.
#[derive(Serialize, Deserialize)]
struct Line {
    cwd: String,
    text: String,
    /// Unix seconds.
    ts: i64,
}

impl InputHistory {
    /// Reads `input_history.jsonl` in `CANTER_HOME`, or else in `~/.canter`.
    /// Damaged lines are skipped and a file that cannot be read starts empty;
    /// either is said once.
    pub fn load(env: &BTreeMap<String, String>) -> Self {
        let file = crate::statusline::home(env).map(|home| home.join("input_history.jsonl"));
        let mut history = Self {
            file,
            ..Self::default()
        };
        let Some(path) = &history.file else {
            return history;
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return history,
            Err(_) => {
                history.warning = Some(UNREADABLE);
                return history;
            }
        };
        for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            let parsed = std::str::from_utf8(line)
                .ok()
                .and_then(|line| serde_json::from_str::<Line>(line).ok());
            match parsed {
                Some(line) if !line.text.is_empty() => history.entries.push(Entry {
                    cwd: line.cwd,
                    shown: line.text,
                    draft: None,
                }),
                Some(_) => {}
                None => history.warning = Some(DAMAGED),
            }
        }
        let extra = history.entries.len().saturating_sub(MAX_ENTRIES);
        history.entries.drain(..extra);
        history
    }

    /// What to say once in the status line, if anything.
    pub fn take_warning(&mut self) -> Option<&'static str> {
        self.warning.take()
    }

    /// Stops browsing: the next ↑ in the empty box starts from the newest.
    pub fn stop_browsing(&mut self) {
        self.browsing = None;
    }

    /// Keeps `draft`, sent from the input box in `cwd`, and appends its text
    /// to the file: folded pastes in full, image markers left out. One the
    /// same as the last kept in `cwd` is not kept again.
    pub fn record(&mut self, cwd: &str, draft: &InputBox, now_secs: i64) {
        self.browsing = None;
        let shown = draft.text();
        if shown.trim().is_empty()
            || self
                .entries
                .iter()
                .rev()
                .find(|entry| entry.cwd == cwd)
                .is_some_and(|last| last.shown == shown)
        {
            return;
        }
        self.entries.push(Entry {
            cwd: cwd.to_owned(),
            shown: shown.to_owned(),
            draft: Some(draft.clone()),
        });
        let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..extra);
        let text = draft.text_only();
        let Some(path) = &self.file else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        let line = Line {
            cwd: cwd.to_owned(),
            text,
            ts: now_secs,
        };
        if append(path, &line).is_err() && self.warning.is_none() {
            self.warning = Some(UNSAVED);
        }
    }

    /// ↑: the message before the one shown, or the newest in `cwd` when the
    /// box is empty. False when ↑ is not for the history: the box has
    /// something else in it, a recalled message was changed or the cursor
    /// is inside it, or there is nothing to recall.
    pub fn up(&mut self, cwd: &str, input: &mut InputBox) -> bool {
        let Some(at) = self.navigating(input) else {
            return false;
        };
        match self.older(cwd, at) {
            Some(i) => {
                self.show(i, input);
                true
            }
            // At the oldest it stays.
            None => at.is_some(),
        }
    }

    /// ↓: the message after the one shown; past the newest, the box empties.
    /// False when not browsing, as for [`Self::up`].
    pub fn down(&mut self, cwd: &str, input: &mut InputBox) -> bool {
        let Some(Some(at)) = self.navigating(input) else {
            return false;
        };
        match self.newer(cwd, at) {
            Some(i) => self.show(i, input),
            None => {
                input.clear();
                self.browsing = None;
            }
        }
        true
    }

    /// Whether the box shows a recalled message, unchanged: the `/` and `@`
    /// lists stay closed so that ↑ and ↓ go on through the history.
    pub fn showing(&self, input: &InputBox) -> bool {
        self.browsing
            .as_ref()
            .is_some_and(|(_, text)| input.text() == text)
    }

    /// `Some(Some(i))` while entry `i` is shown unchanged with the cursor at
    /// its start or end; `Some(None)` in the empty box; otherwise `None`.
    fn navigating(&self, input: &InputBox) -> Option<Option<usize>> {
        if let Some((i, text)) = &self.browsing
            && input.text() == text
            && (input.cursor() == 0 || input.cursor() == text.len())
        {
            return Some(Some(*i));
        }
        input.is_empty().then_some(None)
    }

    /// The entry in `cwd` before `at` (or the newest), past copies of the
    /// one at `at`.
    fn older(&self, cwd: &str, at: Option<usize>) -> Option<usize> {
        let end = at.unwrap_or(self.entries.len());
        let same = at.map(|at| self.entries[at].shown.as_str());
        (0..end).rev().find(|&i| {
            let entry = &self.entries[i];
            entry.cwd == cwd && Some(entry.shown.as_str()) != same
        })
    }

    /// The entry in `cwd` after `at`, past copies of it.
    fn newer(&self, cwd: &str, at: usize) -> Option<usize> {
        let same = self.entries[at].shown.as_str();
        (at + 1..self.entries.len()).find(|&i| {
            let entry = &self.entries[i];
            entry.cwd == cwd && entry.shown != same
        })
    }

    fn show(&mut self, i: usize, input: &mut InputBox) {
        let entry = &self.entries[i];
        match &entry.draft {
            Some(draft) => input.recall(draft),
            None => {
                input.clear();
                input.insert(&entry.shown);
            }
        }
        self.browsing = Some((i, input.text().to_owned()));
    }
}

/// Adds `line` to the file, then keeps only the newest [`MAX_ENTRIES`]
/// lines. Terminals take turns through a lock on a file beside it; the
/// file is rewritten into a new file that then takes its name, so it is
/// never half written.
fn append(path: &Path, line: &Line) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let lock = private_file(&dir.join("input_history.lock"), false)?;
    lock.lock()?;
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    let old = match std::fs::read(path) {
        Ok(old) => old,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let lines: Vec<&[u8]> = old
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() < MAX_ENTRIES {
        let mut file = private_file(path, true)?;
        // A line cut short (canter stopped mid-write) stays on its own.
        if old.last().is_some_and(|b| *b != b'\n') {
            text.insert(0, '\n');
        }
        return file.write_all(text.as_bytes());
    }
    let mut kept = Vec::with_capacity(old.len() + text.len());
    for line in &lines[lines.len() + 1 - MAX_ENTRIES..] {
        kept.extend_from_slice(line);
        kept.push(b'\n');
    }
    kept.extend_from_slice(text.as_bytes());
    let temp = dir.join(format!(".input_history.{}.tmp", std::process::id()));
    let saved = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&kept)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved
}

/// Opens (making it if needed) a file readable only by the user, for
/// appending or as a lock.
fn private_file(path: &Path, append: bool) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(!append)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([(
            "CANTER_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        )])
    }

    fn typed(text: &str) -> InputBox {
        let mut input = InputBox::default();
        input.insert(text);
        input
    }

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn the_file_is_private_and_each_line_has_directory_text_and_time() {
        let home = tempfile::tempdir().unwrap();
        let mut history = InputHistory::load(&env(home.path()));
        history.record("/work", &typed("hello"), 1_700_000_000);
        let path = home.path().join("input_history.jsonl");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            lines(&path),
            [serde_json::json!({"cwd": "/work", "text": "hello", "ts": 1_700_000_000})]
        );
        assert!(history.take_warning().is_none());
    }

    #[test]
    fn damaged_lines_are_skipped_and_said_once() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("input_history.jsonl");
        let mut bytes = br#"{"cwd":"/work","text":"first","ts":1}"#.to_vec();
        bytes.extend_from_slice(b"\nnot json\n\xff\xfe\n{\"cwd\":\"/work\"}\n");
        bytes.extend_from_slice(br#"{"cwd":"/work","text":"second","ts":2}"#);
        bytes.extend_from_slice(b"\n{\"cwd\":\"/work\",\"text\":\"cut sh");
        std::fs::write(&path, bytes).unwrap();
        let mut history = InputHistory::load(&env(home.path()));
        assert_eq!(history.take_warning(), Some(DAMAGED));
        assert_eq!(history.take_warning(), None);
        let mut input = InputBox::default();
        assert!(history.up("/work", &mut input));
        assert_eq!(input.text(), "second");
        assert!(history.up("/work", &mut input));
        assert_eq!(input.text(), "first");

        // A new line after the cut one is read whole.
        history.record("/work", &typed("third"), 3);
        let history = InputHistory::load(&env(home.path()));
        assert_eq!(history.entries.last().unwrap().shown, "third");
    }

    #[test]
    fn a_file_that_cannot_be_read_starts_empty_and_is_said() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("input_history.jsonl")).unwrap();
        let mut history = InputHistory::load(&env(home.path()));
        assert_eq!(history.take_warning(), Some(UNREADABLE));
        let mut input = InputBox::default();
        assert!(!history.up("/work", &mut input));
        // Sending still works; saving fails and is said.
        history.record("/work", &typed("hello"), 1);
        assert_eq!(history.take_warning(), Some(UNSAVED));
        assert!(history.up("/work", &mut input));
        assert_eq!(input.text(), "hello");
    }

    #[test]
    fn over_two_thousand_the_oldest_go() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("input_history.jsonl");
        let old: String = (0..MAX_ENTRIES)
            .map(|i| format!("{{\"cwd\":\"/work\",\"text\":\"m{i}\",\"ts\":{i}}}\n"))
            .collect();
        std::fs::write(&path, old).unwrap();
        let mut history = InputHistory::load(&env(home.path()));
        history.record("/work", &typed("newest"), 9);
        let saved = lines(&path);
        assert_eq!(saved.len(), MAX_ENTRIES);
        assert_eq!(saved[0]["text"], "m1");
        assert_eq!(saved[MAX_ENTRIES - 1]["text"], "newest");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(history.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn terminals_appending_at_once_do_not_spoil_the_file() {
        let home = tempfile::tempdir().unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let env = env(home.path());
                std::thread::spawn(move || {
                    let mut history = InputHistory::load(&env);
                    for i in 0..50 {
                        let text = format!("terminal {t} message {i} {}", "x".repeat(500));
                        history.record("/work", &typed(&text), i);
                    }
                    assert!(history.take_warning().is_none());
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let saved = lines(&home.path().join("input_history.jsonl"));
        assert_eq!(saved.len(), 400);
        for t in 0..8 {
            let mine: Vec<_> = saved
                .iter()
                .filter_map(|line| line["text"].as_str())
                .filter(|text| text.starts_with(&format!("terminal {t} ")))
                .collect();
            assert_eq!(mine.len(), 50);
            assert!(mine[49].starts_with(&format!("terminal {t} message 49 ")));
        }
    }

    #[test]
    fn only_this_directory_newest_first_without_repeats_in_a_row() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("input_history.jsonl");
        let old = [
            ("/work", "a"),
            ("/other", "elsewhere"),
            ("/work", "b"),
            ("/work", "b"),
            ("/work", "c"),
        ]
        .map(|(cwd, text)| format!("{{\"cwd\":\"{cwd}\",\"text\":\"{text}\",\"ts\":1}}\n"))
        .concat();
        std::fs::write(&path, old).unwrap();
        let mut history = InputHistory::load(&env(home.path()));
        // The same as the last one in this directory is not kept again.
        history.record("/work", &typed("c"), 2);
        assert_eq!(lines(&path).len(), 5);

        let mut input = InputBox::default();
        let mut seen = Vec::new();
        while history.up("/work", &mut input) && seen.last() != Some(&input.text().to_owned()) {
            seen.push(input.text().to_owned());
        }
        assert_eq!(seen, ["c", "b", "a"]);
        assert!(history.down("/work", &mut input));
        assert_eq!(input.text(), "b");
        assert!(history.down("/work", &mut input));
        assert_eq!(input.text(), "c");
        // Past the newest the box empties and ↓ goes back to the box.
        assert!(history.down("/work", &mut input));
        assert!(input.is_empty());
        assert!(!history.down("/work", &mut input));

        let mut input = InputBox::default();
        assert!(!history.up("/nowhere", &mut input));
    }

    #[test]
    fn without_a_home_nothing_is_saved() {
        let mut history = InputHistory::load(&BTreeMap::new());
        history.record("/work", &typed("hello"), 1);
        assert!(history.take_warning().is_none());
        let mut input = InputBox::default();
        assert!(history.up("/work", &mut input));
        assert_eq!(input.text(), "hello");
    }
}
