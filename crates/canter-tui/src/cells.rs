//! Rows for each kind of item: the user's prompt, the model's text, thinking,
//! and the tool cards (read a file, change files with a diff, run a command).

use canter_protocol::{
    FileUpdateChange, Item, ItemStatus, PatchChangeKind, Turn, TurnStatus, UserInput,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown;
use crate::text::{LogicalLine, sanitize, spans_width, truncate, visible, wrap};
use crate::theme;

/// Output lines a finished command keeps.
const FINISHED_TAIL: usize = 3;
/// Output lines shown while a command runs.
const LIVE_TAIL: usize = 5;
const GUTTER: &str = "    │ ";

/// A blank row, `› ` and the text, then another blank row. `source` names the
/// client that sent it when that was not this terminal.
pub(crate) fn prompt(input: &[UserInput], source: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let mut image_number = 0;
    let texts: Vec<String> = input
        .iter()
        .filter_map(|part| match part {
            UserInput::Text { text } => Some(text.clone()),
            UserInput::LocalImage { name, .. } => {
                image_number += 1;
                Some(format!(
                    "[Image #{image_number} {}]",
                    name.as_deref().unwrap_or("image")
                ))
            }
            UserInput::Unknown => None,
        })
        .collect();
    if texts.is_empty() {
        return Vec::new();
    }
    let text = sanitize(&texts.join("\n"));
    let mut rows = vec![Line::default()];
    for (i, line) in text.split('\n').enumerate() {
        let indent = if i == 0 {
            Span::styled("› ", theme::accent().bold())
        } else {
            Span::raw("  ")
        };
        let mut body = vec![Span::raw(line.to_owned())];
        if let Some(source) = source.filter(|_| i == 0) {
            body.push(Span::styled(
                format!("  (from {})", sanitize(source)),
                theme::secondary(),
            ));
        }
        rows.extend(wrap(&LogicalLine::new(vec![indent], body), width));
    }
    rows.push(Line::default());
    rows
}

/// The model's text as markdown, indented two columns.
pub(crate) fn agent_message(text: &str) -> Vec<LogicalLine> {
    markdown::render(&sanitize(text))
        .into_iter()
        .map(|line| line.prefixed(Span::raw("  "), Span::raw("  ")))
        .collect()
}

/// Rows for an item; `finished` picks its final form over the in-progress one.
pub(crate) fn item(item: &Item, finished: bool, cwd: &str, width: u16) -> Vec<Line<'static>> {
    match item {
        Item::UserMessage { content, .. } => prompt(content, None, width),
        Item::AgentMessage { text, .. } => wrap_all(&agent_message(text), width),
        Item::Reasoning {
            summary, content, ..
        } => reasoning(summary, content, finished, width),
        Item::WebFetch {
            url,
            resolved_url,
            result,
            status,
            ..
        } => command_card(
            &format!(
                "web_fetch {} ({} bytes)",
                resolved_url.as_deref().unwrap_or(url),
                result.len()
            ),
            *status,
            None,
            None,
            result,
            width,
        ),
        Item::McpToolCall {
            server,
            tool,
            status,
            result,
            ..
        } => command_card(
            &format!("MCP {server} / {tool}"),
            *status,
            None,
            None,
            result,
            width,
        ),
        Item::CommandExecution {
            command,
            status,
            aggregated_output,
            exit_code,
            duration_ms,
            ..
        } => command_card(
            command,
            *status,
            *exit_code,
            *duration_ms,
            aggregated_output,
            width,
        ),
        Item::FileChange {
            changes, status, ..
        } => changes
            .iter()
            .flat_map(|change| file_change(change, *status, cwd, width))
            .collect(),
        Item::FileRead {
            path,
            start_line,
            content,
            status,
            ..
        } => vec![file_read(path, *start_line, content, *status, cwd, width)],
        Item::ContextCompaction { .. } => {
            vec![Line::from(Span::styled(
                "  ── Context compacted ──",
                theme::secondary(),
            ))]
        }
        Item::UserInput {
            question,
            answer,
            status,
            ..
        } => {
            let answer = match answer {
                Some(canter_protocol::UserInputAnswer::Option { label }) => label.clone(),
                Some(canter_protocol::UserInputAnswer::Text { text }) => {
                    format!("Own answer: {text}")
                }
                _ if *status == ItemStatus::InProgress => "Waiting for your answer".into(),
                _ => "No answer".into(),
            };
            wrap(
                &notice(
                    "? ",
                    &format!("{} → {}", visible(question), visible(&answer)),
                    theme::secondary(),
                ),
                width,
            )
        }
        Item::Unknown => Vec::new(),
    }
}

