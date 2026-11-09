//! The round's comparison table, committed to `eval/results/`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use crate::{
    Result,
    collect::{ContextShare, RereadFiles, RunResult, SessionRow},
    paths::MemoryMode,
};

pub struct ReportOptions {
    pub home: crate::paths::Home,
    pub round: String,
    pub against: Option<String>,
    pub out: PathBuf,
}

pub fn report(options: &ReportOptions) -> Result<PathBuf> {
    let mut rounds = Vec::new();
    if let Some(against) = &options.against {
        rounds.push(against.clone());
    }
    rounds.push(options.round.clone());
    let mut rows = Vec::new();
    for round in &rounds {
        let mut results = round_results(&options.home, round)?;
        results.sort_by(|a, b| (&a.task, &a.run).cmp(&(&b.task, &b.run)));
        rows.extend(results);
    }
    let text = render(&options.round, options.against.as_deref(), &rows);
    if let Some(parent) = options.out.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(&options.out, text)
        .map_err(|error| format!("cannot write {}: {error}", options.out.display()))?;
    Ok(options.out.clone())
}

fn round_results(home: &crate::paths::Home, round: &str) -> Result<Vec<RunResult>> {
    let dir = home.round_dir(round);
    let entries = fs::read_dir(&dir).map_err(|error| {
        format!(
            "cannot read the runs of round {round} in {}: {error}",
            dir.display()
        )
    })?;
    let mut results = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path().join("result.json");
        if path.exists() {
            results.push(RunResult::load(
                home,
                round,
                &entry.file_name().to_string_lossy(),
            )?);
        } else {
            eprintln!(
                "note: {} has no result.json; it is left out of the table",
                entry.path().display()
            );
        }
    }
    Ok(results)
}

