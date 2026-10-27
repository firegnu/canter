//! `/diff`: the uncommitted changes of the session's repository in a box
//! above the input box: the staged and the unstaged diff, each file drawn as
//! the conversation draws a diff, then the untracked files by name. git is
//! run as `/review` runs it ([`crate::git::local`]), with the same limits.

use std::time::Duration;

use canter_protocol::PatchChangeKind;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::diff::Diff;
use crate::files::{heading, none};
use crate::git::{LIMIT, LIST_LIMIT, Local, prefix};
use crate::report::{Part, Report, clean};
use crate::text::sanitize;
use crate::theme;

/// Where a diff's rows start in the box: under its file's path.
const DIFF_INDENT: &str = "     ";

/// What `/diff` found, for [`crate::App::show_diff`].
pub struct Changes(Result<Local, String>);

/// The uncommitted changes of the repository holding `cwd`, collected in at
/// most 15 seconds.
pub async fn collect(cwd: &str) -> Changes {
    let local = tokio::time::timeout(Duration::from_secs(15), crate::git::local(cwd))
        .await
        .unwrap_or_else(|_| Err("collecting changes timed out".into()));
    Changes(local)
}

/// The box for `changes`: a section for each of staged, unstaged and
/// untracked that has something in it, or why there is nothing to show.
pub(crate) fn popup(changes: Changes) -> Report {
    let title = vec![Span::styled(" Diff", Style::new().bold())];
    let local = match changes.0 {
        Ok(local) => local,
        Err(error) if error.contains("not a git repository") => {
            return Report::new(title, vec![none("Not a git repository")], Vec::new());
        }
        Err(error) => {
            let error = Part::Wrap {
                indent: 3,
                text: sanitize(&error),
                style: theme::failure(),
                rows: 10,
            };
            return Report::new(title, vec![error], Vec::new());
        }
    };
    let (staged, unstaged, untracked) = (&local.staged, &local.unstaged, &local.untracked);
    if staged.text.is_empty() && unstaged.text.is_empty() && untracked.text.is_empty() {
        return Report::new(title, vec![none("No uncommitted changes")], Vec::new());
    }
    // The diffs share one allowance and the list has its own, as for /review.
    let truncated =
        staged.truncated || unstaged.truncated || staged.text.len() + unstaged.text.len() > LIMIT;
    let list_truncated = untracked.truncated || untracked.text.len() > LIST_LIMIT;
    let staged = prefix(&staged.text, LIMIT);
    let unstaged = prefix(&unstaged.text, LIMIT - staged.len());
    let untracked = prefix(&untracked.text, LIST_LIMIT);

    let mut body = Vec::new();
    for (name, text) in [("Staged", staged), ("Unstaged", unstaged)] {
        if text.is_empty() {
            continue;
        }
        if !body.is_empty() {
            body.push(Part::blank());
        }
        body.push(heading(name, None));
        for file in files(text) {
            file_parts(&file, &mut body);
        }
    }
    if !untracked.is_empty() {
        if !body.is_empty() {
            body.push(Part::blank());
        }
        body.push(heading("Untracked", None));
        body.extend(
            untracked
                .lines()
                .map(|path| Part::Line(vec![Span::raw(format!("   {}", clean(path)))])),
        );
    }
    let mut notes = Vec::new();
    if truncated {
        notes.push(vec![Span::styled(
            format!(
                " Truncated at {} KiB; run git diff for the rest",
                LIMIT / 1024
            ),
            theme::warning(),
        )]);
    }
    if list_truncated {
        notes.push(vec![Span::styled(
            format!(
                " Untracked files truncated at {} KiB; run git status for the rest",
                LIST_LIMIT / 1024
            ),
            theme::warning(),
        )]);
    }
    Report::new(title, body, notes)
}

/// One file's part of a `git diff`.
struct File<'a> {
    path: String,
    kind: PatchChangeKind,
    text: &'a str,
}

/// Splits a `git diff` into its files, at each `diff --git` line.
fn files(text: &str) -> Vec<File<'_>> {
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(at);
        }
        at += line.len();
    }
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let ends = starts.iter().skip(1).copied().chain([text.len()]);
    starts
        .iter()
        .zip(ends)
        .map(|(&start, end)| file(&text[start..end]))
        .filter(|file| !file.text.trim().is_empty())
        .collect()
}

/// A file's path (the new one, or the old one when deleted) and what
/// happened to it, from the headers before its first hunk.
fn file(text: &str) -> File<'_> {
    let headers = text.lines().take_while(|line| !line.starts_with("@@"));
    let (mut path, mut old, mut kind) = (None, None, PatchChangeKind::Update { move_path: None });
    for line in headers {
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = rest.strip_prefix("b/").map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old = rest.strip_prefix("a/").map(str::to_owned);
        } else if line.starts_with("new file mode") {
            kind = PatchChangeKind::Add;
        } else if line.starts_with("deleted file mode") {
            kind = PatchChangeKind::Delete;
        } else if let Some(rest) = line.strip_prefix("diff --git a/") {
            old = old.or_else(|| rest.split_once(" b/").map(|(a, _)| a.to_owned()));
        }
    }
    File {
        path: path.or(old).unwrap_or_default(),
        kind,
        text,
    }
}

/// `path  +a −r` and the file's diff; a file without hunks (binary, or only
/// its mode changed) shows what git says of it instead.
fn file_parts(file: &File, body: &mut Vec<Part>) {
    let mut title = vec![Span::raw(format!("   {}", clean(&file.path)))];
    if !file.text.lines().any(|line| line.starts_with("@@")) {
        body.push(Part::Line(title));
        body.extend(
            file.text
                .lines()
                .filter(|line| !line.starts_with("diff --git ") && !line.starts_with("index "))
                .map(|line| {
                    Part::Line(vec![Span::styled(
                        format!("{DIFF_INDENT}{}", clean(line)),
                        theme::secondary(),
                    )])
                }),
        );
        return;
    }
    let diff = Diff::parse(&sanitize(file.text), &file.kind, &file.path);
    let (added, removed) = (diff.added(), diff.removed());
    title.push(Span::raw("  "));
    title.push(Span::styled(format!("+{added}"), theme::success()));
    if removed > 0 {
        title.push(Span::raw(" "));
        title.push(Span::styled(format!("−{removed}"), theme::failure()));
    }
    let word = match file.kind {
        PatchChangeKind::Add => Some("new"),
        PatchChangeKind::Delete => Some("deleted"),
        _ => None,
    };
    if let Some(word) = word {
        title.push(Span::styled(format!("  {word}"), theme::secondary()));
    }
    body.push(Part::Line(title));
    body.push(Part::Diff {
        indent: DIFF_INDENT,
        diff,
    });
}
