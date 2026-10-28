use crate::{
    approval::ApprovalBox,
    markdown,
    text::{LogicalLine, sanitize, wrap},
    theme,
};
use canter_protocol::{ItemStatus, PermissionMode, PlanAnswer, PlanApprovalRequest, PlanDecision};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

pub(crate) fn result_label(answer: Option<&PlanAnswer>, status: ItemStatus) -> &'static str {
    match answer.map(|answer| answer.decision) {
        Some(PlanDecision::Implement) => "Plan approved",
        Some(PlanDecision::ClearContext) => "Plan approved · clear context",
        Some(PlanDecision::KeepPlanning) => "Keep planning",
        _ => match status {
            ItemStatus::Blocked => "Plan blocked",
            ItemStatus::Failed => "Plan failed",
            ItemStatus::Declined => "Plan approval cancelled",
            _ => "Proposed plan",
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn layout(
    plan: &PlanApprovalRequest,
    mode: PermissionMode,
    selected: usize,
    custom: Option<&str>,
    scroll: usize,
    taken: bool,
    width: u16,
    height: u16,
) -> ApprovalBox {
    let inner = width.saturating_sub(4).max(1);
    let body: Vec<_> = markdown::render(&sanitize(&plan.plan), inner)
        .iter()
        .flat_map(|line| wrap(line, inner))
        .collect();
    let mut tail = vec![
        Line::default(),
        Line::from(vec![
            Span::styled(
                format!("Mode: {}", crate::app::mode_label(mode).0),
                Style::new().bold(),
            ),
            Span::styled("   (shift+tab to change)", theme::secondary()),
        ]),
    ];
    if let Some(text) = custom {
        tail.push(Line::from("Feedback (optional):"));
        let input = wrap(
            &LogicalLine::new(
                Vec::new(),
                vec![Span::raw(format!("› {}▏", sanitize(text)))],
            ),
            inner,
        );
        let room = usize::from(height.saturating_sub(8)).max(1);
        tail.extend(
            input
                .into_iter()
                .rev()
                .take(room)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
    } else {
        for (index, label) in [
            "Yes, implement this plan",
            "Yes, clear context and implement",
            "No, keep planning",
        ]
        .iter()
        .enumerate()
        {
            tail.extend(wrap(
                &LogicalLine::new(
                    vec![Span::raw(if index == selected { "› " } else { "  " })],
                    vec![Span::styled(
                        format!("{}. {label}", index + 1),
                        if index == selected {
                            Style::new().bold()
                        } else {
                            Style::new()
                        },
                    )],
                ),
                inner,
            ));
        }
    }
    if !taken {
        for line in &mut tail {
            *line = line.clone().patch_style(theme::secondary());
        }
    }
    let room = usize::from(height).saturating_sub(tail.len() + 2);
    let page = room.min(body.len()).max(1);
    let max_scroll = body.len().saturating_sub(room);
    let scroll = scroll.min(max_scroll);
    let rows = if room == 0 {
        vec![Line::from(
            " Window too small for the plan approval; make it taller to choose",
        )]
    } else {
        body.into_iter()
            .skip(scroll)
            .take(room)
            .chain(tail)
            .map(|mut row| {
                row.spans.insert(0, Span::raw(" "));
                row
            })
            .collect()
    };
    ApprovalBox {
        title: Line::from(Span::styled("─ Implement this plan? ", Style::new().bold())),
        position: (max_scroll > 0).then(|| {
            Line::from(format!(
                " {}–{} / {} ",
                scroll + 1,
                scroll + page,
                max_scroll + page
            ))
            .right_aligned()
        }),
        rows,
        footer: Line::from(if custom.is_some() {
            "─ Enter send · Backspace/↑ back if empty · PgUp/PgDn scroll ─"
        } else {
            "─ ↑↓ select · Enter confirm · 1–3 pick · PgUp/PgDn scroll ─"
        }),
        page,
        max_scroll,
        fits: room > 0,
    }
}