fn render(round: &str, against: Option<&str>, rows: &[RunResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# eval 结果：{round}\n\n"));
    if let Some(against) = against {
        out.push_str(&format!("上一轮：{against}（并排）\n\n"));
    }
    let models: BTreeSet<&str> = rows.iter().map(|row| row.model.as_str()).collect();
    let versions: BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.canter_version.as_deref())
        .collect();
    if !models.is_empty() {
        out.push_str(&format!(
            "模型：{}　canter：{}\n\n",
            models.into_iter().collect::<Vec<_>>().join("、"),
            if versions.is_empty() {
                "未知".to_string()
            } else {
                versions.into_iter().collect::<Vec<_>>().join("、")
            }
        ));
    }
    let times: Vec<i64> = rows.iter().map(|row| row.completed_at).collect();
    if let (Some(first), Some(last)) = (times.iter().min(), times.iter().max()) {
        out.push_str(&format!(
            "时段：{} ～ {}\n\n",
            timestamp(*first),
            timestamp(*last)
        ));
    }

    let show_round = against.is_some();
    // Multi-session tasks compare runs with memories kept or emptied, so they
    // get one more column and a section per session.
    let show_memory = rows.iter().any(|row| row.session_count > 1);
    out.push_str("| ");
    if show_round {
        out.push_str("轮次 | ");
    }
    out.push_str("题 | 次 | ");
    if show_memory {
        out.push_str("记忆 | ");
    }
    out.push_str(
        "判分 | 未过 | 用时 | 请求 | 压缩 | 上下文峰值 | 平均上下文 | 上下文组成（估算） | 缓存输入 | 未缓存输入 | 输出 | 思考 | 花费 | 读 | 压后重读 | 工具失败 | 改计划 | 护栏 | 被拦 | 记录 | 会话 |\n",
    );
    out.push('|');
    let columns = if show_round { 24 } else { 23 } + usize::from(show_memory);
    for _ in 0..columns {
        out.push_str("---|");
    }
    out.push('\n');
    for row in rows {
        let metrics = &row.metrics;
        let failed: Vec<&str> = row
            .checks
            .iter()
            .filter(|check| check.passed == Some(false))
            .map(|check| check.id.as_str())
            .collect();
        let records: Vec<String> = row
            .checks
            .iter()
            .filter(|check| check.record)
            .map(|check| match check.value {
                Some(value) => format!("{}={value}", check.id),
                None => format!("{}=?", check.id),
            })
            .collect();
        out.push_str("| ");
        if show_round {
            out.push_str(&format!("{} | ", row.round));
        }
        let run_cell = if show_memory {
            format!("{} | {}", row.run, memory_label(row.memory_mode))
        } else {
            row.run.clone()
        };
        out.push_str(&format!(
            "{} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            row.task,
            run_cell,
            row.checks_passed,
            row.checks_total,
            if failed.is_empty() {
                "—".to_string()
            } else {
                failed.join(", ")
            },
            duration(metrics.duration_ms),
            metrics.requests,
            metrics.compactions.len(),
            context_tokens(metrics.context_peak_tokens),
            context_tokens(metrics.context_average_tokens),
            context_share(metrics.context_share.as_ref()),
            tokens(metrics.cached_input_tokens),
            tokens(metrics.uncached_input_tokens),
            tokens(metrics.output_tokens),
            tokens(Some(metrics.reasoning_output_tokens)),
            cost(metrics.cost_usd, metrics.cost_unknown, metrics.compaction_cost_usd),
            metrics.read_calls,
            reread_cell(&metrics.reread, metrics.reread_files_after_compaction),
            tool_failures(metrics.tool_failures, &metrics.tool_failures_by_name),
            metrics.update_plan_calls,
            metrics.guard_reminders,
            metrics.blocked_commands,
            if records.is_empty() {
                "—".to_string()
            } else {
                records.join(" · ")
            },
            sessions_cell(row),
        ));
    }
    out.push('\n');

    if !rows.is_empty() {
        let show_modes = show_memory && rows.iter().any(|row| row.memory_mode.is_some());
        out.push_str("## 通过次数\n\n| 轮次 | 题 | ");
        if show_modes {
            out.push_str("记忆 | ");
        }
        out.push_str("全部通过 | 平均判分 |\n| --- | --- | ");
        if show_modes {
            out.push_str("--- | ");
        }
        out.push_str("--- | --- |\n");
        let mut groups: std::collections::BTreeMap<(&str, &str, &str), Vec<&RunResult>> =
            std::collections::BTreeMap::new();
        for row in rows {
            // Only multi-session runs compare memory modes; a one-session run
            // keeps the original (round, task) grouping even when it records one.
            let mode = if row.session_count > 1 {
                memory_label(row.memory_mode)
            } else {
                ""
            };
            groups
                .entry((row.round.as_str(), row.task.as_str(), mode))
                .or_default()
                .push(row);
        }
        for ((round, task, mode), group) in groups {
            let complete = group
                .iter()
                .filter(|row| row.checks_passed == row.checks_total)
                .count();
            let average = group
                .iter()
                .map(|row| row.checks_passed as f64)
                .sum::<f64>()
                / group.len() as f64;
            let total = group.first().map_or(0, |row| row.checks_total);
            out.push_str(&format!("| {round} | {task} | "));
            if show_modes {
                let label = if mode.is_empty() { "—" } else { mode };
                out.push_str(&format!("{label} | "));
            }
            out.push_str(&format!(
                "{complete}/{} | {average:.1}/{total} |\n",
                group.len()
            ));
        }
        out.push('\n');
    }

    if show_memory {
        out.push_str("## 会话\n\n");
        out.push_str("| ");
        if show_round {
            out.push_str("轮次 | ");
        }
        out.push_str("次 | 会话 | 用时 | 花费 | 写记忆 | 读记忆 | 记忆文件数 |\n| ");
        if show_round {
            out.push_str("--- | ");
        }
        out.push_str("--- | --- | --- | --- | --- | --- | --- |\n");
        for row in rows.iter().filter(|row| row.session_count > 1) {
            for session in &row.sessions {
                out.push_str("| ");
                if show_round {
                    out.push_str(&format!("{} | ", row.round));
                }
                out.push_str(&format!(
                    "{} | {} | {} | {} | {} | {} | {} |\n",
                    row.run,
                    session_cell(session),
                    duration(session.duration_ms),
                    cost(session.cost_usd, session.cost_unknown, None),
                    session.memory_writes,
                    session.memory_reads,
                    session.memory_files,
                ));
            }
        }
        out.push('\n');
    }

    let mut details = String::new();
    for row in rows {
        if row.metrics.compactions.is_empty() {
            continue;
        }
        details.push_str(&format!(
            "- {} / {}：{} 次\n",
            row.round,
            row.run,
            row.metrics.compactions.len()
        ));
        for compaction in &row.metrics.compactions {
            let reread = if compaction.reread.count() > 0 {
                format!(
                    " · 压后重读 {}（{}）",
                    compaction.reread.count(),
                    reread_parts(&compaction.reread)
                )
            } else {
                String::new()
            };
            details.push_str(&format!(
                "  - {} {} → {}（{}）{}\n",
                compaction.trigger.as_deref().unwrap_or("unknown"),
                tokens(compaction.before_tokens),
                tokens(compaction.after_tokens),
                match compaction.succeeded {
                    Some(true) => "成功",
                    Some(false) => "失败",
                    None => "结果未知",
                },
                reread
            ));
        }
    }
    if !details.is_empty() {
        out.push_str("## 压缩\n\n");
        out.push_str(&details);
        out.push('\n');
    }
    out
}

