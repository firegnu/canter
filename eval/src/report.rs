//! The round's comparison table, committed to `eval/results/`.

use std::{collections::BTreeSet, fs, path::PathBuf};

use crate::{Result, collect::RunResult};

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
    out.push_str("| ");
    if show_round {
        out.push_str("轮次 | ");
    }
    out.push_str(
        "题 | 次 | 判分 | 未过 | 用时 | 请求 | 压缩 | 缓存输入 | 未缓存输入 | 输出 | 思考 | 花费 | 读 | 压后重读 | 工具失败 | 护栏 | 被拦 | 记录 | 会话 |\n",
    );
    out.push('|');
    for _ in 0..if show_round { 20 } else { 19 } {
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
        out.push_str(&format!(
            "{} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            row.task,
            row.run,
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
            tokens(metrics.cached_input_tokens),
            tokens(metrics.uncached_input_tokens),
            tokens(metrics.output_tokens),
            tokens(Some(metrics.reasoning_output_tokens)),
            cost(metrics.cost_usd, metrics.cost_unknown, metrics.compaction_cost_usd),
            metrics.read_calls,
            metrics.reread_files_after_compaction,
            metrics.tool_failures,
            metrics.guard_reminders,
            metrics.blocked_commands,
            if records.is_empty() {
                "—".to_string()
            } else {
                records.join(" · ")
            },
            row.session_id,
        ));
    }
    out.push('\n');

    if !rows.is_empty() {
        out.push_str(
            "## 通过次数\n\n| 轮次 | 题 | 全部通过 | 平均判分 |\n| --- | --- | --- | --- |\n",
        );
        let mut groups: std::collections::BTreeMap<(&str, &str), Vec<&RunResult>> =
            std::collections::BTreeMap::new();
        for row in rows {
            groups
                .entry((row.round.as_str(), row.task.as_str()))
                .or_default()
                .push(row);
        }
        for ((round, task), group) in groups {
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
            out.push_str(&format!(
                "| {round} | {task} | {complete}/{} | {average:.1}/{total} |\n",
                group.len()
            ));
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
            details.push_str(&format!(
                "  - {} {} → {}（{}）\n",
                compaction.trigger.as_deref().unwrap_or("unknown"),
                tokens(compaction.before_tokens),
                tokens(compaction.after_tokens),
                match compaction.succeeded {
                    Some(true) => "成功",
                    Some(false) => "失败",
                    None => "结果未知",
                }
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
