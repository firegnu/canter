//! Presentation only: every statistic is supplied by the server.
use crate::text::{sanitize, str_width};
use canter_protocol::*;
use std::fmt::Write;

const REQUEST_HEADERS: &[&str] = &[
    "#",
    "Time",
    "Purpose",
    "Outcome",
    "ms",
    "First ms",
    "Retries",
    "System",
    "Tool defs",
    "Dialogue",
    "Results",
    "Total",
];
const TOOL_HEADERS: &[&str] = &[
    "Tool",
    "Calls",
    "Failed",
    "Truncated",
    "Avg tokens",
    "Max tokens",
];

fn known(value: Option<impl std::fmt::Display>) -> String {
    value.map_or_else(|| "unknown".into(), |value| value.to_string())
}
fn clean(text: &str) -> String {
    sanitize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn short(text: &str, width: usize) -> String {
    let text = clean(text);
    if str_width(&text) <= width {
        return text;
    }
    let mut result = String::new();
    for c in text.chars() {
        if str_width(&result) + str_width(&c.to_string()) >= width {
            break;
        }
        result.push(c);
    }
    result.push('…');
    result
}
fn time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp).map_or_else(
        || "unknown".into(),
        |time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}
fn label(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(Into::into))
        .unwrap_or_else(|| "unknown".into())
}
fn widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
    headers
        .iter()
        .enumerate()
        .map(|(index, title)| {
            rows.iter()
                .map(|row| str_width(&clean(&row[index])))
                .max()
                .unwrap_or(0)
                .max(title.len())
        })
        .collect()
}
fn table_row(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .zip(widths)
        .map(|(value, width)| {
            let value = clean(value);
            let pad = width.saturating_sub(str_width(&value));
            format!("{value}{}", " ".repeat(pad))
        })
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}
fn table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let widths = widths(headers, rows);
    std::iter::once(table_row(
        &headers.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
        &widths,
    ))
    .chain(rows.iter().map(|row| table_row(row, &widths)))
    .collect()
}

pub fn format_list(list: &StatsListResponse) -> String {
    if list.data.is_empty() {
        return "No sessions recorded.\n".into();
    }
    let rows = list
        .data
        .iter()
        .map(|s| {
            vec![
                s.number.to_string(),
                time(s.updated_at),
                short(&s.cwd, 40),
                short(s.first_message.as_deref().unwrap_or("unknown"), 70),
            ]
        })
        .collect::<Vec<_>>();
    table(&["#", "Last activity", "Directory", "First message"], &rows).join("\n")
        + "\n\nUse canter stats <number>, --last, or a session ID. Numbers follow current activity order.\n"
}

