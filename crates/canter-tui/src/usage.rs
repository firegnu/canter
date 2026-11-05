//! Shared CLI and conversation rendering. Prices and arithmetic stay on the server.
use crate::report::{Part, Report, title};
use crate::text::{sanitize, str_width};
use canter_protocol::{UsageCost, UsageResponse, UsageRow};
use ratatui::text::Span;

pub(crate) fn dollars(usd: f64) -> String {
    if usd > 0.0 && usd < 0.01 {
        "<$0.01".into()
    } else {
        format!("${usd:.2}")
    }
}

pub(crate) fn cost(value: &UsageCost) -> String {
    let metered = if value.unknown && value.usd == 0.0 {
        "unknown".into()
    } else {
        format!(
            "{}{}",
            dollars(value.usd),
            if value.unknown { "+?" } else { "" }
        )
    };
    if value.chatgpt_plan {
        if metered == "$0.00" {
            "ChatGPT plan".into()
        } else {
            format!("{metered} + ChatGPT plan")
        }
    } else {
        metered
    }
}

pub(crate) const PLAN_USAGE: &str = "ChatGPT plan usage: https://chatgpt.com/settings/usage";

/// The table and the last turn's cost, token counts in full, for the CLI.
pub fn format_report(report: &UsageResponse) -> Vec<String> {
    let mut lines = table(report, |count| count.to_string());
    lines.extend(last_turn(report));
    lines
}

/// `/usage` above the input box until Esc or Enter: the table, token
/// counts as `/context` writes them (`340k`, `1.2M`), then the last turn's
/// cost.
pub(crate) fn popup(report: &UsageResponse) -> Report {
    Report::new(
        title("Usage", ""),
        table(report, crate::app::tokens)
            .into_iter()
            .map(|line| Part::Line(vec![Span::raw(format!(" {line}"))]))
            .collect(),
        last_turn(report)
            .into_iter()
            .map(|line| vec![Span::raw(format!(" {line}"))])
            .collect(),
    )
}

fn last_turn(report: &UsageResponse) -> Option<String> {
    let last = report.last_turn_cost.as_ref()?;
    (!report.data.is_empty()).then(|| format!("Last turn: {}", cost(last)))
}

/// A row for each model (and date), then the total; `count` writes a
/// token count.
fn table(report: &UsageResponse, count: impl Fn(u64) -> String) -> Vec<String> {
    if report.data.is_empty() {
        return vec!["No usage recorded.".into()];
    }
    let daily = report.data.iter().any(|row| row.date.is_some());
    let label =
        |row: &UsageRow| sanitize(row.model.as_deref().unwrap_or("Total")).replace('\n', " ");
    let width = report
        .data
        .iter()
        .map(|row| str_width(&label(row)))
        .max()
        .unwrap_or(0)
        .max(5);
    let heading = if daily { "Date        " } else { "" };
    let mut lines = vec![format!(
        "{heading}{:<width$}  {:>10}  {:>10}  {:>10}  Cost (USD)",
        "Model", "Cache hit", "Cache miss", "Output"
    )];
    for row in report.data.iter().chain(std::iter::once(&report.total)) {
        let name = if daily && row.model.is_none() {
            String::new()
        } else {
            label(row)
        };
        let date = if daily {
            format!("{:<10}  ", row.date.as_deref().unwrap_or("Total"))
        } else {
            String::new()
        };
        lines.push(format!(
            "{date}{name}{}  {:>10}  {:>10}  {:>10}  {}",
            " ".repeat(width.saturating_sub(str_width(&name))),
            count(row.cached_input_tokens),
            count(row.uncached_input_tokens),
            count(row.output_tokens),
            cost(&row.cost)
        ));
    }
    if report.total.cost.chatgpt_plan {
        lines.push(PLAN_USAGE.into());
    }
    lines
}
