//! Turn `canter stats --json` and the session events into one run result.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use canter_protocol::{
    Event, EventPayload, Item, ItemStatus, RequestPurpose, StatsCompaction, StatsResponse,
};
use serde::{Deserialize, Serialize};

use crate::{Result, grade::CheckResult, paths::MemoryMode};

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

/// Each part's share of the estimated context the conversation requests carried,
/// in whole percents of the four parts summed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextShare {
    pub system_percent: u32,
    pub tool_definition_percent: u32,
    pub conversation_percent: u32,
    pub tool_result_percent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub duration_ms: Option<u64>,
    pub requests: usize,
    pub conversation_requests: usize,
    /// Highest real input tokens the provider reported for a conversation
    /// request; `None` for results collected before this was measured.
    #[serde(default)]
    pub context_peak_tokens: Option<u64>,
    /// Mean real input tokens of the conversation requests, rounded.
    #[serde(default)]
    pub context_average_tokens: Option<u64>,
    /// Estimated context parts, only proportions.
    #[serde(default)]
    pub context_share: Option<ContextShare>,
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

/// One session of a multi-session run; the run's metrics are the sum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub index: usize,
    pub session_id: String,
    pub duration_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_unknown: bool,
    /// Completed `fileChange` writes of `.md` files in the eval memories.
    pub memory_writes: usize,
    /// Completed `fileRead` reads of `.md` files in the eval memories.
    pub memory_reads: usize,
    pub memory_files: usize,
    pub memory_file_names: Vec<String>,
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
    /// Sessions in the task; absent in results collected before multi-session
    /// tasks, which always had one.
    #[serde(default = "one_session")]
    pub session_count: usize,
    /// `keep` or `clear`; absent in results collected before memory modes.
    #[serde(default)]
    pub memory_mode: Option<MemoryMode>,
    /// One row per session, in order; empty for first-version results.
    #[serde(default)]
    pub sessions: Vec<SessionRow>,
}

fn one_session() -> usize {
    1
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
    let session_count = state.session_count.max(1);
    let mut sessions = Vec::with_capacity(session_count);
    let mut rows = Vec::with_capacity(session_count);
    for index in 1..=session_count {
        let (stats, scan, row) = collect_session(&options, &state, index, session_count)?;
        sessions.push((stats, scan));
        rows.push(row);
    }
    let metrics = metrics(&sessions);
    let session_id = rows
        .first()
        .map(|row| row.session_id.clone())
        .unwrap_or_default();
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
        session_count,
        memory_mode: state.memory_mode,
        sessions: rows,
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

/// The session's stats and events: one result row and one input to the run's
/// summed metrics.
fn collect_session(
    options: &CollectOptions,
    state: &crate::paths::State,
    index: usize,
    session_count: usize,
) -> Result<(StatsResponse, EventScan, SessionRow)> {
    let home = &options.home;
    let run = &state.run;
    let recorded =
        crate::paths::read_session_id(&crate::paths::session_id_file(home, state, index))?;
    if session_count > 1 && recorded.is_none() {
        return Err(format!(
            "session {index} has no id yet; run `canter-eval steer --session {index}` while it is open"
        ));
    }
    let explicit = options.session.clone().filter(|_| session_count == 1);
    let stats = match &options.stats_json {
        Some(path) => read_stats(path, index, session_count)?,
        None => {
            let id = explicit.clone().or(recorded.clone()).ok_or_else(|| {
                format!(
                    "no session id yet; the run has not written {}",
                    crate::paths::session_id_file(home, state, index).display()
                )
            })?;
            let local = crate::paths::Local::load(home)?;
            let text = crate::server::run_canter(
                &crate::server::canter_binary(&local),
                &["stats", &id, "--json"],
                home,
            )?;
            serde_json::from_str::<StatsResponse>(&text)
                .map_err(|error| format!("the stats JSON is not valid: {error}"))?
        }
    };
    let session_id = stats.session_id.clone();
    // An explicit id wins for a one-session run, as it did before; a
    // multi-session run always trusts the id recorded for this session.
    if explicit.is_none()
        && let Some(recorded) = &recorded
        && *recorded != session_id
    {
        return Err(format!(
            "session {index} was recorded as {recorded}, but its stats are for {session_id}"
        ));
    }
    let events_path = home.session_events(&session_id);
    let events = fs::read_to_string(&events_path).map_err(|error| {
        format!("cannot read the events of {session_id}: {error}\nthe eval home may be wrong")
    })?;
    let scan = scan_events(&events, &stats.overview.cwd);
    let memory = memory_snapshot(home, &options.round, run, index, session_count)?;
    let activity = memory_activity(&events, &stats.overview.cwd, home);
    let usage = stats.overview.usage.as_ref();
    let row = SessionRow {
        index,
        session_id,
        duration_ms: duration(&stats),
        cost_usd: usage.map(|row| row.total.cost.usd),
        cost_unknown: usage.is_none_or(|row| row.total.cost.unknown),
        memory_writes: activity.writes,
        memory_reads: activity.reads,
        memory_files: memory.files.len(),
        memory_file_names: memory.files,
    };
    Ok((stats, scan, row))
}

/// `--stats-json` is one file for a one-session run and a directory of
/// `<session>.json` files for a multi-session one.
fn read_stats(path: &Path, index: usize, session_count: usize) -> Result<StatsResponse> {
    let path = if path.is_dir() {
        path.join(format!("{index}.json"))
    } else if session_count == 1 {
        path.to_path_buf()
    } else {
        return Err(format!(
            "--stats-json must be a directory with <session>.json files for {session_count} sessions: {}",
            path.display()
        ));
    };
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("the stats JSON in {} is not valid: {error}", path.display()))
}