fn overview(report: &StatsResponse) -> Vec<String> {
    let o = &report.overview;
    let tags = if o.tags.is_empty() {
        "none".into()
    } else {
        o.tags
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![
        format!("Session: {}", report.session_id),
        format!("Directory: {}", o.cwd),
        format!("Model: {}", o.model),
        format!(
            "Time range: {} — {} (local)",
            time(o.started_at),
            time(o.updated_at)
        ),
        format!("Tags: {tags}"),
        format!(
            "Turns: {}  Recorded requests: {}  Retries: {}  Public retry records: {}",
            o.turn_count,
            o.request_count,
            known(o.retry_count),
            o.recorded_retry_count
        ),
    ];
    if let Some(usage) = &o.usage {
        lines.extend(crate::usage::format_report(usage));
    } else {
        lines.push("Usage and cost: unknown (no usage recorded).".into());
    }
    lines.push("Context and result sizes are estimated tokens. Unknown values are not zero. Signals do not determine information loss.".into());
    lines
}
fn request_cells(number: usize, row: &StatsRequest) -> Vec<String> {
    let context = row.context.as_ref();
    vec![
        number.to_string(),
        time(row.started_at),
        label(&row.purpose),
        label(&row.outcome),
        known(row.duration_ms),
        known(row.first_response_ms),
        known(row.retry_count),
        known(context.map(|c| c.system_tokens)),
        known(context.map(|c| c.tool_definition_tokens)),
        known(context.map(|c| c.conversation_tokens)),
        known(context.map(|c| c.tool_result_tokens)),
        known(row.context_total_tokens),
    ]
}
fn tool_cells(row: &StatsTool) -> Vec<String> {
    vec![
        row.name.clone(),
        row.call_count.to_string(),
        known(row.failure_count),
        known(row.truncated_count),
        row.average_result_tokens
            .map_or_else(|| "unknown".into(), |v| format!("{v:.1}")),
        known(row.max_result_tokens),
    ]
}
fn files(label: &str, paths: &Option<Vec<String>>) -> String {
    match paths {
        None => format!("{label}: unknown"),
        Some(paths) => format!(
            "{label}: {}{}",
            paths.len(),
            if paths.is_empty() {
                String::new()
            } else {
                format!(" — {}", paths.join(", "))
            }
        ),
    }
}
fn plan_steps(steps: &[PlanStep]) -> String {
    if steps.is_empty() {
        return "(empty)".into();
    }
    steps
        .iter()
        .map(|step| format!("{} [{}]", step.step, label(&step.status)))
        .collect::<Vec<_>>()
        .join("; ")
}
fn compaction_lines(row: &StatsCompaction) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Compaction {} at {}: {}; trigger occupancy {}; before {} → after {}; removed messages {}; succeeded {}",
            row.item_id,
            time(row.timestamp),
            label(&row.trigger),
            known(row.trigger_occupancy_tokens),
            known(row.before_tokens),
            known(row.after_tokens),
            known(row.removed_messages),
            known(row.succeeded)
        ),
        files("Reread files", &row.signals.reread_files),
        files(
            "Changed files not mentioned",
            &row.signals.unmentioned_files,
        ),
    ];
    let plan = &row.signals.plan;
    if let Some(before) = &plan.before {
        lines.push(format!("Plan before: {}", plan_steps(before)));
        lines.push(format!(
            "Plan after: {}",
            plan.after.as_ref().map_or_else(
                || "unknown (no update before the next compaction)".into(),
                |steps| plan_steps(steps)
            )
        ));
        lines.push(format!(
            "Removed steps: {}",
            plan.removed
                .as_ref()
                .map_or_else(|| "unknown".into(), |steps| plan_steps(steps))
        ));
        lines.push(format!(
            "Status changes: {}",
            plan.status_changed.as_ref().map_or_else(
                || "unknown".into(),
                |changes| {
                    if changes.is_empty() {
                        "none".into()
                    } else {
                        changes
                            .iter()
                            .map(|c| {
                                format!("{}: {} → {}", c.step, label(&c.before), label(&c.after))
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    }
                }
            )
        ));
    } else {
        lines.push(
            if row.succeeded == Some(false) {
                "Plan comparison unavailable: compaction failed."
            } else if row.summary.is_none() {
                "Plan comparison unavailable: compaction has not finished."
            } else {
                "No plan before compaction."
            }
            .into(),
        );
        if let Some(after) = &plan.after {
            lines.push(format!("Plan after: {}", plan_steps(after)));
        }
    }
    lines.push(format!(
        "Summary: {}",
        row.summary.as_deref().unwrap_or("unknown")
    ));
    lines
}
fn approvals(report: &StatsResponse) -> String {
    format!(
        "Approvals: {}  Total wait: {} ms  Longest wait: {} ms",
        report.approvals.count,
        known(report.approvals.total_duration_ms),
        known(report.approvals.max_duration_ms)
    )
}

enum Timeline<'a> {
    Request(usize, &'a StatsRequest),
    Compaction(&'a StatsCompaction),
}
fn timeline(report: &StatsResponse) -> Vec<Timeline<'_>> {
    let mut rows: Vec<_> = report
        .requests
        .iter()
        .enumerate()
        .map(|(i, r)| (r.started_at, Timeline::Request(i + 1, r)))
        .chain(
            report
                .compactions
                .iter()
                .map(|c| (c.timestamp, Timeline::Compaction(c))),
        )
        .collect();
    rows.sort_by_key(|(timestamp, _)| *timestamp);
    rows.into_iter().map(|(_, row)| row).collect()
}

pub fn format_report(report: &StatsResponse) -> String {
    let mut lines = overview(report);
    lines.push("\nRequests".into());
    let rows: Vec<_> = report
        .requests
        .iter()
        .enumerate()
        .map(|(i, r)| request_cells(i + 1, r))
        .collect();
    let widths = widths(REQUEST_HEADERS, &rows);
    lines.push(table_row(
        &REQUEST_HEADERS
            .iter()
            .map(|v| (*v).into())
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in timeline(report) {
        match row {
            Timeline::Request(i, row) => lines.push(table_row(&request_cells(i, row), &widths)),
            Timeline::Compaction(row) => lines.extend(
                compaction_lines(row)
                    .into_iter()
                    .map(|line| format!("  {line}")),
            ),
        }
    }
    if report.requests.is_empty() {
        lines.push("No requests recorded.".into());
    }
    lines.push("\nTools".into());
    lines.extend(table(
        TOOL_HEADERS,
        &report.tools.iter().map(tool_cells).collect::<Vec<_>>(),
    ));
    if report.tools.is_empty() {
        lines.push("No tool calls recorded.".into());
    }
    lines.push(format!("\n{}", approvals(report)));
    lines
        .into_iter()
        .map(|line| sanitize(&line))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn html_cells(tag: &str, cells: &[String]) -> String {
    format!(
        "<tr>{}</tr>",
        cells
            .iter()
            .map(|cell| format!("<{tag}>{}</{tag}>", escape(cell)))
            .collect::<String>()
    )
}
fn html_header(headers: &[&str]) -> String {
    format!(
        "<thead>{}</thead><tbody>",
        html_cells(
            "th",
            &headers.iter().map(|v| (*v).into()).collect::<Vec<_>>()
        )
    )
}

pub fn html_report(report: &StatsResponse) -> String {
    let mut html = String::from(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><title>Canter session stats</title><style>
:root{color-scheme:light dark;font:15px/1.55 system-ui,sans-serif;background:#f5f7fa;color:#202b3d}body{max-width:1280px;margin:40px auto;padding:0 24px}h1{font-size:30px;margin-bottom:4px}h2{margin-top:36px}p{white-space:pre-wrap;overflow-wrap:anywhere}.panel{background:#fff;border:1px solid #cbd4df;border-radius:12px;padding:20px;margin:20px 0}.scroll{overflow-x:auto}table{border-collapse:collapse;width:100%;font-size:13px}th,td{text-align:left;padding:10px 12px;border-bottom:1px solid #d5dce6;white-space:nowrap;font-variant-numeric:tabular-nums}th{font-weight:600;background:#edf1f7}.compaction td{white-space:normal;background:#fff5e4}.compaction p{margin:5px 0}.legend{display:flex;flex-wrap:wrap;gap:20px}.legend span:before{content:' ';display:inline-block;width:12px;height:12px;margin-right:6px;background:var(--color)}svg{width:100%;height:auto;min-width:480px}svg text{fill:currentColor;font:12px system-ui}svg .axis{stroke:#8090a5}svg .marker{stroke:#9a4d00;stroke-width:2;stroke-dasharray:5 4}
@media(prefers-color-scheme:dark){:root{background:#111823;color:#e3e9f3}.panel{background:#1b2534;border-color:#435168}th{background:#26344a}th,td{border-color:#435168}.compaction td{background:#403119}svg .marker{stroke:#ffca7d}}
</style><body><h1>Session stats</h1><div class="panel">"#,
    );
    for line in overview(report) {
        let _ = write!(html, "<p>{}</p>", escape(&line));
    }
    html.push_str("</div><h2>Context across requests</h2><p>Estimated tokens. Unknown measurements are gaps. Request numbers match the table.</p><div class=\"panel scroll\">");
    html.push_str(&chart(report));
    html.push_str("<div class=\"legend\"><span style=\"--color:#4778d6\">System / fixed prefix</span><span style=\"--color:#b375cf\">Tool definitions</span><span style=\"--color:#3a9c85\">Dialogue</span><span style=\"--color:#d59a39\">Tool results</span></div></div><h2>Requests</h2><div class=\"panel scroll\"><table>");
    html.push_str(&html_header(REQUEST_HEADERS));
    for row in timeline(report) {
        match row {
            Timeline::Request(i, row) => html.push_str(&html_cells("td", &request_cells(i, row))),
            Timeline::Compaction(row) => {
                html.push_str("<tr class=\"compaction\"><td colspan=\"12\">");
                for line in compaction_lines(row) {
                    let _ = write!(html, "<p>{}</p>", escape(&line));
                }
                html.push_str("</td></tr>");
            }
        }
    }
    html.push_str("</tbody></table></div><h2>Tools</h2><div class=\"panel scroll\"><table>");
    html.push_str(&html_header(TOOL_HEADERS));
    for row in &report.tools {
        html.push_str(&html_cells("td", &tool_cells(row)));
    }
    let _ = writeln!(
        html,
        "</tbody></table></div><h2>Approvals</h2><p>{}</p></body></html>",
        escape(&approvals(report))
    );
    html
}

fn chart(report: &StatsResponse) -> String {
    let max = report
        .requests
        .iter()
        .filter_map(|r| r.context_total_tokens)
        .max()
        .unwrap_or(0)
        .max(1) as f64;
    let x = |index: f64| 65.0 + index * 900.0 / report.requests.len().max(1) as f64;
    let y = |value: u64| 280.0 - value as f64 / max * 240.0;
    let mut svg = format!(
        "<svg viewBox=\"0 0 1000 330\" role=\"img\" aria-labelledby=\"chart-title\"><title id=\"chart-title\">Estimated context tokens by request, with compaction markers</title><path class=\"axis\" fill=\"none\" d=\"M65 40 V280 H965\"/><text x=\"5\" y=\"45\">{max:.0}</text><text x=\"40\" y=\"284\">0</text><text x=\"65\" y=\"320\">Request 1</text><text x=\"850\" y=\"320\">Request {}</text>",
        report.requests.len()
    );
    let mut start = 0;
    while start < report.requests.len() {
        if report.requests[start].context.is_none() {
            start += 1;
            continue;
        }
        let end = start
            + report.requests[start..]
                .iter()
                .take_while(|r| r.context.is_some())
                .count();
        for (layer, color) in ["#4778d6", "#b375cf", "#3a9c85", "#d59a39"]
            .iter()
            .enumerate()
        {
            let mut lower = Vec::new();
            let mut upper = Vec::new();
            for index in start..end {
                let c = report.requests[index].context.as_ref().unwrap();
                let values = [
                    c.system_tokens,
                    c.tool_definition_tokens,
                    c.conversation_tokens,
                    c.tool_result_tokens,
                ];
                let bottom: u64 = values[..layer].iter().sum();
                for offset in if end - start == 1 {
                    vec![0.35, 0.65]
                } else {
                    vec![0.5]
                } {
                    lower.push(format!("{:.1},{:.1}", x(index as f64 + offset), y(bottom)));
                    upper.push(format!(
                        "{:.1},{:.1}",
                        x(index as f64 + offset),
                        y(bottom + values[layer])
                    ));
                }
            }
            upper.reverse();
            lower.extend(upper);
            let _ = write!(
                svg,
                "<polygon fill=\"{color}\" points=\"{}\"/>",
                lower.join(" ")
            );
        }
        start = end;
    }
    for c in &report.compactions {
        let index = report
            .requests
            .partition_point(|r| r.started_at <= c.timestamp);
        let px = x(index as f64);
        let _ = write!(
            svg,
            "<g><title>Compaction: {} ({})</title><line class=\"marker\" x1=\"{px:.1}\" x2=\"{px:.1}\" y1=\"35\" y2=\"285\"/><text x=\"{px:.1}\" y=\"22\" text-anchor=\"middle\">C</text></g>",
            escape(&c.item_id),
            label(&c.trigger)
        );
    }
    svg.push_str("</svg>");
    svg
}
