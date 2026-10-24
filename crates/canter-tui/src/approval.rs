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

use crate::cells::{DiffKind, diff_style, display_path, parse_diff};
use crate::text::{LogicalLine, visible, wrap};

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

pub(crate) fn decision_label(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Accept => "允许这一次",
        ApprovalDecision::AcceptForSession => "本会话都允许（以后这个工具不再问）",
        ApprovalDecision::Decline => "拒绝",
        ApprovalDecision::Cancel => "拒绝并停下",
        ApprovalDecision::Unknown => "（不认识的回答）",
    }
}

/// Lays out the box for `width` columns and at most `max_height` rows,
/// borders included. `selected` and `scroll` are clamped here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout(
    snapshot: &SessionSnapshot,
    request: &PendingRequest,
    approval: &ApprovalRequest,
    home: Option<&str>,
    selected: usize,
    scroll: usize,
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
        "shell" => (
            "要跑这条命令吗？".to_owned(),
            shell_body(approval, item, cwd, home, inner),
        ),
        "write" | "edit" => {
            let kind = match item {
                Some(Item::FileChange { changes, .. }) => changes.first().map(|c| c.kind.clone()),
                _ => None,
            }
            .unwrap_or(PatchChangeKind::Update { move_path: None });
            let title = if kind == PatchChangeKind::Add {
                "要新建这个文件吗？"
            } else {
                "要改这个文件吗？"
            };
            (title.to_owned(), change_body(approval, &kind, cwd, inner))
        }
        tool => (
            format!("要允许 {} 吗？", visible(tool)),
            json_body(&approval.arguments, inner),
        ),
    };

    let options = options(approval);
    let selected = selected.min(options.len().saturating_sub(1));
    let mut tail = vec![Line::default()];
    for (index, decision) in options.iter().enumerate() {
        let text = format!("{}. {}", index + 1, decision_label(*decision));
        tail.push(if index == selected {
            Line::from(vec![
                Span::styled("› ", Style::new().cyan().bold()),
                Span::styled(text, Style::new().bold()),
            ])
        } else {
            Line::from(vec![Span::raw("  "), Span::raw(text)])
        });
        // What it grants, readable before choosing it (also with its number key).
        if *decision == ApprovalDecision::AcceptForSession {
            let note = LogicalLine::new(
                vec![Span::raw("     ")],
                vec![Span::styled(
                    session_note(&approval.tool),
                    Style::new().yellow(),
                )],
            );
            tail.extend(wrap(&note, inner));
        }
    }

    let border = Style::new().yellow();
    let title = Line::from(vec![
        Span::styled("─ ", border),
        Span::styled(title, Style::new().bold()),
        Span::raw(" "),
    ]);
    let footer = |hint: &'static str| {
        Line::from(vec![
            Span::styled("─ ", border),
            Span::styled(hint, Style::new().dim()),
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
                " 窗口太小，放不下批准框；把窗口拉高一点再选",
                Style::new().yellow(),
            ))],
            footer: footer("Esc 打断这一轮"),
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
        "↑↓ 选，PgUp/PgDn 翻看，Enter 确定，数字键直接选，Esc 打断这一轮"
    } else {
        "↑↓ 选，Enter 确定，数字键直接选，Esc 打断这一轮"
    };
    let position = (max_scroll > 0).then(|| {
        Line::from(vec![
            Span::styled(
                format!(" {}–{} / {} ", scroll + 1, scroll + shown, body.len()),
                Style::new().dim(),
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
fn session_note(tool: &str) -> String {
    let what = match tool {
        "shell" => "这个会话里以后所有命令都直接跑，不再问".to_owned(),
        "write" => "这个会话里以后整文件写入都不再问".to_owned(),
        "edit" => "这个会话里以后按文字替换改文件都不再问".to_owned(),
        other => format!("这个会话里以后 {} 都不再问", visible(other)),
    };
    format!("{what}；之后把权限模式改回「每次问」，这个允许照样有效。")
}

/// The whole command, then the directory it runs in.
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
    let mut rows = Vec::new();
    for line in visible(command).split('\n') {
        rows.extend(wrap(
            &LogicalLine::new(Vec::new(), vec![Span::raw(line.to_owned())]),
            width,
        ));
    }
    let cwd = if item_cwd.is_empty() {
        session_cwd
    } else {
        item_cwd
    };
    rows.extend(wrap(
        &LogicalLine::new(
            vec![Span::styled("目录：", Style::new().dim())],
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
    let diff = parse_diff(&visible(text("diff").unwrap_or_default()), kind);
    let count = |wanted| diff.iter().filter(|(kind, _)| *kind == wanted).count();
    let added = Span::styled(format!("+{}", count(DiffKind::Added)), Style::new().green());
    let removed = Span::styled(format!("−{}", count(DiffKind::Removed)), Style::new().red());
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
        rows.push(Line::from(Span::styled("（没有改动）", Style::new().dim())));
    }
    for (kind, line) in diff {
        let (marker, style) = diff_style(kind);
        let line = LogicalLine {
            indent: vec![Span::styled(marker, style)],
            hang: vec![Span::raw("  ")],
            body: vec![Span::styled(line, style)],
        };
        rows.extend(wrap(&line, width));
    }
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
