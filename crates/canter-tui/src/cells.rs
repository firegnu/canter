//! Rows for each kind of item: the user's prompt, the model's text, thinking,
//! and the tool cards (read a file, change files with a diff, run a command,
//! call an MCP tool, fetch a web page).

use canter_protocol::{
    FileUpdateChange, Item, ItemStatus, PatchChangeKind, Turn, TurnStatus, UserInput,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::diff::Diff;
use crate::markdown;
use crate::text::{
    LogicalLine, char_width, sanitize, spans_width, str_width, truncate, visible, wrap,
};
use crate::theme;

/// Output lines a finished command keeps.
const FINISHED_TAIL: usize = 3;
/// Output lines shown while a command runs.
const LIVE_TAIL: usize = 5;
/// Output lines a failed command keeps: the error is what matters then.
const FAILED_TAIL: usize = 10;
/// Result lines an MCP call keeps, from the start.
const MCP_HEAD: usize = 3;
/// Output lines one item shows at most with details (Ctrl+O): the first
/// and last half of them.
const DETAILS_LINES: usize = 500;
const GUTTER: &str = "    │ ";
/// Where a card's title text starts: after `  ● Run `.
const TITLE_INDENT: &str = "        ";
/// Diff lines a changed file shows in the history; the approval box shows all.
const EDIT_LINES: usize = 12;
/// Diff lines a new file shows in the history. A deleted file shows none
/// until details are shown.
const ADD_LINES: usize = 6;
/// Where a card's diff lines start: under the text of its command output.
const DIFF_INDENT: &str = "      ";

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

/// The model's text as markdown, indented two columns, for a screen `width`
/// columns wide.
pub(crate) fn agent_message(text: &str, width: u16) -> Vec<LogicalLine> {
    markdown::render(&sanitize(text), width.saturating_sub(2))
        .into_iter()
        .map(|line| line.prefixed(Span::raw("  "), Span::raw("  ")))
        .collect()
}

/// Rows for an item; `finished` picks its final form over the in-progress one.
/// With `details` (Ctrl+O), nothing is folded: whole thinking, all output, all
/// of a diff.
pub(crate) fn item(
    item: &Item,
    finished: bool,
    details: bool,
    cwd: &str,
    width: u16,
) -> Vec<Line<'static>> {
    match item {
        Item::UserMessage { content, .. } => prompt(content, None, width),
        Item::AgentMessage { text, .. } => wrap_all(&agent_message(text, width), width),
        Item::Reasoning {
            summary, content, ..
        } => reasoning(summary, content, finished, details, width),
        Item::WebFetch {
            url,
            resolved_url,
            result,
            status,
            ..
        } => web_fetch(
            resolved_url.as_deref().unwrap_or(url),
            result,
            *status,
            details,
            width,
        ),
        Item::McpToolCall {
            server,
            tool,
            arguments,
            status,
            result,
            ..
        } => mcp_call(
            &format!("{server}/{tool}"),
            arguments,
            *status,
            result,
            details,
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
            details,
            width,
        ),
        Item::FileChange {
            changes, status, ..
        } => changes
            .iter()
            .flat_map(|change| file_change(change, *status, details, cwd, width))
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

/// Thinking, grey and italic, from the summary when there is one. Folded it
/// is one row: `Thinking… <the latest line>` while it runs, `Thought: <the
/// first line>` once done. With `details`, a `Thought` row and then the whole
/// text as markdown, each row after a grey `┊`.
fn reasoning(
    summary: &[String],
    content: &[String],
    finished: bool,
    details: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let style = theme::thinking();
    let summarized = summary.iter().any(|part| !part.trim().is_empty());
    let parts = if summarized { summary } else { content };
    let text = sanitize(&parts.join("\n\n"));
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if details {
        if finished && text.trim().is_empty() {
            return Vec::new();
        }
        let heading = if finished {
            "  Thought"
        } else {
            "  Thinking…"
        };
        let mut rows = vec![Line::from(Span::styled(heading, style))];
        let bar = || Span::styled("  ┊ ", theme::secondary());
        let thinking = |spans: Vec<Span<'static>>| {
            spans
                .into_iter()
                .map(|span| span.patch_style(style))
                .collect()
        };
        for line in markdown::render(&text, width.saturating_sub(4)) {
            let line = LogicalLine {
                indent: thinking(line.indent),
                hang: thinking(line.hang),
                body: thinking(line.body),
            };
            rows.extend(wrap(&line.prefixed(bar(), bar()), width));
        }
        return rows;
    }
    if !finished {
        let lead = "  Thinking…";
        let Some(latest) = lines.next_back() else {
            return vec![Line::from(Span::styled(lead, style))];
        };
        let room = usize::from(width).saturating_sub(str_width(lead) + 1);
        let shown = if str_width(latest) <= room {
            latest.to_owned()
        } else {
            format!("…{}", tail(latest, room.saturating_sub(1)).trim_start())
        };
        return vec![Line::from(truncate(
            vec![Span::styled(format!("{lead} {shown}"), style)],
            usize::from(width),
        ))];
    }
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    // A summary starts with its title in bold: the title alone is the row.
    let (first, rest) = match first
        .strip_prefix("**")
        .and_then(|after| after.split_once("**"))
    {
        Some((title, rest)) if summarized && !title.trim().is_empty() => (title.trim(), rest),
        _ => (first, ""),
    };
    let more = if !rest.trim().is_empty() || lines.next().is_some() {
        "…"
    } else {
        ""
    };
    vec![Line::from(truncate(
        vec![Span::styled(format!("  Thought: {first}{more}"), style)],
        usize::from(width),
    ))]
}

/// The end of `text` that fits in `width` columns.
fn tail(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices().rev() {
        used += char_width(c);
        if used > width {
            return &text[at + c.len_utf8()..];
        }
    }
    text
}

fn file_read(
    path: &str,
    start_line: u32,
    content: &str,
    status: ItemStatus,
    cwd: &str,
    width: u16,
) -> Line<'static> {
    let mark = if status == ItemStatus::Failed {
        cross()
    } else {
        Span::styled("◇ ", theme::secondary())
    };
    let mut spans = vec![
        Span::raw("  "),
        mark,
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

/// An item waiting for an approval: only its title row, marked 「等你批准」.
/// The approval box below shows the whole command or diff.
pub(crate) fn waiting(item: &Item, details: bool, cwd: &str, width: u16) -> Vec<Line<'static>> {
    let mark = || vec![Span::styled("Waiting for approval", theme::warning())];
    match item {
        Item::CommandExecution { command, .. } => {
            command_title(command, dot(theme::secondary()), mark(), false, width)
        }
        Item::FileChange { changes, .. } => changes
            .iter()
            .map(|change| {
                let diff = Diff::parse(&sanitize(&change.diff), &change.kind);
                let header = change_header(change, &diff, dot(theme::secondary()), cwd);
                right_aligned(header, mark(), usize::from(width))
            })
            .collect(),
        _ => self::item(item, false, details, cwd, width),
    }
}

fn file_change(
    change: &FileUpdateChange,
    status: ItemStatus,
    details: bool,
    cwd: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let diff = Diff::parse(&sanitize(&change.diff), &change.kind);
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
    let limit = match &change.kind {
        _ if details => None,
        PatchChangeKind::Add => Some(ADD_LINES),
        PatchChangeKind::Delete => return rows,
        _ => Some(EDIT_LINES),
    };
    rows.extend(diff.rows(DIFF_INDENT, width, limit));
    rows
}

/// `● 改 path  +a −r`: a file change's title, before any status suffix.
fn change_header(
    change: &FileUpdateChange,
    diff: &Diff,
    bullet: Span<'static>,
    cwd: &str,
) -> Vec<Span<'static>> {
    let (added, removed) = (diff.added(), diff.removed());
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

fn command_card(
    command: &str,
    status: ItemStatus,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: &str,
    details: bool,
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
    // The same cases as the `✗` above.
    let failed = !matches!(status, ItemStatus::InProgress | ItemStatus::Declined)
        && exit_code.map_or(status == ItemStatus::Failed, |code| code != 0);
    let mut rows = command_title(command, bullet, right, details, width);
    let (fold, style) = if details {
        (Fold::Details, if failed { Style::new() } else { dim })
    } else if status == ItemStatus::InProgress {
        (Fold::Tail(LIVE_TAIL), dim)
    } else if failed {
        (Fold::Tail(FAILED_TAIL), Style::new())
    } else {
        (Fold::Tail(FINISHED_TAIL), dim)
    };
    rows.extend(output_rows(output, fold, style, width));
    rows
}

/// `● Call server/tool  {arguments}`, the arguments as JSON on one row, then
/// the first lines of the result: an MCP result's start matters most.
fn mcp_call(
    name: &str,
    arguments: &serde_json::Value,
    status: ItemStatus,
    result: &str,
    details: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let (bullet, right) = status_marks(status);
    let mut object = vec![Span::raw(first_line(name))];
    let empty = arguments.is_null() || arguments.as_object().is_some_and(|args| args.is_empty());
    if !empty {
        object.push(Span::raw(format!(
            "  {}",
            first_line(&arguments.to_string())
        )));
    }
    let mut rows = vec![title(bullet, "Call ", object, right, width)];
    let fold = if details {
        Fold::Details
    } else {
        Fold::Head(MCP_HEAD)
    };
    let style = if status == ItemStatus::Failed {
        Style::new()
    } else {
        theme::secondary()
    };
    rows.extend(output_rows(result, fold, style, width));
    rows
}

/// `● Fetch <url>` and the size fetched. The page is for the model, so it
/// shows only with details; a one-line note (a redirect to another host,
/// content left out) shows as it is, and a failure's reason in red.
fn web_fetch(
    url: &str,
    result: &str,
    status: ItemStatus,
    details: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let dim = theme::secondary();
    let (bullet, mut right) = status_marks(status);
    let one_line = !result.trim_end().contains('\n');
    let redirect = one_line && result.starts_with("Redirected to ");
    let note = one_line
        && (result.starts_with("[Content omitted: ")
            || (result.starts_with("Download exceeds ")
                && result.trim_end().ends_with("; content omitted.")));
    let page = status == ItemStatus::Completed && !redirect && !note;
    if status == ItemStatus::Completed {
        if redirect {
            right = vec![Span::styled("redirect", dim)];
        } else if page {
            right = vec![Span::styled(format_size(result.len()), dim)];
        }
    }
    let mut rows = vec![title(
        bullet,
        "Fetch ",
        vec![Span::raw(first_line(url))],
        right,
        width,
    )];
    if page && !details {
        return rows;
    }
    let fold = if details {
        Fold::Details
    } else {
        Fold::Head(FINISHED_TAIL)
    };
    let style = if status == ItemStatus::Failed {
        theme::failure()
    } else {
        dim
    };
    rows.extend(output_rows(result, fold, style, width));
    rows
}

/// The mark and right-hand status of a card that has no exit code.
fn status_marks(status: ItemStatus) -> (Span<'static>, Vec<Span<'static>>) {
    let dim = theme::secondary();
    match status {
        ItemStatus::InProgress => (dot(dim), vec![Span::styled("running", dim)]),
        ItemStatus::Declined => (dot(dim), vec![Span::styled("declined", dim)]),
        ItemStatus::Failed => (cross(), vec![Span::styled("failed", theme::failure())]),
        _ => (dot(theme::success()), Vec::new()),
    }
}

/// Which output lines a card keeps.
enum Fold {
    /// The first lines, then `… N more lines · Ctrl+O`.
    Head(usize),
    /// `… N more lines · Ctrl+O`, then the last lines.
    Tail(usize),
    /// All of them, up to `DETAILS_LINES`: past that the first and last half
    /// with one row between saying how many are left out.
    Details,
}

/// Output rows, each after `    │ ` and cut to `width`, in `style`.
fn output_rows(output: &str, fold: Fold, style: Style, width: u16) -> Vec<Line<'static>> {
    let dim = theme::secondary();
    let output = sanitize(output);
    let mut lines: Vec<&str> = output.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let all = lines.len();
    let row = |line: &str| {
        Line::from(truncate(
            vec![
                Span::styled(GUTTER, dim),
                Span::styled(line.to_owned(), style),
            ],
            usize::from(width),
        ))
    };
    let note = |text: String| Line::from(vec![Span::styled(GUTTER, dim), Span::styled(text, dim)]);
    let more = |hidden: usize| note(format!("… {hidden} more lines · Ctrl+O"));
    let mut rows = Vec::new();
    match fold {
        Fold::Head(keep) => {
            rows.extend(lines.iter().take(keep).map(|line| row(line)));
            if all > keep {
                rows.push(more(all - keep));
            }
        }
        Fold::Tail(keep) => {
            if all > keep {
                rows.push(more(all - keep));
            }
            rows.extend(
                lines[all.saturating_sub(keep)..]
                    .iter()
                    .map(|line| row(line)),
            );
        }
        Fold::Details if all > DETAILS_LINES => {
            let half = DETAILS_LINES / 2;
            rows.extend(lines[..half].iter().map(|line| row(line)));
            rows.push(note(format!("… {} lines not shown", all - DETAILS_LINES)));
            rows.extend(lines[all - half..].iter().map(|line| row(line)));
        }
        Fold::Details => rows.extend(lines.iter().map(|line| row(line))),
    }
    rows
}

