//! The approval box. It takes the input box's place while the model waits for
//! permission to run a command or change a file, and shows the whole command
//! or diff, control characters drawn as escapes. The answers always fit; a
//! long body scrolls in the rows left over.

use canter_protocol::{
    ApprovalDecision, ApprovalRequest, Item, PatchChangeKind, PendingRequest, SessionSnapshot,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::cells::display_path;
use crate::diff::Diff;
use crate::highlight;
use crate::text::{LogicalLine, visible, wrap};
use crate::theme;

/// The box laid out for one frame.
pub(crate) struct ApprovalBox {
    pub title: Line<'static>,
    /// Which body rows are showing, when the body scrolls.
    pub position: Option<Line<'static>>,
    /// Inside the border: the visible body rows, then the answers.
    pub rows: Vec<Line<'static>>,
    pub footer: Line<'static>,
    /// Body rows one PgUp/PgDn moves.
    pub page: usize,
    pub max_scroll: usize,
    /// False when the window is too small for the answers: the box says so
    /// and takes no answer.
    pub fits: bool,
}

/// The answers offered, in the box's order. Unknown answers are left out.
pub(crate) fn options(approval: &ApprovalRequest) -> Vec<ApprovalDecision> {
    [
        ApprovalDecision::Accept,
        ApprovalDecision::AcceptForSession,
        ApprovalDecision::Decline,
        ApprovalDecision::Cancel,
    ]
    .into_iter()
    .filter(|decision| approval.available_decisions.contains(decision))
    .collect()
}

pub(crate) fn decision_label(decision: ApprovalDecision, tool: &str) -> &'static str {
    match decision {
        ApprovalDecision::Accept => "Allow once",
        ApprovalDecision::AcceptForSession if tool == "web_fetch" => {
            "Allow for this session (this host and port)"
        }
        ApprovalDecision::AcceptForSession => {
            "Allow for this session (don't ask again for this tool)"
        }
        ApprovalDecision::Decline => "Decline",
        ApprovalDecision::Cancel => "Decline and stop",
        ApprovalDecision::Unknown => "(unknown answer)",
    }
}

/// Lays out the box for `width` columns and at most `max_height` rows,
/// borders included. `selected` and `scroll` are clamped here. The answers
/// show dim until `taken`: keys do not answer yet.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout(
    snapshot: &SessionSnapshot,
    request: &PendingRequest,
    approval: &ApprovalRequest,
    home: Option<&str>,
    selected: usize,
    scroll: usize,
    taken: bool,
    width: u16,
    max_height: u16,
) -> ApprovalBox {
    // Border and one column of padding on each side.
    let inner = width.saturating_sub(4).max(1);
    let item = snapshot
        .turns
        .iter()
        .find(|turn| turn.id == request.turn_id)
        .and_then(|turn| {
            turn.items
                .iter()
                .find(|item| item.id() == Some(approval.item_id.as_str()))
        });
    let cwd = &snapshot.settings.cwd;
    let (title, body) = match approval.tool.as_str() {
        "web_fetch" => (
            "Fetch this URL?".to_owned(),
            json_body(&approval.arguments, inner),
        ),
        "shell" => (
            "Run this command?".to_owned(),
            shell_body(approval, item, cwd, home, inner),
        ),
        "write" | "edit" => {
            let kind = match item {
                Some(Item::FileChange { changes, .. }) => changes.first().map(|c| c.kind.clone()),
                _ => None,
            }
            .unwrap_or(PatchChangeKind::Update { move_path: None });
            let title = if kind == PatchChangeKind::Add {
                "Create this file?"
            } else {
                "Edit this file?"
            };
            (title.to_owned(), change_body(approval, &kind, cwd, inner))
        }
        _ if matches!(item, Some(Item::McpToolCall { .. })) => {
            let Some(Item::McpToolCall { server, tool, .. }) = item else {
                unreachable!()
            };
            (
                format!("Allow MCP {} / {}?", visible(server), visible(tool)),
                json_body(&approval.arguments, inner),
            )
        }
        tool => (
            format!("Allow {}?", visible(tool)),
            json_body(&approval.arguments, inner),
        ),
    };

    let options = options(approval);
    let selected = selected.min(options.len().saturating_sub(1));
    let mut tail = vec![Line::default()];
    for (index, decision) in options.iter().enumerate() {
        let text = format!(
            "{}. {}",
            index + 1,
            decision_label(*decision, &approval.tool)
        );
        let line = if index == selected {
            Line::from(vec![
                Span::styled("› ", theme::accent().bold()),
                Span::styled(text, Style::new().bold()),
            ])
        } else {
            Line::from(vec![Span::raw("  "), Span::raw(text)])
        };
        tail.push(if taken {
            line
        } else {
            line.patch_style(theme::secondary())
        });
        // What it grants, readable before choosing it (also with its number key).
        if *decision == ApprovalDecision::AcceptForSession {
            let note = LogicalLine::new(
                vec![Span::raw("     ")],
                vec![Span::styled(session_note(approval), theme::warning())],
            );
            tail.extend(wrap(&note, inner));
        }
    }

    let border = theme::warning_border();
    let title = Line::from(vec![
        Span::styled("─ ", border),
        Span::styled(title, Style::new().bold()),
        Span::raw(" "),
    ]);
    let footer = |hint: &'static str| {
        Line::from(vec![
            Span::styled("─ ", border),
            Span::styled(hint, theme::secondary()),
            Span::styled(" ─", border),
        ])
    };
    // The answers come first; the body gets the rows left, at least one.
    let room = usize::from(max_height).saturating_sub(2 + tail.len());
    if room == 0 {
        return ApprovalBox {
            title,
            position: None,
            rows: vec![Line::from(Span::styled(
                " Window too small for the approval box; make it taller to choose",
                theme::warning(),
            ))],
            footer: footer("Esc interrupt"),
            page: 1,
            max_scroll: 0,
            fits: false,
        };
    }
    let shown = body.len().min(room);
    let max_scroll = body.len() - shown;
    let scroll = scroll.min(max_scroll);
    let mut rows: Vec<Line<'static>> = body[scroll..scroll + shown].to_vec();
    rows.extend(tail);
    for row in &mut rows {
        row.spans.insert(0, Span::raw(" "));
    }

    let hint = if max_scroll > 0 {
        "↑↓ select, PgUp/PgDn scroll, Enter confirm, number keys pick, Esc interrupt"
    } else {
        "↑↓ select, Enter confirm, number keys pick, Esc interrupt"
    };
    let position = (max_scroll > 0).then(|| {
        Line::from(vec![
            Span::styled(
                format!(" {}–{} / {} ", scroll + 1, scroll + shown, body.len()),
                theme::secondary(),
            ),
            Span::styled("─", border),
        ])
        .right_aligned()
    });
    ApprovalBox {
        title,
        position,
        rows,
        footer: footer(hint),
        page: shown.max(1),
        max_scroll,
        fits: true,
    }
}