fn memory_snapshot(
    home: &crate::paths::Home,
    round: &str,
    run: &str,
    index: usize,
    session_count: usize,
) -> Result<crate::finish::MemorySnapshot> {
    let path = home.session_memory_path(round, run, index);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && session_count == 1 => {
            // A first-version run recorded nothing; every session was the last.
            Ok(crate::finish::MemorySnapshot {
                files: crate::memory::files(home)?,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "session {index} is not recorded: {} is missing\nrun `canter-eval finish --session {index}` when it stops",
            path.display()
        )),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// How many times the session wrote and read memory `.md` files.
#[derive(Debug, Default, Clone, Copy)]
struct MemoryActivity {
    writes: usize,
    reads: usize,
}

fn memory_activity(text: &str, cwd: &str, home: &crate::paths::Home) -> MemoryActivity {
    let mut activity = MemoryActivity::default();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            continue;
        };
        let EventPayload::ItemCompleted { item, .. } = &event.payload else {
            continue;
        };
        match item {
            Item::FileChange {
                changes,
                status: ItemStatus::Completed,
                ..
            } => {
                for change in changes {
                    let path = normalize(cwd, &change.path);
                    if crate::memory::is_memory_md(home, Path::new(&path)) {
                        activity.writes += 1;
                    }
                }
            }
            Item::FileRead {
                path,
                status: ItemStatus::Completed,
                ..
            } => {
                let path = normalize(cwd, path);
                if crate::memory::is_memory_md(home, Path::new(&path)) {
                    activity.reads += 1;
                }
            }
            _ => {}
        }
    }
    activity
}