fn memory_label(mode: Option<MemoryMode>) -> &'static str {
    match mode {
        Some(MemoryMode::Keep) => "保留",
        Some(MemoryMode::Clear) => "清空",
        None => "—",
    }
}

fn sessions_cell(row: &RunResult) -> String {
    if row.session_count <= 1 || row.sessions.is_empty() {
        return row.session_id.clone();
    }
    row.sessions
        .iter()
        .map(session_cell)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn session_cell(session: &SessionRow) -> String {
    format!("{} {}", session.index, session.session_id)
}

fn timestamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

fn duration(ms: Option<u64>) -> String {
    let Some(ms) = ms else {
        return "?".into();
    };
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn tokens(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "?".into();
    };
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.0}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// A missing context column used to be absent from `result.json`; show that as
/// a dash, not as an unknown token count.
fn context_tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| tokens(Some(value)))
}

fn context_share(share: Option<&ContextShare>) -> String {
    let Some(share) = share else {
        return "—".into();
    };
    format!(
        "系统 {}% · 工具定义 {}% · 对话 {}% · 工具结果 {}%",
        share.system_percent,
        share.tool_definition_percent,
        share.conversation_percent,
        share.tool_result_percent
    )
}

fn cost(value: Option<f64>, unknown: bool, compaction: Option<f64>) -> String {
    let Some(value) = value else {
        return "?".into();
    };
    let base = if unknown {
        "?".to_string()
    } else {
        money(value)
    };
    match compaction {
        Some(compaction) if compaction > 0.0 => format!("{base} (压缩 {})", money(compaction)),
        _ => base,
    }
}

fn reread_cell(files: &RereadFiles, total: usize) -> String {
    if files.count() == 0 {
        total.to_string()
    } else {
        format!("{total}（{}）", reread_parts(files))
    }
}

fn reread_parts(files: &RereadFiles) -> String {
    format!(
        "改过 {} · 读回 {} · 没改 {}",
        files.changed.len(),
        files.restored.len(),
        files.unchanged.len()
    )
}

fn tool_failures(total: usize, by_name: &BTreeMap<String, usize>) -> String {
    if by_name.is_empty() {
        return total.to_string();
    }
    let mut rows: Vec<(&str, usize)> = by_name
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let parts: Vec<String> = rows
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    format!("{total}（{}）", parts.join(" · "))
}

fn money(value: f64) -> String {
    if value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_tokens_are_compact() {
        assert_eq!(duration(Some(3_723_000)), "1h02m");
        assert_eq!(duration(Some(754_000)), "12m34s");
        assert_eq!(duration(Some(3_000)), "3s");
        assert_eq!(duration(None), "?");
        assert_eq!(tokens(Some(1_234_567)), "1.2M");
        assert_eq!(tokens(Some(12_300)), "12k");
        assert_eq!(tokens(None), "?");
    }
}
