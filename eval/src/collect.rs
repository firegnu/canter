//! Turn `canter stats --json` and the session events into one run result.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use canter_protocol::{
    Event, EventPayload, Item, ItemStatus, RequestPurpose, StatsCompaction, StatsResponse,
};
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
    #[serde(default)]
    pub reread: RereadFiles,
}

/// The files read again after a compaction, split by why; paths are relative to
/// the worktree. The three lists together are that compaction's `rereadFiles`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RereadFiles {
    /// A completed `fileChange` touched them after their last read, so rereading
    /// is expected.
    #[serde(default)]
    pub changed: Vec<String>,
    /// The compaction restored them and the model read them again anyway.
    #[serde(default)]
    pub restored: Vec<String>,
    /// Neither changed nor restored: the compaction lost content still in use.
    /// Shell edits (`sed -i`, `git checkout`) do not show up here, so only this
    /// list can over-count.
    #[serde(default)]
    pub unchanged: Vec<String>,
}

impl RereadFiles {
    pub fn count(&self) -> usize {
        self.changed.len() + self.restored.len() + self.unchanged.len()
    }

    fn push_all(&mut self, other: &Self) {
        self.changed.extend(other.changed.iter().cloned());
        self.restored.extend(other.restored.iter().cloned());
        self.unchanged.extend(other.unchanged.iter().cloned());
    }
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
    #[serde(default)]
    pub reread: RereadFiles,
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
    let scan = scan_events(&events, &stats.overview.cwd);
    let metrics = metrics(&stats, &scan);
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

fn metrics(stats: &StatsResponse, scan: &EventScan) -> Metrics {
    let usage = stats.overview.usage.as_ref();
    let duration_ms = (stats.overview.updated_at >= stats.overview.started_at).then(|| {
        u64::try_from(stats.overview.updated_at - stats.overview.started_at).unwrap_or(u64::MAX)
    });
    let reread = reread_breakdowns(&stats.compactions, scan, &stats.overview.cwd);
    let compactions: Vec<CompactionRow> = stats
        .compactions
        .iter()
        .zip(reread)
        .map(|(compaction, reread)| CompactionRow {
            trigger: serde_json::to_value(compaction.trigger)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string)),
            before_tokens: compaction.before_tokens,
            after_tokens: compaction.after_tokens,
            succeeded: compaction.succeeded,
            reread_files: compaction.signals.reread_files.as_ref().map_or(0, Vec::len),
            reread,
        })
        .collect();
    let mut reread_total = RereadFiles::default();
    for row in &compactions {
        reread_total.push_all(&row.reread);
    }
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
        reread: reread_total,
        tool_failures,
        tool_failures_by_name,
        guard_reminders: scan.guard_reminders,
        blocked_commands: scan.blocked_commands,
        event_parse_errors: scan.parse_errors,
    }
}

fn tool_calls(stats: &StatsResponse, name: &str) -> usize {
    stats
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .map_or(0, |tool| tool.call_count)
}

/// P2-251 reminders and blocked commands are not in the stats report; the reads
/// and changes are what tells apart why a file was read again after a
/// compaction.
#[derive(Debug, Default)]
struct EventScan {
    guard_reminders: usize,
    blocked_commands: usize,
    parse_errors: usize,
    /// Completed reads: event seq and normalized path, oldest first.
    reads: Vec<(u64, String)>,
    /// Completed file changes: event seq and normalized path.
    changes: Vec<(u64, String)>,
    /// The files each compaction restored, by compaction item id.
    restored: BTreeMap<String, BTreeSet<String>>,
}

