use crate::approval::shorten_home;
use crate::git::Status;
use crate::report::{Part, Report, clean};
use crate::text::str_width;
use crate::theme;
use canter_protocol::{Item, ItemStatus, PatchChangeKind, SessionSnapshot};
use ratatui::style::Style;
use ratatui::text::Span;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub(crate) fn display_path(path: &Path, cwd: &str, home: Option<&str>) -> String {
    match path.strip_prefix(cwd) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => shorten_home(&path.to_string_lossy(), home),
    }
}

/// `/files`: what the session read, what canter changed and git's status,
/// each a section.
pub(crate) async fn popup(snapshot: &SessionSnapshot, home: Option<&str>) -> Report {
    let cwd = &snapshot.settings.cwd;
    let mut read = Vec::new();
    let mut seen = HashSet::new();
    let mut changed: Vec<(PathBuf, Vec<&str>)> = Vec::new();
    let mut positions = HashMap::new();
    for item in snapshot.turns.iter().flat_map(|turn| &turn.items) {
        match item {
            Item::FileRead {
                path,
                status: ItemStatus::Completed,
                ..
            } => {
                let path = Path::new(cwd).join(path);
                if seen.insert(path.clone()) {
                    read.push(path);
                }
            }
            Item::FileChange {
                changes,
                status: ItemStatus::Completed,
                ..
            } => {
                for change in changes {
                    let path = Path::new(cwd).join(&change.path);
                    let kind = match change.kind {
                        PatchChangeKind::Add => "added",
                        PatchChangeKind::Update { .. } => "modified",
                        PatchChangeKind::Delete => "deleted",
                        PatchChangeKind::Unknown => "unknown",
                    };
                    let index = *positions.entry(path.clone()).or_insert_with(|| {
                        changed.push((path, Vec::new()));
                        changed.len() - 1
                    });
                    if !changed[index].1.contains(&kind) {
                        changed[index].1.push(kind);
                    }
                }
            }
            _ => {}
        }
    }
    let git = crate::git::status(cwd, home).await;
    let git_count = match &git {
        Status::Repository { entries, .. } => entries.len(),
        Status::Unavailable(_) => 0,
    };
    let title = vec![
        Span::styled(" Files", Style::new().bold()),
        Span::styled(
            format!(
                "  read {} · changed {} · git {git_count}",
                read.len(),
                changed.len()
            ),
            theme::secondary(),
        ),
    ];
    let mut body = vec![heading("Read", None)];
    if read.is_empty() {
        body.push(none("(none)"));
    }
    body.extend(read.iter().map(|path| {
        Part::Line(vec![Span::raw(format!(
            "   {}",
            clean(&display_path(path, cwd, home))
        ))])
    }));
    body.push(Part::blank());
    body.push(heading("Changed by canter", None));
    if changed.is_empty() {
        body.push(none("(none)"));
    }
    let kinds_width = changed
        .iter()
        .map(|(_, kinds)| str_width(&kinds.join(" / ")))
        .max()
        .unwrap_or(0);
    for (path, kinds) in &changed {
        let mut spans = vec![Span::raw("   ")];
        for (index, kind) in kinds.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" / "));
            }
            spans.push(Span::styled(*kind, word_style(kind)));
        }
        let pad = kinds_width - str_width(&kinds.join(" / ")) + 3;
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(clean(&display_path(path, cwd, home))));
        body.push(Part::Line(spans));
    }
    body.push(Part::blank());
    match git {
        Status::Unavailable(why) => {
            body.push(heading("Git status", None));
            body.push(none(&format!("({why})")));
        }
        Status::Repository { branch, entries } => {
            body.push(heading("Git status", Some(&clean(&branch))));
            if entries.is_empty() {
                body.push(none("(none)"));
            }
            let rows: Vec<_> = entries
                .iter()
                .map(|(code, path)| (git_word(code), clean(path)))
                .collect();
            let word_width = rows
                .iter()
                .map(|((word, ..), _)| str_width(word))
                .max()
                .unwrap_or(0);
            let path_width = rows
                .iter()
                .filter(|((.., mark), _)| mark.is_some())
                .map(|(_, path)| str_width(path))
                .max()
                .unwrap_or(0);
            for ((word, style, mark), path) in rows {
                let mut spans = vec![
                    Span::raw("   "),
                    Span::styled(word.clone(), style),
                    Span::raw(" ".repeat(word_width - str_width(&word) + 3)),
                ];
                match mark {
                    Some(mark) => {
                        let pad = path_width - str_width(&path) + 3;
                        spans.push(Span::raw(format!("{path}{}", " ".repeat(pad))));
                        spans.push(Span::styled(mark, theme::secondary()));
                    }
                    None => spans.push(Span::raw(path)),
                }
                body.push(Part::Line(spans));
            }
        }
    }
    Report::new(
        title,
        body,
        vec![vec![Span::styled(
            " Edits made through shell commands only show up under Git status.",
            theme::secondary(),
        )]],
    )
}

/// A section's first row, in bold, and after it what it is about in grey.
pub(crate) fn heading(name: &str, about: Option<&str>) -> Part {
    let mut spans = vec![Span::styled(format!(" {name}"), Style::new().bold())];
    if let Some(about) = about {
        spans.push(Span::styled(format!(" · {about}"), theme::secondary()));
    }
    Part::Line(spans)
}

/// An empty section's row.
pub(crate) fn none(text: &str) -> Part {
    Part::Line(vec![Span::styled(format!("   {text}"), theme::secondary())])
}

fn word_style(word: &str) -> Style {
    match word {
        "modified" => theme::warning(),
        "added" => theme::success(),
        "deleted" => theme::failure(),
        "conflict" => theme::failure().bold(),
        "renamed" | "untracked" => theme::secondary(),
        _ => Style::new(),
    }
}

/// A git status code (`XY`, X the index, Y the work tree) as a word, how
/// it is drawn, and whether it is staged: only in the index, or in the
/// index and changed again since.
fn git_word(code: &str) -> (String, Style, Option<&'static str>) {
    let mut chars = code.chars();
    let (x, y) = (chars.next().unwrap_or(' '), chars.next().unwrap_or(' '));
    let (word, mark) = if code == "??" {
        ("untracked".to_owned(), None)
    } else if x == 'U' || y == 'U' || code == "AA" || code == "DD" {
        ("conflict".to_owned(), None)
    } else if x == ' ' {
        (letter_word(y), None)
    } else if y == ' ' {
        (letter_word(x), Some("staged"))
    } else {
        (letter_word(x), Some("partly staged"))
    };
    let style = word_style(&word);
    (word, style, mark)
}

fn letter_word(letter: char) -> String {
    match letter {
        'M' | 'T' => "modified".into(),
        'A' => "added".into(),
        'D' => "deleted".into(),
        'R' => "renamed".into(),
        other => other.to_string(),
    }
}
