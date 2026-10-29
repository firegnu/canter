use crate::session::Shared;
use canter_protocol::*;
use canter_tools::lsp::{DiagnosticBatch, Freshness};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const REMINDER: &str = "These are partial, asynchronous diagnostics. Run the checks required by the project; these diagnostics do not replace them.";
const INLINE: Duration = Duration::from_millis(500);
const MAX_BYTES: usize = 8 * 1024;
const MAX_LINE: usize = 1024;
type Source = (PathBuf, String);
#[derive(Default)]
pub(crate) struct Delivery {
    seen: BTreeMap<Source, BTreeSet<u64>>,
    pending: BTreeMap<PathBuf, Pending>,
    batches: BTreeMap<Source, u64>,
    pub notices: BTreeSet<usize>,
    expired: Vec<(String, String, PathBuf)>,
}
struct Pending {
    ticket: canter_tools::lsp::ChangeTicket,
    turn_id: String,
    item_id: String,
    root: Option<PathBuf>,
    reported: bool,
}
fn key(batch: &DiagnosticBatch) -> Source {
    (batch.path.clone(), batch.server_id.clone())
}
fn identity(d: &Value) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    serde_json::json!([d["severity"], d["code"], d["range"], d["message"]])
        .to_string()
        .hash(&mut hash);
    hash.finish()
}
fn path(path: &Path, cwd: &str) -> String {
    let root = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.into());
    path.strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string()
}
fn truncated(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit.saturating_sub("[truncated]".len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[truncated]", &text[..end])
}
fn count(errors: usize, warnings: usize, omitted: usize) -> String {
    let mut line = format!(
        "{errors} {}, {warnings} {}.",
        if errors == 1 { "error" } else { "errors" },
        if warnings == 1 { "warning" } else { "warnings" }
    );
    if omitted > 0 {
        line.push_str(&format!("; {omitted} not shown"));
    }
    line
}
struct Group {
    batch: DiagnosticBatch,
    sources: Vec<DiagnosticBatch>,
    diagnostics: Vec<Value>,
    selected: Vec<Value>,
    shown: Vec<Value>,
    first: bool,
    resolved: Option<usize>,
}
struct Report {
    text: String,
    files: Vec<FileReport>,
}
struct FileReport {
    batch: DiagnosticBatch,
    sources: Vec<DiagnosticBatch>,
    shown: Vec<Value>,
    total: u32,
    omitted: u32,
    resolved: Option<u32>,
}
impl Delivery {
    fn expire(&mut self) {
        self.pending.retain(|path, p| {
            if p.ticket.changed_at.elapsed() <= Duration::from_secs(60) {
                return true;
            }
            if !p.reported && self.expired.len() < 1024 {
                self.expired
                    .push((p.turn_id.clone(), p.item_id.clone(), path.clone()));
            }
            false
        });
    }
    fn render(&mut self, batches: Vec<DiagnosticBatch>, cwd: &str, late: bool) -> Report {
        self.render_with_budget(batches, cwd, late, MAX_BYTES)
    }
    fn render_with_budget(
        &mut self,
        batches: Vec<DiagnosticBatch>,
        cwd: &str,
        late: bool,
        budget: usize,
    ) -> Report {
        let mut groups = Vec::new();
        let mut remaining = 20;
        let mut by_file: Vec<Vec<DiagnosticBatch>> = Vec::new();
        for batch in batches {
            if let Some(sources) = by_file
                .iter_mut()
                .find(|sources| key(&sources[0]) == key(&batch))
            {
                sources.push(batch);
            } else {
                by_file.push(vec![batch]);
            }
        }
        for sources in by_file {
            let mut batch = sources[0].clone();
            batch.id = sources.iter().map(|b| b.id).max().unwrap();
            batch.diagnostics = sources.iter().flat_map(|b| b.diagnostics.clone()).collect();
            batch.incomplete = sources.iter().any(|b| b.incomplete);
            if let Some(uncertain) = sources
                .iter()
                .find(|b| b.freshness == Freshness::Stale)
                .or_else(|| {
                    sources
                        .iter()
                        .find(|b| b.freshness == Freshness::VersionUnconfirmed)
                })
            {
                batch.freshness = uncertain.freshness;
                batch.reason = uncertain.reason.clone();
            }
            if batch.incomplete && batch.reason.is_none() {
                batch.reason = sources.iter().find_map(|b| b.reason.clone());
            }
            let previous = self.seen.get(&key(&batch));
            let mut unique = BTreeSet::new();
            let mut diagnostics: Vec<_> = batch
                .diagnostics
                .iter()
                .filter(|d| matches!(d["severity"].as_u64(), Some(1 | 2)))
                .filter(|d| unique.insert(identity(d)))
                .cloned()
                .collect();
            diagnostics.sort_by_key(|d| d["severity"].as_u64());
            if matches!(batch.freshness, Freshness::Stale | Freshness::Deleted) {
                diagnostics.clear();
            }
            let selected: Vec<_> = diagnostics
                .iter()
                .filter(|d| !previous.is_some_and(|set| set.contains(&identity(d))))
                .cloned()
                .collect();
            let current: BTreeSet<_> = diagnostics.iter().map(identity).collect();
            let resolved = previous
                .filter(|_| batch.freshness == Freshness::Current && !batch.incomplete)
                .map(|set| set.difference(&current).count());
            let shown: Vec<_> = selected.iter().take(remaining).cloned().collect();
            remaining -= shown.len();
            groups.push(Group {
                batch,
                sources,
                diagnostics,
                selected,
                shown,
                first: previous.is_none(),
                resolved,
            });
        }
        let total_errors = groups
            .iter()
            .flat_map(|g| &g.diagnostics)
            .filter(|d| d["severity"] == 1)
            .count();
        let total_warnings =
            groups.iter().map(|g| g.diagnostics.len()).sum::<usize>() - total_errors;
        let totals = (total_errors, total_warnings, groups.len());
        let mut omitted_groups = 0;
        let mut dropped = Vec::new();
        let text = loop {
            let text = render_groups(&groups, cwd, late, omitted_groups, totals);
            if text.len() <= budget {
                break text;
            }
            if let Some(group) = groups.iter_mut().rev().find(|g| !g.shown.is_empty()) {
                group.shown.pop();
            } else if let Some(group) = groups.pop() {
                omitted_groups += group.selected.len();
                dropped.push(group);
            } else {
                break format!("[LSP diagnostics]\n{REMINDER}");
            }
        };
        let mut files = Vec::new();
        for group in groups {
            self.batches.insert(key(&group.batch), group.batch.id);
            if !matches!(group.batch.freshness, Freshness::Stale | Freshness::Deleted) {
                let seen = self.seen.entry(key(&group.batch)).or_default();
                if group.batch.freshness == Freshness::Current && !group.batch.incomplete {
                    let current: BTreeSet<_> = group.diagnostics.iter().map(identity).collect();
                    seen.retain(|d| current.contains(d));
                }
                seen.extend(group.shown.iter().map(identity));
            }
            files.push(FileReport {
                batch: group.batch,
                sources: group.sources,
                total: group.diagnostics.len() as u32,
                omitted: (group.selected.len() - group.shown.len()) as u32,
                resolved: group.resolved.map(|n| n as u32),
                shown: group.shown,
            });
        }
        for group in dropped {
            files.push(FileReport {
                batch: group.batch,
                sources: group.sources,
                total: group.diagnostics.len() as u32,
                omitted: group.selected.len() as u32,
                resolved: None,
                shown: vec![],
            });
        }
        // A forgotten baseline is explicitly reported as a first report next time.
        while self
            .seen
            .iter()
            .map(|((path, server), values)| {
                path.as_os_str().len() + server.len() + values.len() * 32 + 256
            })
            .sum::<usize>()
            > 1024 * 1024
        {
            if let Some((source, _)) = self.seen.pop_first() {
                self.batches.remove(&source);
            } else {
                break;
            }
        }
        Report { text, files }
    }
}
fn render_groups(
    groups: &[Group],
    cwd: &str,
    late: bool,
    omitted_groups: usize,
    totals: (usize, usize, usize),
) -> String {
    let mut lines = vec![if late {
        "[LSP diagnostics update] (arrived after the file change)".into()
    } else {
        "[LSP diagnostics]".into()
    }];
    let mut omitted = omitted_groups;
    for g in groups {
        let p = path(&g.batch.path, cwd);
        lines.push(format!(
            "{p} ({}). {}",
            g.batch.server_id,
            if g.first {
                "First report for this file: pre-existing problems may be included."
            } else {
                "Only new or changed diagnostics are listed."
            }
        ));
        match g.batch.freshness {
            Freshness::VersionUnconfirmed=>lines.push("Freshness: version unconfirmed. These results may be out of date; they cannot confirm that earlier problems are resolved.".into()),
            Freshness::Stale=>lines.push(format!("Diagnostics may be stale: {}. No current result is being delivered for this file.",g.batch.reason.as_deref().unwrap_or("Document coverage is unknown"))),
            Freshness::Deleted=>lines.push("The file was deleted. Its previous diagnostics are no longer applicable; this is not a project check result.".into()),
            Freshness::Current=>{},
        }
        if g.batch.incomplete {
            lines.push(format!(
                "Diagnostic coverage is incomplete: {}. Counts include only the available results.",
                g.batch
                    .reason
                    .as_deref()
                    .unwrap_or("Some diagnostics are unavailable")
            ));
        }
        for d in &g.shown {
            let code = match &d["code"] {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            lines.push(truncated(
                &format!(
                    "{p}:{}:{}: {}{} {}",
                    d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1,
                    d["range"]["start"]["character"].as_u64().unwrap_or(0) + 1,
                    if d["severity"] == 1 {
                        "error"
                    } else {
                        "warning"
                    },
                    code.map_or(String::new(), |c| format!(" [{c}]")),
                    d["message"].as_str().unwrap_or("")
                ),
                MAX_LINE,
            ));
        }
        let e = g.diagnostics.iter().filter(|d| d["severity"] == 1).count();
        let w = g.diagnostics.len() - e;
        let o = g.selected.len() - g.shown.len();
        lines.push(count(e, w, o));
        if let Some(n) = g.resolved {
            lines.push(format!("Resolved since last report: {n}."));
        }
        if g.diagnostics.is_empty() && !g.batch.incomplete {
            match g.batch.freshness {
                Freshness::Current=>lines.push("This source reported no errors or warnings for this document version. This is not a project check result.".into()),
                Freshness::VersionUnconfirmed=>lines.push("This source returned an empty diagnostic list without a confirmed document version. Earlier problems are not confirmed resolved.".into()),
                _=>{},
            }
        }
        omitted += o;
    }
    if totals.2 > 1 || omitted_groups > 0 {
        lines.push(count(totals.0, totals.1, omitted));
    }
    lines.push(REMINDER.into());
    lines.join("\n")
}
pub(crate) async fn inline(
    shared: &Shared,
    turn_id: &str,
    output: &mut canter_tools::ToolOutput,
) -> Vec<Item> {
    let Some(ticket) = output.change_ticket.take() else {
        return Vec::new();
    };
    let mut pending = Pending {
        ticket: (*ticket).clone(),
        turn_id: turn_id.into(),
        item_id: output.item.id().unwrap_or_default().into(),
        root: None,
        reported: false,
    };
    let mut items = Vec::new();
    let text = match ticket
        .wait(INLINE.saturating_sub(ticket.changed_at.elapsed()))
        .await
    {
        Ok(batch) => {
            pending.root = Some(batch.root.clone());
            pending.reported = true;
            let mut batches = tokio::time::timeout_at(
                ticket.changed_at + INLINE,
                shared.tools.lsp().unwrap().diagnostics(),
            )
            .await
            .unwrap_or_default();
            batches.retain(|b| b.root == batch.root && b.id != batch.id);
            batches.sort_by_key(|b| b.path.clone());
            batches.insert(0, batch);
            let mut state = shared.state.lock().await;
            let cwd = state.journal.snapshot.settings.cwd.clone();
            let report = state.lsp_delivery.render(batches, &cwd, false);
            items = report_items(
                &report,
                shared.tools.lsp().unwrap(),
                &cwd,
                Some(&pending.item_id),
            );
            report.text
        }
        Err(error) => {
            let status = if error.kind == canter_tools::lsp::ErrorKind::Timeout {
                "The file change succeeded. Diagnostics are still pending after 500 ms.".into()
            } else {
                format!(
                    "The file change succeeded. Diagnostics are unavailable: {}.",
                    error.message
                )
            };
            items.push(status_item(
                &pending.item_id,
                if error.kind == canter_tools::lsp::ErrorKind::Timeout {
                    DiagnosticStatus::Pending
                } else {
                    DiagnosticStatus::Unavailable
                },
                &ticket.path,
                &error.message,
            ));
            format!("[LSP diagnostics]\n{status}\n{REMINDER}")
        }
    };
    if ticket.failure().is_none() {
        shared
            .state
            .lock()
            .await
            .lsp_delivery
            .pending
            .insert(ticket.path.clone(), pending);
    }
    output.text.push_str("\n\n");
    output.text.push_str(&text);
    items
}

pub(crate) async fn before_request(shared: &Shared, turn_id: &str) -> Result<(), crate::CoreError> {
    let Some(lsp) = shared.tools.lsp() else {
        return Ok(());
    };
    let cached = lsp.diagnostics().await;
    let (mut pending, expired) = {
        let mut state = shared.state.lock().await;
        state.lsp_delivery.expire();
        (
            std::mem::take(&mut state.lsp_delivery.pending),
            std::mem::take(&mut state.lsp_delivery.expired),
        )
    };
    let mut eligible = Vec::new();
    let mut statuses:Vec<_>=expired.into_iter().map(|(_,item,path)|(item,path,DiagnosticStatus::TimedOut,"The 60-second diagnostic delivery window has expired. No current result is being delivered for this change.".to_string())).collect();
    let mut discarded = BTreeSet::new();
    for (path, p) in &mut pending {
        if !p.ticket.is_current().await {
            discarded.insert(path.clone());
            continue;
        }
        if let Some(error) = p.ticket.failure() {
            discarded.insert(path.clone());
            if !p.reported && error.kind != canter_tools::lsp::ErrorKind::Cancelled {
                statuses.push((
                    p.item_id.clone(),
                    path.clone(),
                    DiagnosticStatus::Unavailable,
                    format!(
                        "Diagnostics are unavailable for {}: {}.",
                        path.display(),
                        error.message
                    ),
                ));
            }
            continue;
        }
        if p.ticket.changed_at.elapsed() > Duration::from_secs(60) {
            discarded.insert(path.clone());
            if !p.reported {
                statuses.push((p.item_id.clone(),path.clone(),DiagnosticStatus::TimedOut,"The 60-second diagnostic delivery window has expired. No current result is being delivered for this change.".into()));
            }
            continue;
        }
        let root = p.root.clone().or_else(|| {
            cached
                .iter()
                .find(|b| b.path == *path)
                .map(|b| b.root.clone())
        });
        p.root = root.clone();
        for batch in &cached {
            if batch.received_at >= p.ticket.changed_at
                && (batch.path == *path || root.as_ref() == Some(&batch.root))
                && !eligible.iter().any(|b: &DiagnosticBatch| b.id == batch.id)
            {
                eligible.push(batch.clone());
            }
        }
    }
    let mut retained = BTreeMap::new();
    for (path, p) in pending {
        if !discarded.contains(&path)
            && p.ticket.changed_at.elapsed() <= Duration::from_secs(60)
            && p.ticket.is_current().await
        {
            retained.insert(path, p);
        }
    }
    let mut state = shared.state.lock().await;
    let changed: BTreeSet<_> = eligible
        .iter()
        .filter(|b| {
            state
                .lsp_delivery
                .batches
                .get(&key(b))
                .is_none_or(|last| b.id > *last)
        })
        .map(key)
        .collect();
    eligible.retain(|b| changed.contains(&key(b)));
    eligible.sort_by_key(|b| {
        (
            !retained.contains_key(&b.path),
            b.path.clone(),
            b.source.clone(),
        )
    });
    for p in retained.values_mut() {
        if eligible.iter().any(|b| b.path == p.ticket.path) {
            p.reported = true;
        }
    }
    state.lsp_delivery.pending.extend(retained);
    if eligible.is_empty() && statuses.is_empty() {
        return Ok(());
    }
    let cwd = state.journal.snapshot.settings.cwd.clone();
    // Status lines and diagnostics share the same 8 KiB envelope.
    let mut status_text = String::new();
    let mut status_items = Vec::new();
    for (item, path, status, text) in statuses {
        if status_text.len() + text.len() + 1 > 2048 {
            break;
        }
        status_text.push_str(&text);
        status_text.push('\n');
        status_items.push(status_item(&item, status, &path, &text));
    }
    let text = if eligible.is_empty() {
        format!("[LSP diagnostics update] (arrived after the file change)\n{status_text}{REMINDER}")
    } else {
        let report = state.lsp_delivery.render_with_budget(
            eligible,
            &cwd,
            true,
            MAX_BYTES - status_text.len(),
        );
        let mut items = report_items(&report, lsp, &cwd, None);
        for (item, file) in items.iter_mut().zip(&report.files) {
            if let Item::Diagnostics { source_item_id, .. } = item {
                *source_item_id = state
                    .lsp_delivery
                    .pending
                    .get(&file.batch.path)
                    .or_else(|| {
                        state
                            .lsp_delivery
                            .pending
                            .values()
                            .find(|p| p.root.as_ref() == Some(&file.batch.root))
                    })
                    .map(|p| p.item_id.clone());
            }
        }
        state.journal.append(item_events(turn_id, items)).await?;
        if status_text.is_empty() {
            report.text
        } else {
            report
                .text
                .replacen(REMINDER, &format!("{status_text}{REMINDER}"), 1)
        }
    };
    if !status_items.is_empty() {
        state
            .journal
            .append(item_events(turn_id, status_items))
            .await?;
    }
    let message = canter_model::Message::User { text };
    state
        .journal
        .record(crate::context::HistoryEntry::DiagnosticsNotice {
            message: message.clone(),
        })
        .await?;
    let index = state.history.len();
    state.lsp_delivery.notices.insert(index);
    state.history.push(message);
    Ok(())
}

pub(crate) fn item_events(turn_id: &str, items: Vec<Item>) -> Vec<EventPayload> {
    items
        .into_iter()
        .flat_map(|item| {
            [
                EventPayload::ItemStarted {
                    turn_id: turn_id.into(),
                    item: item.clone(),
                },
                EventPayload::ItemCompleted {
                    turn_id: turn_id.into(),
                    item,
                },
            ]
        })
        .collect()
}
fn diagnostic(value: &Value) -> Diagnostic {
    let position = |v: &Value| DiagnosticPosition {
        line: v["line"].as_u64().unwrap_or(0) as u32,
        character: v["character"].as_u64().unwrap_or(0) as u32,
    };
    Diagnostic {
        severity: if value["severity"] == 1 {
            DiagnosticSeverity::Error
        } else {
            DiagnosticSeverity::Warning
        },
        range: DiagnosticRange {
            start: position(&value["range"]["start"]),
            end: position(&value["range"]["end"]),
        },
        source: value["source"].as_str().map(str::to_owned),
        code: match &value["code"] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        },
        message: truncated(value["message"].as_str().unwrap_or(""), MAX_LINE),
    }
}
fn public_file(
    batch: &DiagnosticBatch,
    shown: &[Value],
    lsp: &canter_tools::lsp::LspSession,
    cwd: &str,
) -> DiagnosticFile {
    DiagnosticFile {
        batch_id: batch.id,
        path: path(&batch.path, cwd),
        server_id: lsp.server_id(&batch.server_id, &batch.root),
        source: batch.source.clone(),
        collected_at: Some(batch.collected_at),
        document_version: batch.version,
        freshness: match batch.freshness {
            Freshness::Current => DiagnosticFreshness::Current,
            Freshness::VersionUnconfirmed => DiagnosticFreshness::VersionUnconfirmed,
            Freshness::Deleted => DiagnosticFreshness::Deleted,
            Freshness::Stale => DiagnosticFreshness::Stale,
        },
        incomplete: batch.incomplete,
        reason: batch.reason.clone(),
        diagnostics: shown.iter().map(diagnostic).collect(),
    }
}
fn report_items(
    report: &Report,
    lsp: &canter_tools::lsp::LspSession,
    cwd: &str,
    source: Option<&str>,
) -> Vec<Item> {
    report
        .files
        .iter()
        .map(|f| {
            let mut assigned = BTreeSet::new();
            let files = f
                .sources
                .iter()
                .map(|batch| {
                    let shown = f
                        .shown
                        .iter()
                        .filter(|d| {
                            batch
                                .diagnostics
                                .iter()
                                .any(|raw| identity(raw) == identity(d))
                                && assigned.insert(identity(d))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    public_file(batch, &shown, lsp, cwd)
                })
                .collect();
            Item::Diagnostics {
                id: crate::session::id("diagnostics"),
                source_item_id: source.map(str::to_owned),
                status: DiagnosticStatus::Available,
                files,
                total_count: f.total,
                shown_count: f.shown.len() as u32,
                omitted_count: f.omitted,
                resolved_count: f.resolved,
            }
        })
        .collect()
}
fn status_item(source: &str, status: DiagnosticStatus, path: &Path, reason: &str) -> Item {
    Item::Diagnostics {
        id: crate::session::id("diagnostics"),
        source_item_id: Some(source.into()),
        status,
        files: vec![DiagnosticFile {
            batch_id: 0,
            path: path.display().to_string(),
            server_id: String::new(),
            source: "status".into(),
            collected_at: None,
            document_version: None,
            freshness: DiagnosticFreshness::Stale,
            incomplete: true,
            reason: Some(reason.into()),
            diagnostics: vec![],
        }],
        total_count: 0,
        shown_count: 0,
        omitted_count: 0,
        resolved_count: None,
    }
}

impl Delivery {
    pub fn rewind(&mut self, turns: &[canter_protocol::Turn]) {
        self.expired
            .retain(|(turn, _, _)| turns.iter().any(|t| t.id == *turn));
        self.pending.retain(|_, pending| {
            turns.iter().any(|t| {
                t.id == pending.turn_id && t.items.iter().any(|i| i.id() == Some(&pending.item_id))
            })
        });
    }
}

pub(crate) fn watch(shared: &std::sync::Arc<Shared>) {
    if shared.tools.lsp().is_none() {
        return;
    }
    let weak = std::sync::Arc::downgrade(shared);
    let stop = shared.lifetime.clone();
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_millis(200));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_ = stop.cancelled()=>break,_ = ticks.tick()=>{}}
            let Some(shared) = weak.upgrade() else {
                break;
            };
            if shared.state.lock().await.closing {
                break;
            }
            shared.state.lock().await.lsp_delivery.expire();
            if sync_states(&shared).await.is_err() {
                break;
            }
        }
    });
}
async fn sync_states(shared: &Shared) -> Result<Vec<LanguageServer>, crate::CoreError> {
    let live = shared
        .tools
        .lsp()
        .map(|lsp| lsp.language_servers())
        .unwrap_or_default();
    let mut state = shared.state.lock().await;
    if state.closing {
        return Ok(state.journal.snapshot.language_servers.clone());
    }
    let mut result = state.journal.snapshot.language_servers.clone();
    let mut events = Vec::new();
    for mut server in live {
        let old = result.iter_mut().find(|s| s.id == server.id);
        let changed = old.as_ref().is_none_or(|old| {
            old.state != server.state
                || old.reason != server.reason
                || old.shared_references != server.shared_references
        });
        server.updated_at = if changed {
            crate::journal::now()
        } else {
            old.as_ref().unwrap().updated_at
        };
        if changed {
            events.push(EventPayload::LanguageServerUpdated {
                server: server.clone(),
            });
        }
        if let Some(old) = old {
            *old = server;
        } else {
            result.push(server);
        }
    }
    if !events.is_empty() {
        state.journal.append(events).await?;
    }
    Ok(result)
}
impl crate::Session {
    pub async fn language_servers(&self) -> Result<LanguageServerListResponse, crate::CoreError> {
        Ok(LanguageServerListResponse {
            data: sync_states(&self.shared).await?,
        })
    }
    pub async fn diagnostics(&self) -> DiagnosticListResponse {
        let Some(lsp) = self.shared.tools.lsp() else {
            return DiagnosticListResponse { data: vec![] };
        };
        let batches = lsp.diagnostics().await;
        let cwd = self
            .shared
            .state
            .lock()
            .await
            .journal
            .snapshot
            .settings
            .cwd
            .clone();
        DiagnosticListResponse {
            data: batches
                .iter()
                .map(|b| {
                    public_file(
                        b,
                        &b.diagnostics
                            .iter()
                            .filter(|d| matches!(d["severity"].as_u64(), Some(1 | 2)))
                            .cloned()
                            .collect::<Vec<_>>(),
                        lsp,
                        &cwd,
                    )
                })
                .collect(),
        }
    }
    pub async fn stop_language_server(
        &self,
        id: String,
    ) -> Result<LanguageServerListResponse, crate::CoreError> {
        let session = self.clone();
        crate::session::owned(async move {
            let lsp = session.shared.tools.lsp().ok_or_else(|| {
                crate::CoreError::new(ErrorCode::InvalidRequest, "LSP is disabled")
            })?;
            lsp.stop_server(&id)
                .await
                .map_err(|e| crate::CoreError::new(ErrorCode::InvalidRequest, &e.message))?;
            session.language_servers().await
        })
        .await
    }
    pub async fn retry_language_server(
        &self,
        id: String,
    ) -> Result<LanguageServerListResponse, crate::CoreError> {
        let session = self.clone();
        crate::session::owned(async move {
            let lsp = session.shared.tools.lsp().ok_or_else(|| {
                crate::CoreError::new(ErrorCode::InvalidRequest, "LSP is disabled")
            })?;
            lsp.retry_server(&id)
                .await
                .map_err(|e| crate::CoreError::new(ErrorCode::InvalidRequest, &e.message))?;
            session.language_servers().await
        })
        .await
    }
}