/// The run's metrics, summed over its sessions: tokens, cost, requests and
/// calls add up, the context peak is the highest, and the context average and
/// shares come from all conversation requests together.
fn metrics(sessions: &[(StatsResponse, EventScan)]) -> Metrics {
    let mut duration_ms: Option<u64> = Some(0);
    let mut requests = 0;
    let mut conversation_requests = 0;
    let mut context_peak: Option<u64> = None;
    let mut input_total: u128 = 0;
    let mut input_count: u128 = 0;
    // System prompt, tool definitions, conversation, tool results.
    let mut parts = [0_u128; 4];
    let mut compactions = Vec::new();
    let mut cached_input_tokens: Option<u64> = Some(0);
    let mut uncached_input_tokens: Option<u64> = Some(0);
    let mut output_tokens: Option<u64> = Some(0);
    let mut reasoning_output_tokens = 0_u64;
    let mut cost_usd: Option<f64> = Some(0.0);
    let mut cost_unknown = false;
    let mut compaction_cost_usd: Option<f64> = None;
    let mut read_calls = 0;
    let mut update_plan_calls = 0;
    let mut reread_files_after_compaction = 0;
    let mut reread = RereadFiles::default();
    let mut tool_failures = 0;
    let mut tool_failures_by_name = BTreeMap::new();
    let mut guard_reminders = 0;
    let mut blocked_commands = 0;
    let mut event_parse_errors = 0;

    for (stats, scan) in sessions {
        duration_ms = match (duration_ms, duration(stats)) {
            (Some(total), Some(one)) => Some(total.saturating_add(one)),
            _ => None,
        };
        requests += stats.overview.request_count;
        guard_reminders += scan.guard_reminders;
        blocked_commands += scan.blocked_commands;
        event_parse_errors += scan.parse_errors;

        for request in &stats.requests {
            if request.purpose == RequestPurpose::Conversation {
                conversation_requests += 1;
            }
            if request.purpose != RequestPurpose::Conversation {
                continue;
            }
            let Some(usage) = &request.usage else {
                continue;
            };
            context_peak =
                Some(context_peak.map_or(usage.input_tokens, |peak| peak.max(usage.input_tokens)));
            input_total += u128::from(usage.input_tokens);
            input_count += 1;
            if let Some(context) = &request.context {
                parts[0] += u128::from(context.system_tokens);
                parts[1] += u128::from(context.tool_definition_tokens);
                parts[2] += u128::from(context.conversation_tokens);
                parts[3] += u128::from(context.tool_result_tokens);
            }
        }
        reasoning_output_tokens = reasoning_output_tokens.saturating_add(
            stats
                .requests
                .iter()
                .filter_map(|request| request.usage.as_ref())
                .map(|usage| usage.reasoning_output_tokens)
                .fold(0u64, u64::saturating_add),
        );

        let usage = stats.overview.usage.as_ref();
        cached_input_tokens = add_tokens(
            cached_input_tokens,
            usage.map(|row| row.total.cached_input_tokens),
        );
        uncached_input_tokens = add_tokens(
            uncached_input_tokens,
            usage.map(|row| row.total.uncached_input_tokens),
        );
        output_tokens = add_tokens(output_tokens, usage.map(|row| row.total.output_tokens));
        match usage {
            Some(row) => {
                cost_unknown |= row.total.cost.unknown;
                cost_usd = cost_usd.map(|total| total + row.total.cost.usd);
            }
            None => {
                cost_usd = None;
                cost_unknown = true;
            }
        }
        let session_compaction_cost: Option<f64> = stats
            .requests
            .iter()
            .filter(|request| request.purpose == RequestPurpose::Compaction)
            .filter_map(|request| request.cost.as_ref().map(|cost| cost.usd))
            .reduce(|total, value| total + value);
        if let Some(value) = session_compaction_cost {
            compaction_cost_usd = Some(compaction_cost_usd.unwrap_or(0.0) + value);
        }

        let breakdowns = reread_breakdowns(&stats.compactions, scan, &stats.overview.cwd);
        for (compaction, compaction_reread) in stats.compactions.iter().zip(breakdowns) {
            reread_files_after_compaction +=
                compaction.signals.reread_files.as_ref().map_or(0, Vec::len);
            reread.push_all(&compaction_reread);
            compactions.push(CompactionRow {
                trigger: serde_json::to_value(compaction.trigger)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string)),
                before_tokens: compaction.before_tokens,
                after_tokens: compaction.after_tokens,
                succeeded: compaction.succeeded,
                reread_files: compaction.signals.reread_files.as_ref().map_or(0, Vec::len),
                reread: compaction_reread,
            });
        }

        read_calls += tool_calls(stats, "read");
        update_plan_calls += tool_calls(stats, "update_plan");
        for tool in &stats.tools {
            if let Some(failures) = tool.failure_count
                && failures > 0
            {
                tool_failures += failures;
                *tool_failures_by_name.entry(tool.name.clone()).or_insert(0) += failures;
            }
        }
    }

    let context_average_tokens = (input_count > 0)
        .then(|| u64::try_from((input_total + input_count / 2) / input_count).unwrap_or(u64::MAX));
    let sum: u128 = parts.iter().sum();
    let context_share = (sum > 0).then(|| ContextShare {
        system_percent: percent(parts[0], sum),
        tool_definition_percent: percent(parts[1], sum),
        conversation_percent: percent(parts[2], sum),
        tool_result_percent: percent(parts[3], sum),
    });
    Metrics {
        duration_ms,
        requests,
        conversation_requests,
        context_peak_tokens: context_peak,
        context_average_tokens,
        context_share,
        compactions,
        cached_input_tokens,
        uncached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        cost_usd,
        cost_unknown,
        compaction_cost_usd,
        read_calls,
        update_plan_calls,
        reread_files_after_compaction,
        reread,
        tool_failures,
        tool_failures_by_name,
        guard_reminders,
        blocked_commands,
        event_parse_errors,
    }
}

