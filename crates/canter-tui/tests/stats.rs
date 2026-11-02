use canter_protocol::*;
use canter_tui::stats::{format_report, html_report};

fn report() -> StatsResponse {
    let usage = UsageRow {
        cached_input_tokens: 70,
        uncached_input_tokens: 30,
        output_tokens: 20,
        cost: UsageCost {
            usd: 0.25,
            unknown: false,
        },
        ..Default::default()
    };
    let mut report = StatsResponse {
        session_id: "parent".into(),
        ..Default::default()
    };
    report.overview.usage = Some(UsageResponse {
        data: vec![usage.clone()],
        total: usage.clone(),
        ..Default::default()
    });
    report.overview.usage_with_sub_agents = Some(UsageRow {
        output_tokens: 777,
        ..usage.clone()
    });
    report.sub_agents.push(StatsSubAgent {
        id: "child".into(),
        name: Some("agent-<child>".into()),
        agent_type: AgentType::Explorer,
        status: AgentStatus::Completed,
        request_count: 3,
        usage: Some(usage),
        tool_count: 5,
        duration_ms: Some(133_000),
    });
    report
}

#[test]
fn stats_show_server_supplied_subagent_totals_and_rows_in_text_and_html() {
    let report = report();
    for output in [format_report(&report), html_report(&report)] {
        assert!(output.contains("Usage (this session): in 100 (cached 70) · out 20 · $0.25"));
        assert!(output.contains("Usage (with sub-agents): in 100 (cached 70) · out 777 · $0.25"));
        assert!(output.contains("Sub-agents"));
        for header in [
            "Name", "Type", "Status", "Requests", "In", "Out", "Cost", "Tools", "Time",
        ] {
            assert!(output.contains(header));
        }
        assert!(output.contains("explorer"));
        assert!(output.contains("completed"));
        assert!(output.contains("2:13"));
        assert!(output.contains("Details: canter stats"));
        assert!(output.find("Sub-agents").unwrap() < output.find("Requests").unwrap());
    }
    let html = html_report(&report);
    assert!(html.contains("agent-&lt;child&gt;"));
    assert!(!html.contains("agent-<child>"));
}

#[test]
fn stats_preserve_unknown_values_child_parent_line_and_ordinary_output() {
    let mut report = report();
    report.overview.usage_with_sub_agents = None;
    report.sub_agents[0].usage = None;
    report.sub_agents[0].duration_ms = None;
    for output in [format_report(&report), html_report(&report)] {
        assert!(output.contains("Usage (with sub-agents): unknown"));
        assert!(output.matches("unknown").count() >= 5);
    }
    report.sub_agents.clear();
    report.parent = Some(SessionParent {
        session_id: "main-id".into(),
        turn_id: "t1".into(),
        task: "inspect".into(),
        agent_type: AgentType::Explorer,
    });
    for output in [format_report(&report), html_report(&report)] {
        assert!(output.contains("Parent: main-id · explorer"));
        assert!(!output.contains("Sub-agents"));
        assert!(!output.contains("Usage (with sub-agents)"));
        assert!(output.contains("Cache hit"));
    }
}