/// `● Run <first line of the command>` with `right` flush with the right
/// edge; with `whole`, the command's other lines follow, each on its row
/// under the first. Control characters in the command show as escapes, as in
/// the approval box.
fn command_title(
    command: &str,
    bullet: Span<'static>,
    right: Vec<Span<'static>>,
    whole: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let clean = visible(command);
    let mut lines = clean.lines();
    let mut shown = lines.next().unwrap_or_default().to_owned();
    let rest: Vec<&str> = lines.collect();
    if !whole && !rest.is_empty() {
        shown.push_str(" …");
    }
    let mut rows = vec![title(bullet, "Run ", vec![Span::raw(shown)], right, width)];
    if whole {
        rows.extend(rest.iter().map(|line| {
            Line::from(truncate(
                vec![Span::raw(TITLE_INDENT), Span::raw((*line).to_owned())],
                usize::from(width),
            ))
        }));
    }
    rows
}

/// A card's first row: `● Verb object` with `right` flush with the right edge.
fn title(
    bullet: Span<'static>,
    verb: &'static str,
    object: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let mut left = vec![
        Span::raw("  "),
        bullet,
        Span::styled(verb, Style::new().bold()),
    ];
    left.extend(object);
    right_aligned(left, right, usize::from(width))
}

/// The first line of `text` with control characters as escapes, ending with
/// ` …` when more lines follow.
fn first_line(text: &str) -> String {
    let clean = visible(text);
    let mut lines = clean.lines();
    let mut shown = lines.next().unwrap_or_default().to_owned();
    if lines.next().is_some() {
        shown.push_str(" …");
    }
    shown
}

/// `12 B`, `18.4 KB`, `1.2 MB`.
fn format_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let size = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if size < KB * KB {
        format!("{:.1} KB", size / KB)
    } else {
        format!("{:.1} MB", size / KB / KB)
    }
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
