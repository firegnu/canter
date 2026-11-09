//! Turn `canter stats --json` and the session events into one run result.

use std::{collections::BTreeMap, path::PathBuf};

use canter_protocol::{Event, EventPayload, Item, ItemStatus, RequestPurpose, StatsResponse};
use serde::{Deserialize, Serialize};

use crate::{Result, grade::CheckResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionRow {
    pub trigger: Option<String>,
    pub before_tokens: Option<u64>,
    pub after_tokens: Option<u64>,
    pub succeeded: Option<bool>,
    pub reread_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub duration_ms: Option<u64>,
    pub requests: usize,
    pub conversation_requests: usize,
    pub compactions: Vec<CompactionRow>,
    pub cached_input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub cost_unknown: bool,
    pub compaction_cost_usd: Option<f64>,
    pub read_calls: usize,
    pub update_plan_calls: usize,
    pub reread_files_after_compaction: usize,
    pub tool_failures: usize,
    pub tool_failures_by_name: BTreeMap<String, usize>,
    pub guard_reminders: usize,
    pub blocked_commands: usize,
    pub event_parse_errors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub task: String,
    pub round: String,
    pub run: String,
    pub session_id: String,
    pub model: String,
    pub canter_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub completed_at: i64,
    pub checks: Vec<CheckResult>,
    pub checks_passed: usize,
    pub checks_total: usize,
    pub metrics: Metrics,
}

impl RunResult {
    pub fn load(home: &crate::paths::Home, round: &str, run: &str) -> Result<Self> {
        let path = home.run_dir(round, run).join("result.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display()))
    }
}

pub struct CollectOptions {
    pub home: crate::paths::Home,
    pub round: String,
    pub run: String,
    pub stats_json: Option<PathBuf>,
    pub session: Option<String>,
}

pub fn collect(options: CollectOptions) -> Result<RunResult> {
    let state = crate::paths::State::load(&options.home, &options.round, &options.run)?;
    let checks = crate::grade::load(&options.home, &options.round, &options.run)?;
    let stats = stats(&options, &state)?;
    let session_id = stats.session_id.clone();
    let events =
        std::fs::read_to_string(options.home.session_events(&session_id)).map_err(|error| {
            format!("cannot read the events of {session_id}: {error}\nthe eval home may be wrong")
        })?;
    let (guard_reminders, blocked_commands, event_parse_errors) = event_counts(&events);
    let metrics = metrics(
        &stats,
        guard_reminders,
        blocked_commands,
        event_parse_errors,
    );
    let registration = crate::server::registration(&options.home).ok();
    let checks_passed = checks
        .results
        .iter()
        .filter(|check| check.passed == Some(true))
        .count();
    let checks_total = checks.results.iter().filter(|check| !check.record).count();
    let result = RunResult {
        task: state.task.clone(),
        round: options.round.clone(),
        run: options.run.clone(),
        session_id,
        model: state.model.clone(),
        canter_version: registration.as_ref().map(|row| row.version.clone()),
        protocol_version: registration.as_ref().map(|row| row.protocol_version),
        completed_at: crate::now_ms(),
        checks: checks.results.clone(),
        checks_passed,
        checks_total,
        metrics,
    };
    crate::paths::write_private(
        &options
            .home
            .run_dir(&options.round, &options.run)
            .join("result.json"),
        serde_json::to_vec_pretty(&result)
            .map_err(|error| format!("cannot encode the run result: {error}"))?
            .as_slice(),
    )?;
    Ok(result)
}

fn stats(options: &CollectOptions, state: &crate::paths::State) -> Result<StatsResponse> {
    let text = match &options.stats_json {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        None => {
            let local = crate::paths::Local::load(&options.home)?;
            let session = options
                .session
                .clone()
                .or_else(|| read_session(&state.session_file))
                .ok_or_else(|| {
                    format!(
                        "no session id yet; the run has not written {}",
                        state.session_file
                    )
                })?;
            crate::server::run_canter(
                &crate::server::canter_binary(&local),
                &["stats", &session, "--json"],
                &options.home,
            )?
        }
    };
    serde_json::from_str::<StatsResponse>(&text)
        .map_err(|error| format!("the stats JSON is not valid: {error}"))
}

fn read_session(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn metrics(
    stats: &StatsResponse,
    guard_reminders: usize,
    blocked_commands: usize,
    event_parse_errors: usize,
) -> Metrics {
    let usage = stats.overview.usage.as_ref();
    let duration_ms = (stats.overview.updated_at >= stats.overview.started_at).then(|| {
        u64::try_from(stats.overview.updated_at - stats.overview.started_at).unwrap_or(u64::MAX)
    });
    let compactions: Vec<CompactionRow> = stats
        .compactions
        .iter()
        .map(|compaction| CompactionRow {
            trigger: serde_json::to_value(compaction.trigger)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string)),
            before_tokens: compaction.before_tokens,
            after_tokens: compaction.after_tokens,
            succeeded: compaction.succeeded,
            reread_files: compaction.signals.reread_files.as_ref().map_or(0, Vec::len),
        })
        .collect();
    let mut tool_failures_by_name = BTreeMap::new();
    let mut tool_failures = 0;
    for tool in &stats.tools {
        if let Some(failures) = tool.failure_count
            && failures > 0
        {
            tool_failures += failures;
            tool_failures_by_name.insert(tool.name.clone(), failures);
        }
    }
    let cost = usage.map(|row| &row.total.cost);
    let compaction_cost: Option<f64> = stats
        .requests
        .iter()
        .filter(|request| request.purpose == RequestPurpose::Compaction)
        .filter_map(|request| request.cost.as_ref().map(|cost| cost.usd))
        .reduce(|total, value| total + value);
    Metrics {
        duration_ms,
        requests: stats.overview.request_count,
        conversation_requests: stats
            .requests
            .iter()
            .filter(|request| request.purpose == RequestPurpose::Conversation)
            .count(),
        compactions,
        cached_input_tokens: usage.map(|row| row.total.cached_input_tokens),
        uncached_input_tokens: usage.map(|row| row.total.uncached_input_tokens),
        output_tokens: usage.map(|row| row.total.output_tokens),
        reasoning_output_tokens: stats
            .requests
            .iter()
            .filter_map(|request| request.usage.as_ref())
            .map(|usage| usage.reasoning_output_tokens)
            .fold(0u64, u64::saturating_add),
        cost_usd: cost.map(|cost| cost.usd),
        cost_unknown: cost.is_none_or(|cost| cost.unknown),
        compaction_cost_usd: compaction_cost,
        read_calls: tool_calls(stats, "read"),
        update_plan_calls: tool_calls(stats, "update_plan"),
        reread_files_after_compaction: stats
            .compactions
            .iter()
            .map(|compaction| compaction.signals.reread_files.as_ref().map_or(0, Vec::len))
            .sum(),
        tool_failures,
        tool_failures_by_name,
        guard_reminders,
        blocked_commands,
        event_parse_errors,
    }
}

fn tool_calls(stats: &StatsResponse, name: &str) -> usize {
    stats
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .map_or(0, |tool| tool.call_count)
}

/// P2-251 reminders and blocked commands are not in the stats report.
fn event_counts(text: &str) -> (usize, usize, usize) {
    const GUARDS: [&str; 3] = [
        "The model stopped with unfinished plan steps",
        "The model reply was truncated",
        "Stopped before the fifth repeated tool call",
    ];
    let mut guards = 0;
    let mut blocked = 0;
    let mut errors = 0;
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            errors += 1;
            continue;
        };
        match &event.payload {
            EventPayload::Warning { message } => {
                if GUARDS.iter().any(|prefix| message.starts_with(prefix)) {
                    guards += 1;
                }
            }
            EventPayload::ItemCompleted { item, .. } => {
                if let Item::CommandExecution { status, .. } = item
                    && matches!(status, ItemStatus::Declined | ItemStatus::Blocked)
                {
                    blocked += 1;
                }
                if serde_json::to_string(item).is_ok_and(|text| text.contains("[Repeated call]")) {
                    guards += 1;
                }
            }
            _ => {}
        }
    }
    (guards, blocked, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_guards_and_blocked_commands() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1,"type":"warning","message":"The model reply was truncated; continuing automatically."}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":2,"type":"warning","message":"something else"}"#,
            r#"{"seq":3,"sessionId":"s","timestamp":3,"type":"item.completed","turnId":"t","item":{"type":"commandExecution","id":"i","command":"rm -rf /","cwd":"/tmp","status":"declined","aggregatedOutput":"Blocked: dangerous","exitCode":null,"durationMs":null}}"#,
            r#"{"seq":4,"sessionId":"s","timestamp":4,"type":"item.completed","turnId":"t","item":{"type":"commandExecution","id":"j","command":"ls","cwd":"/tmp","status":"completed","aggregatedOutput":"ok\n\n[Repeated call] This identical call and result have occurred 3 consecutive times.","exitCode":0,"durationMs":1}}"#,
        ]
        .join("\n");
        let (guards, blocked, errors) = event_counts(&events);
        assert_eq!(guards, 2);
        assert_eq!(blocked, 1);
        assert_eq!(errors, 0);
    }

    #[test]
    fn counts_malformed_lines() {
        let (guards, blocked, errors) = event_counts("not json\n{\"seq\":1");
        assert_eq!((guards, blocked, errors), (0, 0, 2));
    }
}
