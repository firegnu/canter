//! Shared CLI and conversation rendering. Prices and arithmetic stay on the server.
use crate::text::{sanitize, str_width};
use canter_protocol::{UsageCost, UsageResponse, UsageRow};

pub(crate) fn dollars(usd: f64) -> String {
    if usd > 0.0 && usd < 0.01 {
        "<$0.01".into()
    } else {
        format!("${usd:.2}")
    }
}

fn cost(value: &UsageCost) -> String {
    if value.unknown && value.usd == 0.0 {
        "unknown".into()
    } else {
        format!(
            "{}{}",
            dollars(value.usd),
            if value.unknown { "+?" } else { "" }
        )
    }
}

pub fn format_report(report: &UsageResponse) -> Vec<String> {
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
            row.cached_input_tokens,
            row.uncached_input_tokens,
            row.output_tokens,
            cost(&row.cost)
        ));
    }
    if let Some(last) = &report.last_turn_cost {
        lines.push(format!("Last turn: {}", cost(last)));
    }
    lines
}