/// What a turn leaves behind when it did not end normally.
pub(crate) fn turn_footer(turn: &Turn, error_shown: bool) -> Option<LogicalLine> {
    match turn.status {
        TurnStatus::Interrupted => Some(notice("■ ", "Turn interrupted", theme::secondary())),
        TurnStatus::Failed if !error_shown => {
            let message = turn
                .error
                .as_ref()
                .map_or("no reason given", |error| error.message.as_str());
            Some(notice(
                "✗ ",
                &format!("Turn failed: {message}"),
                theme::failure(),
            ))
        }
        _ => None,
    }
}

/// A one-line message from the client or server (warning, error, send failure).
pub(crate) fn notice(mark: &str, text: &str, style: Style) -> LogicalLine {
    LogicalLine::new(
        vec![Span::raw("  "), Span::styled(mark.to_owned(), style)],
        vec![Span::styled(sanitize(text), style)],
    )
}

pub(crate) fn wrap_all(lines: &[LogicalLine], width: u16) -> Vec<Line<'static>> {
    lines.iter().flat_map(|line| wrap(line, width)).collect()
}

/// Thinking shows one grey line: 「思考中…」 while it runs, its first line once done.
fn reasoning(
    summary: &[String],
    content: &[String],
    finished: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let style = theme::thinking();
    if !finished {
        return vec![Line::from(Span::styled("  Thinking…", style))];
    }
    let parts = if summary.iter().any(|part| !part.trim().is_empty()) {
        summary
    } else {
        content
    };
    let text = sanitize(&parts.join("\n"));
    let Some(first) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Vec::new();
    };
    vec![Line::from(truncate(
        vec![Span::styled(format!("  Thinking: {first}"), style)],
        usize::from(width),
    ))]
}

fn file_read(
    path: &str,
    start_line: u32,
    content: &str,
    status: ItemStatus,
    cwd: &str,
    width: u16,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("◇ ", theme::secondary()),
        Span::styled("Read ", Style::new().bold()),
        Span::raw(display_path(path, cwd)),
    ];
    let lines = content.lines().count() as u32;
    let detail = match status {
        ItemStatus::InProgress => None,
        ItemStatus::Failed => Some(Span::styled("  failed", theme::failure())),
        ItemStatus::Declined => Some(Span::styled("  declined", theme::secondary())),
        _ if lines == 0 => Some(Span::styled("  empty", theme::secondary())),
        _ if lines == 1 => Some(Span::styled(
            format!("  line {start_line}"),
            theme::secondary(),
        )),
        _ => Some(Span::styled(
            format!("  lines {start_line}–{}", start_line + lines - 1),
            theme::secondary(),
        )),
    };
    spans.extend(detail);
    Line::from(truncate(spans, usize::from(width)))
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DiffKind {
    Hunk,
    Added,
    Removed,
    Context,
    Note,
}

/// An item waiting for an approval: only its title row, marked 「等你批准」.
/// The approval box below shows the whole command or diff.
pub(crate) fn waiting(item: &Item, cwd: &str, width: u16) -> Vec<Line<'static>> {
    let mark = || vec![Span::styled("Waiting for approval", theme::warning())];
    match item {
        Item::CommandExecution { command, .. } => {
            vec![command_title(
                command,
                dot(theme::secondary()),
                mark(),
                width,
            )]
        }
        Item::FileChange { changes, .. } => changes
            .iter()
            .map(|change| {
                let diff = parse_diff(&sanitize(&change.diff), &change.kind);
                let header = change_header(change, &diff, dot(theme::secondary()), cwd);
                right_aligned(header, mark(), usize::from(width))
            })
            .collect(),
        _ => self::item(item, false, cwd, width),
    }
}