fn scan_events(text: &str, cwd: &str) -> EventScan {
    const GUARDS: [&str; 3] = [
        "The model stopped with unfinished plan steps",
        "The model reply was truncated",
        "Stopped before the fifth repeated tool call",
    ];
    let mut scan = EventScan::default();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            scan.parse_errors += 1;
            continue;
        };
        match &event.payload {
            EventPayload::Warning { message } => {
                if GUARDS.iter().any(|prefix| message.starts_with(prefix)) {
                    scan.guard_reminders += 1;
                }
            }
            EventPayload::ItemCompleted { item, .. } => {
                match item {
                    Item::CommandExecution {
                        status: ItemStatus::Declined | ItemStatus::Blocked,
                        ..
                    } => scan.blocked_commands += 1,
                    Item::FileRead {
                        path,
                        status: ItemStatus::Completed,
                        ..
                    } => scan.reads.push((event.seq, normalize(cwd, path))),
                    Item::FileChange {
                        changes,
                        status: ItemStatus::Completed,
                        ..
                    } => scan.changes.extend(
                        changes
                            .iter()
                            .map(|change| (event.seq, normalize(cwd, &change.path))),
                    ),
                    Item::ContextCompaction {
                        id, restored_files, ..
                    } => {
                        scan.restored.insert(
                            id.clone(),
                            restored_files
                                .iter()
                                .map(|file| normalize(cwd, &file.path))
                                .collect(),
                        );
                    }
                    _ => {}
                }
                if serde_json::to_string(item).is_ok_and(|text| text.contains("[Repeated call]")) {
                    scan.guard_reminders += 1;
                }
            }
            _ => {}
        }
    }
    scan
}

/// Split each counted compaction's `rereadFiles` by why the file was read again.
/// One entry per compaction; a failed or unfinished one has no signals and gets
/// an empty breakdown.
fn reread_breakdowns(
    compactions: &[StatsCompaction],
    scan: &EventScan,
    cwd: &str,
) -> Vec<RereadFiles> {
    let counted: Vec<usize> = compactions
        .iter()
        .enumerate()
        .filter(|(_, row)| row.succeeded != Some(false) && row.summary.is_some())
        .map(|(index, _)| index)
        .collect();
    let mut breakdowns = vec![RereadFiles::default(); compactions.len()];
    for (position, &index) in counted.iter().enumerate() {
        let row = &compactions[index];
        let boundary = row.event_seq;
        // The same window as `canter stats`: up to the next counted compaction.
        let end = counted
            .get(position + 1)
            .map_or(u64::MAX, |next| compactions[*next].event_seq);
        let restored = scan.restored.get(&row.item_id);
        let mut files = RereadFiles::default();
        for path in row.signals.reread_files.iter().flatten() {
            let Some(last_read) = scan
                .reads
                .iter()
                .filter(|(seq, read)| read == path && *seq < boundary)
                .map(|(seq, _)| *seq)
                .max()
            else {
                files.unchanged.push(relative(cwd, path));
                continue;
            };
            let reread = scan
                .reads
                .iter()
                .filter(|(seq, read)| read == path && *seq > boundary && *seq < end)
                .map(|(seq, _)| *seq)
                .min();
            let changed = reread.is_some_and(|reread| {
                scan.changes
                    .iter()
                    .any(|(seq, changed)| changed == path && *seq > last_read && *seq < reread)
            });
            if changed {
                files.changed.push(relative(cwd, path));
            } else if restored.is_some_and(|restored| restored.contains(path)) {
                files.restored.push(relative(cwd, path));
            } else {
                files.unchanged.push(relative(cwd, path));
            }
        }
        breakdowns[index] = files;
    }
    breakdowns
}

/// Paths as `canter stats` keys them; see `crates/canter-core/src/stats/signals.rs`.
fn normalize(cwd: &str, path: &str) -> String {
    let joined = Path::new(cwd).join(path);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized.to_string_lossy().into_owned()
}

