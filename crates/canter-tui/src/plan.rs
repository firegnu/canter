//! Plan cards in the conversation and in exports.
use canter_protocol::{ItemStatus, PlanStep, PlanStepStatus};
use ratatui::{style::Style, text::Span};

use crate::{
    text::{LogicalLine, Row, sanitize, truncate, wrap_rows},
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
    } else if status == ItemStatus::Blocked {
        ("Plan update blocked (Plan mode)".into(), theme::warning())
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

/// A later update in the same turn as one row: `● Plan 1/3` and what changed
/// since `before`, each step with its card marker and style (`✔` done, `□`
/// started or added, `−` removed), cut to `width`; `● Plan 3/3 complete`
/// once all are done. `None` when the change cannot be told step by step:
/// steps added and removed at once (as when one is reworded), or the same
/// text twice; the whole card shows then.
pub(crate) fn update_row(before: &[PlanStep], plan: &[PlanStep], width: u16) -> Option<Row> {
    let known = |steps: &[PlanStep]| -> Option<Vec<(String, PlanStepStatus)>> {
        let steps: Vec<_> = steps
            .iter()
            .filter(|step| step.status != PlanStepStatus::Unknown)
            .map(|step| (text(&step.step), step.status))
            .collect();
        let mut names: Vec<&String> = steps.iter().map(|(name, _)| name).collect();
        names.sort();
        names.dedup();
        (names.len() == steps.len()).then_some(steps)
    };
    let (before, plan) = (known(before)?, known(plan)?);
    let find = |steps: &[(String, PlanStepStatus)], name: &str| {
        steps
            .iter()
            .find(|(other, _)| other == name)
            .map(|(_, s)| *s)
    };
    let removed: Vec<&String> = before
        .iter()
        .filter(|(name, _)| find(&plan, name).is_none())
        .map(|(name, _)| name)
        .collect();
    let mut changes: Vec<(String, PlanStepStatus)> = plan
        .iter()
        .filter(|(name, status)| find(&before, name) != Some(*status))
        .cloned()
        .collect();
    let added = changes
        .iter()
        .any(|(name, _)| find(&before, name).is_none());
    if added && !removed.is_empty() {
        return None;
    }
    let done = plan
        .iter()
        .filter(|(_, status)| *status == PlanStepStatus::Completed)
        .count();
    let mut spans = vec![Span::styled(
        format!("Plan {done}/{}", plan.len()),
        Style::default().bold(),
    )];
    if done == plan.len() && done > 0 {
        spans.push(Span::styled(" complete", Style::default().bold()));
        changes.clear();
    } else {
        for name in removed {
            spans.push(Span::styled(" · ", theme::secondary()));
            let style = theme::secondary().crossed_out();
            spans.push(Span::styled("− ", style));
            spans.push(Span::styled(name.clone(), style));
        }
    }
    for (name, status) in changes {
        let style = step_style(status);
        let marker = if status == PlanStepStatus::Completed {
            "✔ "
        } else {
            "□ "
        };
        spans.push(Span::styled(" · ", theme::secondary()));
        spans.push(Span::styled(marker, style));
        spans.push(Span::styled(name, style));
    }
    let spans = truncate(spans, usize::from(width).saturating_sub(4));
    wrap_rows(&LogicalLine::new(vec![Span::raw("  ● ")], spans), width)
        .into_iter()
        .next()
}

pub(crate) fn markdown(plan: &[PlanStep], explanation: Option<&str>, status: ItemStatus) -> String {
    let mut out = if status == ItemStatus::Completed {
        label(plan)
    } else if status == ItemStatus::Blocked {
        "Plan update blocked (Plan mode)".into()
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