fn file_change(
    change: &FileUpdateChange,
    status: ItemStatus,
    cwd: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let diff = parse_diff(&sanitize(&change.diff), &change.kind);
    let (bullet, suffix) = match status {
        ItemStatus::InProgress => (dot(theme::secondary()), None),
        ItemStatus::Failed => (cross(), Some(Span::styled("  failed", theme::failure()))),
        ItemStatus::Declined => (
            dot(theme::secondary()),
            Some(Span::styled("  declined", theme::secondary())),
        ),
        _ => (dot(theme::success()), None),
    };
    let mut header = change_header(change, &diff, bullet, cwd);
    header.extend(suffix);
    let mut rows = vec![Line::from(truncate(header, usize::from(width)))];
    for (kind, text) in diff {
        let (marker, style) = diff_style(kind);
        let line = LogicalLine {
            indent: vec![
                Span::styled(GUTTER, theme::secondary()),
                Span::styled(marker, style),
            ],
            hang: vec![Span::styled(GUTTER, theme::secondary()), Span::raw("  ")],
            body: vec![Span::styled(text, style)],
        };
        rows.extend(wrap(&line, width));
    }
    rows
}

/// `● 改 path  +a −r`: a file change's title, before any status suffix.
fn change_header(
    change: &FileUpdateChange,
    diff: &[(DiffKind, String)],
    bullet: Span<'static>,
    cwd: &str,
) -> Vec<Span<'static>> {
    let count = |wanted| diff.iter().filter(|(kind, _)| *kind == wanted).count();
    let (added, removed) = (count(DiffKind::Added), count(DiffKind::Removed));
    let verb = match &change.kind {
        PatchChangeKind::Add => "Add ",
        PatchChangeKind::Delete => "Delete ",
        _ => "Edit ",
    };
    let mut header = vec![
        Span::raw("  "),
        bullet,
        Span::styled(verb, Style::new().bold()),
        Span::raw(display_path(&change.path, cwd)),
    ];
    if let PatchChangeKind::Update {
        move_path: Some(to),
    } = &change.kind
    {
        header.push(Span::raw(format!(" → {}", display_path(to, cwd))));
    }
    header.push(Span::raw("  "));
    match &change.kind {
        PatchChangeKind::Add => header.push(Span::styled(format!("+{added}"), theme::success())),
        PatchChangeKind::Delete => {
            header.push(Span::styled(format!("−{removed}"), theme::failure()))
        }
        _ => header.extend([
            Span::styled(format!("+{added}"), theme::success()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), theme::failure()),
        ]),
    }
    header
}

/// The marker and colour of a diff line.
pub(crate) fn diff_style(kind: DiffKind) -> (&'static str, Style) {
    match kind {
        DiffKind::Hunk => ("", theme::accent()),
        DiffKind::Added => ("+ ", theme::diff_added()),
        DiffKind::Removed => ("- ", theme::diff_removed()),
        DiffKind::Context => ("  ", theme::secondary()),
        DiffKind::Note => ("", theme::secondary()),
    }
}

