use super::*;
use serde::{Deserialize, Serialize};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path};

const DAY_MS: i64 = 86_400_000;

#[derive(Serialize, Deserialize)]
struct ArchiveRecord {
    session_id: String,
    event_seq: u64,
    rows: Vec<UsageRow>,
}

pub(crate) async fn cleanup_at(app: &Arc<App>, retention_days: u32, now: i64) {
    if retention_days == 0 {
        return;
    }
    let store = app.store.clone();
    let sessions = match tokio::task::spawn_blocking(move || store.list_all()).await {
        Ok(Ok(sessions)) => sessions,
        Ok(Err(error)) => {
            eprintln!("Warning: could not list sessions for cleanup: {error}");
            return;
        }
        Err(_) => {
            eprintln!("Warning: could not list sessions for cleanup");
            return;
        }
    };
    let cutoff = now.saturating_sub(i64::from(retention_days).saturating_mul(DAY_MS));
    let by_id = sessions
        .iter()
        .map(|session| (session.id.clone(), session.clone()))
        .collect::<HashMap<_, _>>();
    for root in sessions
        .iter()
        .filter(|session| session.parent.is_none() && session.updated_at < cutoff)
    {
        let operation = app.session_locks.lock(&root.id).await;
        let family = match agent_sessions::Family::lock(app, &root.id, operation).await {
            Ok(family) => family,
            Err(_) => continue,
        };
        let ids = family.ids();
        if family.has_opened_session()
            || ids
                .iter()
                .filter_map(|id| by_id.get(id))
                .any(|session| session.updated_at >= cutoff)
        {
            continue;
        }
        let guards = match family.guards(app).await {
            Ok(guards) => guards,
            Err(_) => continue,
        };
        if guards
            .iter()
            .any(|(_, guard)| guard.ensure_trashable().is_err())
        {
            continue;
        }
        drop(guards);
        let prices = app.metadata().2.prices;
        let mut archived = true;
        for id in &ids {
            let Some(summary) = by_id.get(id) else {
                archived = false;
                break;
            };
            if let Err(error) = archive(&app.home, &app.store, summary, &prices) {
                eprintln!("Warning: could not archive usage for session {id}: {error}");
                archived = false;
                break;
            }
        }
        if !archived {
            continue;
        }
        for id in ids.iter().rev() {
            let directory = app.store.session_dir(id).unwrap();
            if let Err(error) = std::fs::remove_dir_all(directory) {
                eprintln!("Warning: could not delete expired session {id}: {error}");
            }
        }
        app.registry_changed.notify_waiters();
    }
    cleanup_trash(app, cutoff).await;
}

async fn cleanup_trash(app: &Arc<App>, cutoff: i64) {
    let root = app.home.join("trash");
    if !root.exists() {
        return;
    }
    let store = match Store::new(root) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Warning: could not open trash for cleanup: {error}");
            return;
        }
    };
    let sessions = match store.list_all() {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("Warning: could not list trash for cleanup: {error}");
            return;
        }
    };
    let by_id = sessions
        .iter()
        .map(|session| (session.id.clone(), session.clone()))
        .collect::<HashMap<_, _>>();
    for root in sessions
        .iter()
        .filter(|session| session.parent.is_none() && session.updated_at < cutoff)
    {
        let mut ids = vec![root.id.clone()];
        let mut index = 0;
        while index < ids.len() {
            let parent_id = ids[index].clone();
            for child in sessions.iter().filter(|child| {
                child
                    .parent
                    .as_ref()
                    .is_some_and(|parent| parent.session_id == parent_id)
            }) {
                if !ids.contains(&child.id) {
                    ids.push(child.id.clone());
                }
            }
            index += 1;
        }
        if ids
            .iter()
            .filter_map(|id| by_id.get(id))
            .any(|session| session.updated_at >= cutoff)
        {
            continue;
        }
        let mut operations = Vec::new();
        for id in &ids {
            operations.push(app.session_locks.lock(id).await);
        }
        if ids
            .iter()
            .any(|id| !store.session_dir(id).unwrap().exists())
        {
            continue;
        }
        let prices = app.metadata().2.prices;
        let mut archived = true;
        for id in &ids {
            let Some(summary) = by_id.get(id) else {
                archived = false;
                break;
            };
            if let Err(error) = archive(&app.home, &store, summary, &prices) {
                eprintln!("Warning: could not archive usage for trashed session {id}: {error}");
                archived = false;
                break;
            }
        }
        if !archived {
            continue;
        }
        for id in ids.iter().rev() {
            if let Err(error) = std::fs::remove_dir_all(store.session_dir(id).unwrap()) {
                eprintln!("Warning: could not delete expired trashed session {id}: {error}");
            }
        }
        drop(operations);
    }
}

pub(crate) fn start(app: std::sync::Weak<App>) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(86_400));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let Some(app) = app.upgrade() else {
                return;
            };
            tokio::select! {
                _ = app.stopped.cancelled() => return,
                _ = ticks.tick() => {},
            }
            let days = app
                .reload
                .as_ref()
                .map_or(30, |reload| reload.session_retention_days());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            cleanup_at(&app, days, now).await;
        }
    });
}

fn archive(
    home: &Path,
    store: &Store,
    summary: &SessionSummary,
    prices: &canter_core::usage::PriceBook,
) -> Result<(), String> {
    if read_archive(home)?
        .get(&summary.id)
        .is_some_and(|record| record.event_seq == summary.last_event_seq)
    {
        return Ok(());
    }
    let events = store
        .events_after(&summary.id, 0)
        .map_err(|error| error.to_string())?;
    let ledger = canter_core::usage::Ledger::replay(&events, prices);
    let mut rows = BTreeMap::<(String, String), UsageRow>::new();
    for (timestamp, record) in ledger.records {
        let date = chrono::DateTime::from_timestamp_millis(timestamp)
            .ok_or_else(|| "invalid usage timestamp".to_owned())?
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string();
        let model = record.model.clone().unwrap_or_default();
        let row = rows
            .entry((date.clone(), model.clone()))
            .or_insert_with(|| UsageRow {
                date: Some(date),
                model: Some(model),
                ..UsageRow::default()
            });
        row.add(&record);
    }
    let record = ArchiveRecord {
        session_id: summary.id.clone(),
        event_seq: summary.last_event_seq,
        rows: rows.into_values().collect(),
    };
    let mut encoded = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    let path = home.join("usage-archive.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| error.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    file.write_all(&encoded)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn read_archive(home: &Path) -> Result<BTreeMap<String, ArchiveRecord>, String> {
    let contents = match std::fs::read_to_string(home.join("usage-archive.jsonl")) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut sessions = BTreeMap::new();
    for line in contents.lines() {
        let record: ArchiveRecord =
            serde_json::from_str(line).map_err(|error| error.to_string())?;
        sessions.insert(record.session_id.clone(), record);
    }
    Ok(sessions)
}

pub(super) fn archived_rows(
    home: &Path,
    live: &std::collections::HashSet<String>,
) -> Result<Vec<UsageRow>, String> {
    Ok(read_archive(home)?
        .into_values()
        .filter(|record| !live.contains(&record.session_id))
        .flat_map(|record| record.rows)
        .collect())
}
