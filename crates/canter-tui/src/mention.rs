//! `@` file mentions in the input box: the `@` word at the cursor, the files
//! and directories under the session's directory (walked in the background,
//! by git's ignore rules), and which of them match what is typed after the
//! `@`. Only the path goes into the message; the model reads the file itself.

use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The most files and directories one walk takes; the rest are not listed.
pub(crate) const LIMIT: usize = 10_000;
/// Version control directories, never listed, in a git repository or not.
const SKIPPED: [&str; 4] = [".git", ".hg", ".svn", ".jj"];
/// Entries handed to the screen at a time.
const BATCH: usize = 256;

/// The `@` word the cursor is in: `@` at the start or after a blank, up to
/// the next blank, or `@"…"` up to the closing quote (or the end of the line).
#[derive(Debug, PartialEq)]
pub(crate) struct Word {
    /// Byte range in the input text, `@` included.
    pub range: Range<usize>,
    /// What follows the `@`, without the quotes.
    pub query: String,
}

/// The `@` word containing `cursor` (a byte offset), the cursor after its `@`.
pub(crate) fn word(text: &str, cursor: usize) -> Option<Word> {
    text[..cursor].match_indices('@').find_map(|(at, _)| {
        if !text[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
        {
            return None;
        }
        let rest = &text[at + 1..];
        let (end, query) = match rest.strip_prefix('"') {
            Some(quoted) => match quoted.find(['"', '\n']) {
                Some(i) if quoted[i..].starts_with('"') => (at + 2 + i + 1, &quoted[..i]),
                Some(i) => (at + 2 + i, &quoted[..i]),
                None => (text.len(), quoted),
            },
            None => {
                let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
                (at + 1 + len, &rest[..len])
            }
        };
        (cursor <= end).then(|| Word {
            range: at..end,
            query: query.to_owned(),
        })
    })
}

/// A file or directory under the walked directory.
pub(crate) struct Entry {
    /// The relative path with `/` between parts; a directory's ends with `/`.
    pub label: String,
    pub dir: bool,
    /// The label in lower case, for matching.
    lower: Vec<char>,
    /// Where the last part (the file name) starts in `lower`.
    name: usize,
}

impl Entry {
    fn new(root: &Path, entry: &ignore::DirEntry) -> Option<Self> {
        let path = entry.path().strip_prefix(root).ok()?.to_str()?;
        // Such a name could not be written in the input box as it is.
        if path.is_empty() || path.contains(|c: char| c == '"' || c.is_control()) {
            return None;
        }
        let dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        let label = if dir {
            format!("{path}/")
        } else {
            path.to_owned()
        };
        let lower: Vec<char> = label.chars().flat_map(char::to_lowercase).collect();
        let name = lower[..lower.len() - usize::from(dir)]
            .iter()
            .rposition(|&c| c == '/')
            .map_or(0, |slash| slash + 1);
        Some(Self {
            label,
            dir,
            lower,
            name,
        })
    }

    /// How well it matches `query` (in lower case); `None` when it does not.
    fn rank(&self, query: &[char]) -> Option<Rank> {
        // The directory just completed: what is inside it is wanted.
        if self.dir && self.lower == query {
            return None;
        }
        let name = &self.lower[self.name..self.lower.len() - usize::from(self.dir)];
        let (outside_name, pieces) = match pieces(name, query) {
            Some(pieces) => (false, pieces),
            None => (true, pieces(&self.lower, query)?),
        };
        Some(Rank {
            outside_name,
            pieces,
            not_name_start: !name.starts_with(query),
            length: self.lower.len(),
        })
    }

    /// What replaces the `@` word: `@path ` for a file, `@path/` for a
    /// directory, so the list goes on inside it; a path with blanks is in
    /// double quotes. Also where the cursor goes, as a byte offset into it.
    pub fn completion(&self) -> (String, usize) {
        let quoted = self.label.contains(char::is_whitespace);
        let text = match (quoted, self.dir) {
            (false, false) => format!("@{} ", self.label),
            (false, true) => format!("@{}", self.label),
            (true, false) => format!("@\"{}\" ", self.label),
            (true, true) => format!("@\"{}\"", self.label),
        };
        // Inside the quotes of a directory, so typing goes on in its path.
        let cursor = text.len() - usize::from(quoted && self.dir);
        (text, cursor)
    }
}

/// Lower ranks come first.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// The query is not all in the file name.
    outside_name: bool,
    /// The fewest runs of consecutive letters the query matches in.
    pieces: usize,
    /// The file name does not start with the query.
    not_name_start: bool,
    length: usize,
}

