//! Plan cards and the compact checklist above the input box.
use canter_protocol::{ItemStatus, PlanStep, PlanStepStatus, SessionSnapshot};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::{
    text::{LogicalLine, Row, sanitize, str_width, truncate, wrap_rows},
    theme,
};

fn label(plan: &[PlanStep]) -> String {
    let completed = plan
        .iter()
        .filter(|step| step.status == PlanStepStatus::Completed)
        .count();
    format!("Updated Plan · {completed}/{} complete", plan.len())
}

fn step_style(status: PlanStepStatus) -> Style {
    match status {
        PlanStepStatus::Completed => theme::secondary().crossed_out(),
        PlanStepStatus::InProgress => theme::accent().bold(),
        _ => Style::default(),
    }
}

fn text(step: &str) -> String {
    sanitize(step)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn card(
    plan: &[PlanStep],
    explanation: Option<&str>,
    status: ItemStatus,
    width: u16,
) -> Vec<Row> {
    if status == ItemStatus::InProgress {
        return Vec::new();
    }
    let (title, style) = if status == ItemStatus::Completed {
        (label(plan), Style::default().bold())
    } else {
        ("Plan update failed".into(), theme::failure())
    };
    let mut rows = wrap_rows(
        &LogicalLine::new(vec![Span::raw("  ● ")], vec![Span::styled(title, style)]),
        width,
    );
    if let Some(explanation) = explanation.filter(|s| !s.trim().is_empty()) {
        rows.extend(wrap_rows(
            &LogicalLine::new(
                vec![Span::raw("    ")],
                vec![Span::styled(text(explanation), theme::secondary())],
            ),
            width,
        ));
    }
    for step in plan
        .iter()
        .filter(|step| step.status != PlanStepStatus::Unknown)
    {
        let style = step_style(step.status);
        let marker = if step.status == PlanStepStatus::Completed {
            "✔ "
        } else {
            "□ "
        };
        rows.extend(wrap_rows(
            &LogicalLine::new(
                vec![Span::raw("    "), Span::styled(marker, style)],
                vec![Span::styled(text(&step.step), style)],
            ),
            width,
        ));
    }
    rows
}

pub(crate) fn pinned(snapshot: &SessionSnapshot, width: u16, room: usize) -> Vec<Line<'static>> {
    if snapshot.settings.plan_enabled == Some(false) || room == 0 {
        return Vec::new();
    }
    let Some(plan) = snapshot.current_plan() else {
        return Vec::new();
    };
    let steps: Vec<_> = plan
        .iter()
        .filter(|step| step.status == PlanStepStatus::InProgress)
        .chain(
            plan.iter()
                .filter(|step| step.status == PlanStepStatus::Pending),
        )
        .collect();
    let limit = room.min(5);
    if limit == 1 && steps.len() > 1 {
        let step = steps[0];
        let suffix = format!("（还有 {} 步）", steps.len() - 1);
        let mut spans = truncate(
            vec![
                Span::raw("  "),
                Span::styled(format!("□ {}", text(&step.step)), step_style(step.status)),
            ],
            usize::from(width).saturating_sub(str_width(&suffix)),
        );
        spans.push(Span::styled(suffix, theme::secondary()));
        return vec![Line::from(truncate(spans, usize::from(width)))];
    }
    let shown = if steps.len() > limit {
        limit - 1
    } else {
        steps.len()
    };
    let mut rows: Vec<_> = steps
        .iter()
        .take(shown)
        .map(|step| {
            Line::from(truncate(
                vec![
                    Span::raw("  "),
                    Span::styled(format!("□ {}", text(&step.step)), step_style(step.status)),
                ],
                usize::from(width),
            ))
        })
        .collect();
    if steps.len() > shown {
        rows.push(Line::from(truncate(
            vec![Span::styled(
                format!("  还有 {} 步", steps.len() - shown),
                theme::secondary(),
            )],
            usize::from(width),
        )));
    }
    rows
}

pub(crate) fn markdown(plan: &[PlanStep], explanation: Option<&str>, status: ItemStatus) -> String {
    let mut out = if status == ItemStatus::Completed {
        label(plan)
    } else {
        "Plan update failed".into()
    };
    if let Some(explanation) = explanation.filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("\n\n{}", text(explanation)));
    }
    out.push('\n');
    for step in plan {
        let step_text = text(&step.step);
        let line = match step.status {
            PlanStepStatus::Completed => format!("- [x] ~~{step_text}~~"),
            PlanStepStatus::InProgress => format!("- [ ] **{step_text}**"),
            PlanStepStatus::Pending => format!("- [ ] {step_text}"),
            PlanStepStatus::Unknown => continue,
        };
        out.push_str(&format!("\n{line}"));
    }
    out
}
