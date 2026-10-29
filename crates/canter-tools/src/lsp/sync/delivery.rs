use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Delivery {
    cache: VecDeque<DiagnosticBatch>,
    queue: VecDeque<DiagnosticBatch>,
    dropped_through: u64,
}
impl Delivery {
    pub fn clear_file(&mut self, path: &Path) {
        self.cache.retain(|b| b.path != path);
    }
    pub fn insert(&mut self, mut batch: DiagnosticBatch) -> DiagnosticBatch {
        if batch.freshness == Freshness::Deleted {
            self.clear_file(&batch.path);
        }
        self.cache.retain(|b| {
            !(b.path == batch.path
                && (b.generation != batch.generation || b.source == batch.source))
        });
        let used: usize = self
            .cache
            .iter()
            .filter(|b| b.path == batch.path)
            .map(size)
            .sum();
        let budget = FILE_DIAGNOSTICS_BYTES.saturating_sub(used);
        while size(&batch) > budget && !batch.diagnostics.is_empty() {
            batch.diagnostics.pop();
            incomplete(&mut batch, "LSP file diagnostic byte limit exceeded");
        }
        while self
            .cache
            .iter()
            .filter(|b| b.path == batch.path)
            .map(size)
            .sum::<usize>()
            + size(&batch)
            > FILE_DIAGNOSTICS_BYTES
        {
            let Some(index) = self.cache.iter().position(|b| b.path == batch.path) else {
                break;
            };
            self.cache.remove(index);
            incomplete(&mut batch, "LSP file diagnostic byte limit exceeded");
        }
        while self.cache.iter().map(size).sum::<usize>() + size(&batch) > CACHE_BYTES {
            if self.cache.pop_front().is_none() {
                break;
            }
            incomplete(&mut batch, "LSP session diagnostic cache limit exceeded");
        }
        while self.queue.iter().map(size).sum::<usize>() + size(&batch) > CACHE_BYTES {
            let Some(old) = self.queue.pop_front() else {
                break;
            };
            self.dropped_through = old.id;
            incomplete(&mut batch, "LSP diagnostic delivery queue limit exceeded");
        }
        // The bounded metadata is retained even when one diagnostic alone exceeded the limit.
        self.cache.push_back(batch.clone());
        self.queue.push_back(batch.clone());
        batch
    }
    pub fn cached(&self) -> Vec<DiagnosticBatch> {
        self.cache.iter().cloned().collect()
    }
}
fn size(batch: &DiagnosticBatch) -> usize {
    serde_json::to_vec(&batch.diagnostics).map_or(FILE_DIAGNOSTICS_BYTES + 1, |v| v.len())
        + batch.path.as_os_str().len()
        + batch.source.len()
        + batch.server_id.len()
        + batch.reason.as_ref().map_or(0, String::len)
        + 256
}
fn incomplete(batch: &mut DiagnosticBatch, reason: &str) {
    batch.incomplete = true;
    batch.reason = Some(reason.into());
}

/// A cursor into the session's byte-bounded queue. Falling behind is explicit.
pub struct DiagnosticSubscription {
    session: Weak<SessionInner>,
    cursor: u64,
}
impl DiagnosticSubscription {
    pub async fn recv(&mut self) -> Result<DiagnosticBatch, LspError> {
        loop {
            let session = self.session.upgrade().ok_or_else(closed)?;
            let changed = session.delivered.clone();
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let batch = {
                let state = session.state.lock().unwrap();
                state
                    .delivery
                    .queue
                    .iter()
                    .find(|b| b.id > self.cursor)
                    .cloned()
                    .map(|mut b| {
                        if self.cursor < state.delivery.dropped_through {
                            incomplete(&mut b, "LSP diagnostic subscriber fell behind");
                        }
                        b
                    })
            };
            if let Some(batch) = batch {
                self.cursor = batch.id;
                if session.current(&batch).await {
                    return Ok(batch);
                }
                continue;
            }
            let stop = session.stop.clone();
            drop(session);
            tokio::select! { _ = stop.cancelled() => return Err(closed()), _ = &mut notified => {} }
        }
    }
}
fn closed() -> LspError {
    LspError::new(ErrorKind::Stopped, "LSP session is closed")
}
impl LspSession {
    pub fn subscribe(&self) -> DiagnosticSubscription {
        DiagnosticSubscription {
            session: Arc::downgrade(&self.0),
            cursor: NEXT_BATCH.load(Ordering::Relaxed).saturating_sub(1),
        }
    }
    /// Revalidates generation and on-disk content at delivery time.
    pub async fn diagnostics(&self) -> Vec<DiagnosticBatch> {
        let cached = self.0.state.lock().unwrap().delivery.cached();
        let mut current = Vec::new();
        for batch in cached {
            if self.0.current(&batch).await {
                current.push(batch);
            }
        }
        current
    }
}
impl SessionInner {
    pub(super) async fn current(&self, batch: &DiagnosticBatch) -> bool {
        if self.stop.is_cancelled() {
            return false;
        }
        if batch.freshness == Freshness::Stale {
            return !self.state.lock().unwrap().delivery.cache.iter().any(|b| {
                b.path == batch.path && b.id > batch.id && b.freshness != Freshness::Stale
            });
        }
        let workspace = self
            .state
            .lock()
            .unwrap()
            .workspaces
            .get(&batch.generation)
            .cloned();
        let Some(workspace) = workspace.filter(|w| w.instance.is_running()) else {
            return false;
        };
        if batch.freshness == Freshness::Deleted {
            return matches!(batch.path.try_exists(), Ok(false));
        }
        if canonical(&batch.path) != batch.path {
            return false;
        }
        if !workspace
            .documents
            .lock()
            .await
            .get(&batch.path)
            .is_some_and(|d| {
                d.digest == batch.digest && batch.version.is_none_or(|v| v == d.version)
            })
        {
            return false;
        }
        read_text(&batch.path)
            .await
            .is_ok_and(|text| digest(&text) == batch.digest)
    }
}