/// The fewest runs of consecutive characters of `hay` that `needle` matches
/// in order; `None` when it does not match.
fn pieces(hay: &[char], needle: &[char]) -> Option<usize> {
    let Some((&first, rest)) = needle.split_first() else {
        return Some(0);
    };
    // best[i]: the fewest runs with the needle so far ending at hay[i].
    let mut best: Vec<Option<usize>> = hay.iter().map(|&c| (c == first).then_some(1)).collect();
    for &c in rest {
        let mut before: Option<usize> = None;
        let mut next = vec![None; hay.len()];
        for i in 1..hay.len() {
            if i >= 2 {
                before = min(before, best[i - 2]);
            }
            if hay[i] == c {
                next[i] = min(best[i - 1], before.map(|runs| runs + 1));
            }
        }
        best = next;
    }
    best.into_iter().flatten().min()
}

fn min(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The indices of the entries matching `query`, ignoring case, best first:
/// matches within the file name, then fewer runs, then a name starting with
/// the query, then shorter paths, then by path.
pub(crate) fn matching(entries: &[Entry], query: &str) -> Vec<usize> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut found: Vec<(Rank, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.rank(&query).map(|rank| (rank, index)))
        .collect();
    found.sort_by(|(a, i), (b, j)| {
        a.cmp(b)
            .then_with(|| entries[*i].label.cmp(&entries[*j].label))
    });
    found.into_iter().map(|(_, index)| index).collect()
}

#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    truncated: bool,
}

/// A walk of one directory in the background, and what it found so far.
/// Dropping it stops the walk.
pub(crate) struct Scan {
    pub root: String,
    pub entries: Vec<Entry>,
    /// The walk stopped at [`LIMIT`].
    pub truncated: bool,
    found: Arc<Mutex<Found>>,
    stop: Arc<AtomicBool>,
}

impl Scan {
    pub fn start(root: &str) -> Self {
        let found = Arc::new(Mutex::new(Found::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (into, stopped, dir) = (found.clone(), stop.clone(), root.to_owned());
        std::thread::spawn(move || walk(Path::new(&dir), &into, &stopped));
        Self {
            root: root.to_owned(),
            entries: Vec::new(),
            truncated: false,
            found,
            stop,
        }
    }

    /// Takes what the walk found since the last call.
    pub fn poll(&mut self) {
        let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        self.entries.append(&mut found.entries);
        self.truncated = found.truncated;
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Walks `root` by `.gitignore`, `.git/info/exclude` and the global git
/// ignore file (inside a git repository; outside one, by none of them),
/// in name order, handing what it finds to `found` in batches.
fn walk(root: &Path, found: &Mutex<Found>, stop: &AtomicBool) {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| SKIPPED.contains(&name))
        })
        .build();
    let hand = |batch: &mut Vec<Entry>, truncated: bool| {
        let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
        found.entries.append(batch);
        found.truncated = truncated;
    };
    let mut batch = Vec::new();
    let mut count = 0;
    for entry in walker {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Some(entry) = entry.ok().and_then(|entry| Entry::new(root, &entry)) else {
            continue;
        };
        if count == LIMIT {
            hand(&mut batch, true);
            return;
        }
        batch.push(entry);
        count += 1;
        if batch.len() == BATCH {
            hand(&mut batch, false);
        }
    }
    hand(&mut batch, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_at_the_cursor() {
        let at = |text: &str| {
            let cursor = text.find('|').unwrap();
            let text = text.replace('|', "");
            word(&text, cursor).map(|word| (word.range, word.query))
        };
        assert_eq!(at("@|"), Some((0..1, String::new())));
        assert_eq!(at("see @sr|c now"), Some((4..8, "src".into())));
        assert_eq!(at("see @src |now"), None);
        assert_eq!(at("|@src"), None);
        assert_eq!(at("mail a@b|"), None);
        assert_eq!(at("@\"my d|ir/\""), Some((0..10, "my dir/".into())));
        assert_eq!(at("@\"my notes.txt\" |"), None);
    }

    #[test]
    fn pieces_counts_runs() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(pieces(&chars("mapper"), &chars("app")), Some(1));
        assert_eq!(pieces(&chars("a_p_p"), &chars("app")), Some(3));
        assert_eq!(pieces(&chars("ap_app"), &chars("app")), Some(1));
        assert_eq!(pieces(&chars("pa"), &chars("ap")), None);
    }
}