/// What 「本会话都允许」 really grants. For shell that is every command, and
/// switching the mode back to 「每次问」 later does not take it back.
fn session_note(approval: &ApprovalRequest) -> String {
    let what = match approval.tool.as_str() {
        "web_fetch" => {
            let authority = approval.arguments["resolvedUrl"]
                .as_str()
                .or_else(|| approval.arguments["url"].as_str())
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| {
                    Some(format!(
                        "{}:{}",
                        url.host_str()?,
                        url.port_or_known_default()?
                    ))
                })
                .unwrap_or_else(|| "this host and port".into());
            format!("Later fetches to {authority} won't ask; other hosts or ports still ask")
        }
        "shell" => "All later commands in this session run without asking".to_owned(),
        "write" => "Later whole-file writes in this session won't ask".to_owned(),
        "edit" => "Later text-replacement edits in this session won't ask".to_owned(),
        other => format!("Later {} calls in this session won't ask", visible(other)),
    };
    format!("{what}, even after switching back to Always ask.")
}

/// The whole command, coloured as Bash, then the directory it runs in.
fn shell_body(
    approval: &ApprovalRequest,
    item: Option<&Item>,
    session_cwd: &str,
    home: Option<&str>,
    width: u16,
) -> Vec<Line<'static>> {
    let (item_command, item_cwd) = match item {
        Some(Item::CommandExecution { command, cwd, .. }) => (command.as_str(), cwd.as_str()),
        _ => ("", ""),
    };
    let command = approval
        .arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or(item_command);
    let command = visible(command);
    let lines: Vec<&str> = command.split('\n').collect();
    let bodies = highlight::bash(&lines).unwrap_or_else(|| {
        lines
            .iter()
            .map(|line| vec![Span::raw((*line).to_owned())])
            .collect()
    });
    let mut rows = Vec::new();
    for body in bodies {
        rows.extend(wrap(&LogicalLine::new(Vec::new(), body), width));
    }
    let cwd = if item_cwd.is_empty() {
        session_cwd
    } else {
        item_cwd
    };
    rows.extend(wrap(
        &LogicalLine::new(
            vec![Span::styled("Directory: ", theme::secondary())],
            vec![Span::raw(shorten_home(&visible(cwd), home))],
        ),
        width,
    ));
    rows
}

/// The target file with its line counts, then the diff.
fn change_body(
    approval: &ApprovalRequest,
    kind: &PatchChangeKind,
    cwd: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let text = |key: &str| approval.arguments.get(key).and_then(Value::as_str);
    // The tools resolve the path; the resolved one is what really gets written.
    let path = text("resolvedPath").or(text("path")).unwrap_or_default();
    let diff = Diff::parse(&visible(text("diff").unwrap_or_default()), kind, path);
    let added = Span::styled(format!("+{}", diff.added()), theme::success());
    let removed = Span::styled(format!("−{}", diff.removed()), theme::failure());
    let mut header = vec![
        Span::styled(display_path(path, cwd), Style::new().bold()),
        Span::raw("  "),
    ];
    // Same counts as the item's card: a new file only adds, a deleted one only removes.
    match kind {
        PatchChangeKind::Add => header.push(added),
        PatchChangeKind::Delete => header.push(removed),
        _ => header.extend([added, Span::raw(" "), removed]),
    }
    let mut rows = wrap(&LogicalLine::new(Vec::new(), header), width);
    if diff.is_empty() {
        rows.push(Line::from(Span::styled("(no changes)", theme::secondary())));
    }
    rows.extend(diff.rows("", width, None).into_iter().map(|row| row.line));
    rows
}

/// Arguments of a tool the box does not know, as indented JSON.
fn json_body(arguments: &Value, width: u16) -> Vec<Line<'static>> {
    let text = serde_json::to_string_pretty(arguments).unwrap_or_default();
    visible(&text)
        .split('\n')
        .flat_map(|line| {
            wrap(
                &LogicalLine::new(Vec::new(), vec![Span::raw(line.to_owned())]),
                width,
            )
        })
        .collect()
}

/// `/home/me/x` → `~/x`.
pub(crate) fn shorten_home(path: &str, home: Option<&str>) -> String {
    if let Some(home) = home
        && let Some(rest) = path.strip_prefix(home.trim_end_matches('/'))
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("~{rest}");
    }
    path.to_owned()
}