/// Back to a worktree-relative path for the result.
fn relative(cwd: &str, path: &str) -> String {
    if let Ok(rest) = Path::new(path).strip_prefix(cwd) {
        return rest.to_string_lossy().into_owned();
    }
    path.strip_prefix("./").unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_guards_and_blocked_commands() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1,"type":"warning","message":"The model reply was truncated; continuing automatically."}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":2,"type":"warning","message":"something else"}"#,
            r#"{"seq":3,"sessionId":"s","timestamp":3,"type":"item.completed","turnId":"t","item":{"type":"commandExecution","id":"i","command":"rm -rf /","cwd":"/tmp","status":"declined","aggregatedOutput":"Blocked: dangerous","exitCode":null,"durationMs":null}}"#,
            r#"{"seq":4,"sessionId":"s","timestamp":4,"type":"item.completed","turnId":"t","item":{"type":"commandExecution","id":"j","command":"ls","cwd":"/tmp","status":"completed","aggregatedOutput":"ok\n\n[Repeated call] This identical call and result have occurred 3 consecutive times.","exitCode":0,"durationMs":1}}"#,
        ]
        .join("\n");
        let scan = scan_events(&events, "/work");
        assert_eq!(scan.guard_reminders, 2);
        assert_eq!(scan.blocked_commands, 1);
        assert_eq!(scan.parse_errors, 0);
    }

    #[test]
    fn counts_malformed_lines() {
        let scan = scan_events("not json\n{\"seq\":1", "/work");
        assert_eq!(
            (
                scan.guard_reminders,
                scan.blocked_commands,
                scan.parse_errors
            ),
            (0, 0, 2)
        );
    }

    fn completed_item(seq: u64, item: serde_json::Value) -> String {
        json!({
            "seq": seq, "sessionId": "s", "timestamp": seq, "type": "item.completed",
            "turnId": "t", "item": item
        })
        .to_string()
    }

    fn read(id: &str, path: &str) -> serde_json::Value {
        json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "", "status": "completed"
        })
    }

    fn change(path: &str) -> serde_json::Value {
        json!({
            "type": "fileChange", "id": format!("change-{path}"),
            "changes": [{"path": path, "kind": {"type": "update", "movePath": null}, "diff": ""}],
            "status": "completed"
        })
    }

    fn compaction(id: &str, summary: &str, restored: &[&str]) -> serde_json::Value {
        json!({
            "type": "contextCompaction", "id": id, "summary": summary,
            "restoredFiles": restored.iter().map(|path| json!({"path": path})).collect::<Vec<_>>()
        })
    }

    #[test]
    fn reread_files_split_by_why_the_model_read_them_again() {
        let events = [
            completed_item(1, read("r1", "changed.ts")),
            completed_item(2, change("changed.ts")),
            completed_item(3, read("r2", "restored.ts")),
            completed_item(4, read("r3", "plain.ts")),
            completed_item(5, read("r4", "late.ts")),
            completed_item(6, compaction("c1", "summary", &["restored.ts"])),
            completed_item(7, read("r5", "changed.ts")),
            completed_item(8, read("r6", "restored.ts")),
            completed_item(9, read("r7", "plain.ts")),
            completed_item(10, compaction("c2", "Compaction did not finish.", &[])),
            completed_item(11, read("r8", "late.ts")),
            completed_item(12, change("late.ts")),
        ]
        .join("\n");
        let stats: StatsResponse = serde_json::from_value(json!({
            "sessionId": "s",
            "eventSeq": 12,
            "overview": {
                "cwd": "/work", "model": "m", "startedAt": 1, "updatedAt": 12,
                "tags": {}, "turnCount": 1, "requestCount": 0,
                "retryCount": null, "recordedRetryCount": 0
            },
            "requests": [],
            "compactions": [
                {"turnId": "t", "itemId": "c1", "eventSeq": 6, "timestamp": 6,
                 "summary": "summary", "trigger": "auto", "succeeded": true,
                 "signals": {
                     "rereadFiles": ["/work/changed.ts", "/work/restored.ts",
                                      "/work/plain.ts", "/work/late.ts"],
                     "plan": {"before": null, "after": null, "removed": null,
                              "statusChanged": null}
                 }},
                {"turnId": "t", "itemId": "c2", "eventSeq": 10, "timestamp": 10,
                 "summary": "Compaction did not finish.", "trigger": "auto",
                 "succeeded": false,
                 "signals": {"rereadFiles": null,
                             "plan": {"before": null, "after": null, "removed": null,
                                      "statusChanged": null}}}
            ],
            "tools": [],
            "toolCalls": [],
            "approvals": {"count": 0, "data": []}
        }))
        .unwrap();

        let scan = scan_events(&events, &stats.overview.cwd);
        let breakdowns = reread_breakdowns(&stats.compactions, &scan, &stats.overview.cwd);

        assert_eq!(breakdowns.len(), 2);
        assert_eq!(breakdowns[0].changed, ["changed.ts"]);
        assert_eq!(breakdowns[0].restored, ["restored.ts"]);
        // `late.ts` was only read again after the failed compaction, which is not
        // a boundary: it still belongs to the first compaction's window.
        assert_eq!(breakdowns[0].unchanged, ["plain.ts", "late.ts"]);
        assert_eq!(breakdowns[0].count(), 4);
        assert_eq!(breakdowns[1].count(), 0);
    }
}
