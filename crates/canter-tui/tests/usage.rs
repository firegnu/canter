use canter_protocol::{UsageCost, UsageResponse, UsageRow};
use canter_tui::usage::format_report;

fn row(model: &str, usd: f64, unknown: bool, chatgpt_plan: bool) -> UsageRow {
    UsageRow {
        model: Some(model.into()),
        cached_input_tokens: 40,
        uncached_input_tokens: 60,
        output_tokens: 5,
        cost: UsageCost {
            usd,
            unknown,
            chatgpt_plan,
        },
        ..Default::default()
    }
}

fn report(rows: Vec<UsageRow>) -> UsageResponse {
    let mut report = UsageResponse {
        data: rows.clone(),
        ..Default::default()
    };
    for row in rows {
        report.total.add(&row);
    }
    report
}

#[test]
fn usage_report_shows_plan_only_and_its_usage_link() {
    let mut report = report(vec![row("subscription/gpt", 0.0, false, true)]);
    report.last_turn_cost = Some(report.total.cost.clone());
    let lines = format_report(&report).join("\n");
    assert!(lines.contains("subscription/gpt"));
    assert!(lines.contains("ChatGPT plan"));
    assert!(!lines.contains("$0.00"));
    assert!(!lines.contains("unknown"));
    assert!(lines.contains("Last turn: ChatGPT plan"));
    assert!(lines.contains("ChatGPT plan usage: https://chatgpt.com/settings/usage"));
}

#[test]
fn usage_report_combines_metered_known_and_unknown_costs_with_plan() {
    for (unknown, expected) in [
        (false, "$0.12 + ChatGPT plan"),
        (true, "$0.12+? + ChatGPT plan"),
    ] {
        let lines = format_report(&report(vec![
            row("subscription/gpt", 0.0, false, true),
            row("metered/fake", 0.12, unknown, false),
        ]))
        .join("\n");
        assert!(lines.contains(expected), "{lines}");
        assert!(lines.contains("Total"));
        assert!(lines.contains("https://chatgpt.com/settings/usage"));
    }
}

#[test]
fn usage_report_without_plan_keeps_legacy_cost_text_and_has_no_link() {
    let lines = format_report(&report(vec![row("metered/unknown", 0.0, true, false)])).join("\n");
    assert!(lines.contains("unknown"));
    assert!(!lines.contains("ChatGPT plan"));
    assert!(!lines.contains("chatgpt.com/settings/usage"));
}
