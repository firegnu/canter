use crate::{
    report::{Part, Report},
    theme,
};
use canter_protocol::{GoalSetParams, GoalStatus, SessionGoal};
use ratatui::{style::Style, text::Span};

#[derive(Debug, Clone, PartialEq)]
pub enum GoalCommand {
    Read,
    Set(GoalSetParams),
    Pause,
    Resume,
    Clear,
}

pub(crate) fn parse(args: &str) -> Result<GoalCommand, String> {
    match args.trim() {
        "" => return Ok(GoalCommand::Read),
        "pause" => return Ok(GoalCommand::Pause),
        "resume" => return Ok(GoalCommand::Resume),
        "clear" => return Ok(GoalCommand::Clear),
        _ => {}
    }
    let usage =
        "Usage: /goal [--budget <non-negative USD>] <objective>, or /goal pause|resume|clear";
    let (objective, budget_usd) = if args.starts_with("--budget") {
        let rest = args
            .strip_prefix("--budget")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .ok_or(usage)?
            .trim_start();
        let (amount, objective) = rest.split_once(char::is_whitespace).ok_or(usage)?;
        let amount: f64 = amount.parse().map_err(|_| usage)?;
        if !amount.is_finite() || amount < 0.0 || objective.trim().is_empty() {
            return Err(usage.into());
        }
        (objective.trim(), Some(amount))
    } else {
        (args.trim(), None)
    };
    Ok(GoalCommand::Set(GoalSetParams {
        objective: objective.into(),
        budget_usd,
    }))
}

pub(crate) fn status(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Complete => "complete",
        GoalStatus::Blocked => "blocked",
        GoalStatus::BudgetLimited => "budget limited",
        GoalStatus::Unknown => "unknown",
    }
}

pub(crate) fn bar(goal: &SessionGoal) -> String {
    let budget = if goal.budget_usd == 0.0 {
        "unlimited".into()
    } else {
        format!("${:.2}", goal.budget_usd)
    };
    let state = if goal.status == GoalStatus::Active {
        String::new()
    } else {
        format!(" {}", status(goal.status))
    };
    format!(
        "goal{state} ${:.2}{}/{budget}",
        goal.cost.usd,
        if goal.cost.unknown { "+?" } else { "" }
    )
}

pub(crate) fn popup(goal: Option<&SessionGoal>) -> Report {
    let body = match goal {
        Some(goal) => vec![
            Part::Wrap {
                indent: 0,
                text: crate::text::sanitize(&goal.objective),
                style: Style::new(),
                rows: usize::MAX,
            },
            Part::blank(),
            Part::Line(vec![Span::raw(format!("Status: {}", status(goal.status)))]),
            Part::Line(vec![Span::raw(bar(goal))]),
        ],
        None => vec![Part::Line(vec![Span::raw("No goal set.")])],
    };
    Report::new(
        vec![Span::styled("Goal", Style::new().bold())],
        body,
        vec![
            vec![Span::styled(
                "/goal [--budget USD] <objective> · 0 = unlimited",
                theme::secondary(),
            )],
            vec![Span::styled(
                "/goal pause · /goal resume · /goal clear",
                theme::secondary(),
            )],
        ],
    )
}