fn duration(stats: &StatsResponse) -> Option<u64> {
    (stats.overview.updated_at >= stats.overview.started_at).then(|| {
        u64::try_from(stats.overview.updated_at - stats.overview.started_at).unwrap_or(u64::MAX)
    })
}

/// Add one session's token total; a session without usage makes the run's total
/// unknown.
fn add_tokens(total: Option<u64>, one: Option<u64>) -> Option<u64> {
    match (total, one) {
        (Some(total), Some(one)) => Some(total.saturating_add(one)),
        _ => None,
    }
}

fn percent(part: u128, total: u128) -> u32 {
    (part as f64 / total as f64 * 100.0).round() as u32
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
    fn context_metrics_use_conversation_requests_that_reported_usage() {
        let stats: StatsResponse = serde_json::from_value(json!({
            "sessionId": "s",
            "eventSeq": 1,
            "overview": {
                "cwd": "/w", "model": "m", "startedAt": 1, "updatedAt": 2,
                "tags": {}, "turnCount": 1, "requestCount": 6,
                "retryCount": 0, "recordedRetryCount": 0
            },
            "requests": [
                {"startedAt": 1, "purpose": "conversation", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 1000, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 0, "totalTokens": 1010},
                 "context": {"estimated": true, "systemTokens": 100,
                             "toolDefinitionTokens": 50, "conversationTokens": 200,
                             "toolResultTokens": 50}},
                {"startedAt": 2, "purpose": "conversation", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 2000, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 0, "totalTokens": 2010},
                 "context": {"estimated": true, "systemTokens": 200,
                             "toolDefinitionTokens": 50, "conversationTokens": 300,
                             "toolResultTokens": 50}},
                {"startedAt": 3, "purpose": "conversation", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 1001, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 0, "totalTokens": 1011}},
                {"startedAt": 4, "purpose": "compaction", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 999999, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 0, "totalTokens": 1000009},
                 "context": {"estimated": true, "systemTokens": 999999,
                             "toolDefinitionTokens": 0, "conversationTokens": 0,
                             "toolResultTokens": 0}},
                {"startedAt": 5, "purpose": "suggestion", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 888888, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 0, "totalTokens": 888898},
                 "context": {"estimated": true, "systemTokens": 888888,
                             "toolDefinitionTokens": 0, "conversationTokens": 0,
                             "toolResultTokens": 0}},
                {"startedAt": 6, "purpose": "conversation", "outcome": "failed",
                 "toolCallIds": [],
                 "context": {"estimated": true, "systemTokens": 777777,
                             "toolDefinitionTokens": 0, "conversationTokens": 0,
                             "toolResultTokens": 0}}
            ],
            "compactions": [],
            "tools": [],
            "toolCalls": [],
            "approvals": {"count": 0, "data": []}
        }))
        .unwrap();

        let metrics = metrics(&[(stats, EventScan::default())]);
        assert_eq!(metrics.context_peak_tokens, Some(2000));
        assert_eq!(metrics.context_average_tokens, Some(1334));
        assert_eq!(
            metrics.context_share,
            Some(ContextShare {
                system_percent: 30,
                tool_definition_percent: 10,
                conversation_percent: 50,
                tool_result_percent: 10,
            })
        );
        assert_eq!(metrics.conversation_requests, 4);
    }

    #[test]
    fn sums_metrics_across_sessions() {
        let first: StatsResponse = serde_json::from_value(json!({
            "sessionId": "s-1",
            "eventSeq": 1,
            "overview": {
                "cwd": "/w", "model": "m", "startedAt": 1_000, "updatedAt": 4_000,
                "tags": {}, "turnCount": 1, "requestCount": 4,
                "usage": {"data": [], "total": {
                    "cachedInputTokens": 100, "uncachedInputTokens": 200,
                    "outputTokens": 300, "cost": {"usd": 0.25, "unknown": false}}},
                "retryCount": 0, "recordedRetryCount": 0
            },
            "requests": [
                {"startedAt": 1, "purpose": "conversation", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 1000, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 2, "totalTokens": 1010},
                 "context": {"estimated": true, "systemTokens": 10,
                             "toolDefinitionTokens": 10, "conversationTokens": 10,
                             "toolResultTokens": 10}}
            ],
            "compactions": [],
            "tools": [{"name": "read", "callCount": 2, "failureCount": 1, "truncatedCount": 0,
                       "averageResultTokens": 1.0, "maxResultTokens": 10, "unknownStatusCount": 0,
                       "unknownTruncatedCount": 0, "unknownResultSizeCount": 0}],
            "toolCalls": [],
            "approvals": {"count": 0, "data": []}
        }))
        .unwrap();
        let second: StatsResponse = serde_json::from_value(json!({
            "sessionId": "s-2",
            "eventSeq": 2,
            "overview": {
                "cwd": "/w", "model": "m", "startedAt": 10_000, "updatedAt": 11_000,
                "tags": {}, "turnCount": 1, "requestCount": 3,
                "usage": {"data": [], "total": {
                    "cachedInputTokens": 7, "uncachedInputTokens": 8,
                    "outputTokens": 9, "cost": {"usd": 0.5, "unknown": false}}},
                "retryCount": 0, "recordedRetryCount": 0
            },
            "requests": [
                {"startedAt": 1, "purpose": "conversation", "outcome": "completed",
                 "toolCallIds": [],
                 "usage": {"inputTokens": 3000, "cachedInputTokens": 0, "outputTokens": 10,
                           "reasoningOutputTokens": 5, "totalTokens": 3010},
                 "context": {"estimated": true, "systemTokens": 30,
                             "toolDefinitionTokens": 30, "conversationTokens": 30,
                             "toolResultTokens": 30}}
            ],
            "compactions": [],
            "tools": [{"name": "read", "callCount": 1, "failureCount": 2, "truncatedCount": 0,
                       "averageResultTokens": 1.0, "maxResultTokens": 10, "unknownStatusCount": 0,
                       "unknownTruncatedCount": 0, "unknownResultSizeCount": 0}],
            "toolCalls": [],
            "approvals": {"count": 0, "data": []}
        }))
        .unwrap();

        let metrics = metrics(&[
            (first, EventScan::default()),
            (second, EventScan::default()),
        ]);
        assert_eq!(metrics.duration_ms, Some(4_000));
        assert_eq!(metrics.requests, 7);
        assert_eq!(metrics.conversation_requests, 2);
        assert_eq!(metrics.context_peak_tokens, Some(3000));
        assert_eq!(metrics.context_average_tokens, Some(2000));
        assert_eq!(metrics.cached_input_tokens, Some(107));
        assert_eq!(metrics.uncached_input_tokens, Some(208));
        assert_eq!(metrics.output_tokens, Some(309));
        assert_eq!(metrics.reasoning_output_tokens, 7);
        assert_eq!(metrics.cost_usd, Some(0.75));
        assert_eq!(metrics.read_calls, 3);
        assert_eq!(metrics.tool_failures, 3);
        assert_eq!(metrics.tool_failures_by_name["read"], 3);
        assert_eq!(
            metrics.context_share,
            Some(ContextShare {
                system_percent: 25,
                tool_definition_percent: 25,
                conversation_percent: 25,
                tool_result_percent: 25,
            })
        );
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