/// Splits a unified diff into marked lines. Text without hunks is a whole
/// file being added (or deleted).
pub(crate) fn parse_diff(text: &str, kind: &PatchChangeKind) -> Vec<(DiffKind, String)> {
    if !text.lines().any(|line| line.starts_with("@@")) {
        let kind = match kind {
            PatchChangeKind::Delete => DiffKind::Removed,
            _ => DiffKind::Added,
        };
        return text.lines().map(|line| (kind, line.to_owned())).collect();
    }
    let mut lines = Vec::new();
    let mut in_hunk = false;
    for line in text.lines() {
        let entry = if line.starts_with("@@") {
            in_hunk = true;
            (DiffKind::Hunk, line)
        } else if !in_hunk {
            // File headers (diff --git, index, ---, +++) repeat the card's title.
            continue;
        } else if let Some(rest) = line.strip_prefix('+') {
            (DiffKind::Added, rest)
        } else if let Some(rest) = line.strip_prefix('-') {
            (DiffKind::Removed, rest)
        } else if line.starts_with('\\') {
            (DiffKind::Note, line)
        } else {
            (DiffKind::Context, line.strip_prefix(' ').unwrap_or(line))
        };
        lines.push((entry.0, entry.1.to_owned()));
    }
    lines
}

fn command_card(
    command: &str,
    status: ItemStatus,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let dim = theme::secondary();
    let took = duration_ms.map(|ms| Span::styled(format!(" · {}", format_duration(ms)), dim));
    let (bullet, mut right) = match (status, exit_code) {
        (ItemStatus::InProgress, _) => (dot(dim), vec![Span::styled("running", dim)]),
        (ItemStatus::Declined, _) => (dot(dim), vec![Span::styled("declined", dim)]),
        (_, Some(0)) => (dot(theme::success()), vec![Span::styled("exit 0", dim)]),
        (_, Some(code)) => (
            cross(),
            vec![Span::styled(format!("exit {code}"), theme::failure())],
        ),
        (ItemStatus::Failed, None) => (cross(), vec![Span::styled("failed", theme::failure())]),
        (_, None) => (dot(theme::success()), Vec::new()),
    };
    right.extend(took);
    let mut rows = vec![command_title(command, bullet, right, width)];

    let output = sanitize(output);
    let mut lines: Vec<&str> = output.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let tail = if status == ItemStatus::InProgress {
        LIVE_TAIL
    } else {
        FINISHED_TAIL
    };
    if lines.len() > tail {
        rows.push(Line::from(vec![
            Span::styled(GUTTER, dim),
            Span::styled(
                format!("… ({} lines, showing the last {tail})", lines.len()),
                dim,
            ),
        ]));
    }
    for line in &lines[lines.len().saturating_sub(tail)..] {
        rows.push(Line::from(truncate(
            vec![
                Span::styled(GUTTER, dim),
                Span::styled((*line).to_owned(), dim),
            ],
            usize::from(width),
        )));
    }
    rows
}

/// `● 跑 <first line of the command>` with `right` flush with the right edge.
/// Control characters in the command show as escapes, as in the approval box.
fn command_title(
    command: &str,
    bullet: Span<'static>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let clean = visible(command);
    let mut lines = clean.lines();
    let mut shown = lines.next().unwrap_or_default().to_owned();
    if lines.next().is_some() {
        shown.push_str(" …");
    }
    let left = vec![
        Span::raw("  "),
        bullet,
        Span::styled("Run ", Style::new().bold()),
        Span::raw(shown),
    ];
    right_aligned(left, right, usize::from(width))
}

/// A card's mark: `●` in `style`.
fn dot(style: Style) -> Span<'static> {
    Span::styled("● ", style)
}

/// A failed card's mark: a red `✗`.
fn cross() -> Span<'static> {
    Span::styled("✗ ", theme::failure())
}

/// `left` then `right` flush with the right edge; the left side is cut to fit.
fn right_aligned(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let right_width = spans_width(&right);
    if right.is_empty() || width < right_width + 12 {
        return Line::from(truncate(left, width));
    }
    let mut spans = truncate(left, width - right_width - 2);
    let gap = width - right_width - spans_width(&spans);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// Paths inside the session's working directory are shown relative to it.
/// Control characters show as escapes.
pub(crate) fn display_path(path: &str, cwd: &str) -> String {
    let path = visible(path);
    if cwd.is_empty() {
        return path;
    }
    match path.strip_prefix(cwd.trim_end_matches('/')) {
        Some(rest) if rest.starts_with('/') && rest.len() > 1 => rest[1..].to_owned(),
        _ => path,
    }
}
