//! Serialize operations on one session without holding the live registry.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

type Entries = Mutex<HashMap<String, Weak<Entry>>>;

#[derive(Default)]
pub(crate) struct SessionLocks {
    entries: Arc<Entries>,
}

struct Entry {
    id: String,
    entries: Weak<Entries>,
    lock: Arc<AsyncMutex<()>>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(entries) = self.entries.upgrade() {
            let mut entries = entries.lock().unwrap();
            // Another caller may have replaced the expired weak entry before
            // this destructor acquired the map. Never remove its new lock.
            if entries
                .get(&self.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                entries.remove(&self.id);
            }
        }
    }
}

pub(crate) struct SessionGuard {
    _lock: OwnedMutexGuard<()>,
    _entry: Arc<Entry>,
}

impl SessionLocks {
    pub(crate) async fn lock(&self, id: &str) -> Arc<SessionGuard> {
        let entry = {
            let mut entries = self.entries.lock().unwrap();
            if let Some(entry) = entries.get(id).and_then(Weak::upgrade) {
                entry
            } else {
                let entry = Arc::new(Entry {
                    id: id.into(),
                    entries: Arc::downgrade(&self.entries),
                    lock: Arc::new(AsyncMutex::new(())),
                });
                entries.insert(id.into(), Arc::downgrade(&entry));
                entry
            }
        };
        let guard = entry.lock.clone().lock_owned().await;
        Arc::new(SessionGuard {
            _lock: guard,
            _entry: entry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_waiters_and_finished_operations_release_lock_entries() {
        let locks = SessionLocks::default();
        let first = locks.lock("same").await;
        let mut waiting = Box::pin(locks.lock("same"));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        let other = locks.lock("other").await;
        drop(waiting);
        drop(other);
        drop(first);
        assert!(locks.entries.lock().unwrap().is_empty());
        // Reusing an id after cleanup still serializes its next two callers.
        let next = locks.lock("same").await;
        let mut waiting = Box::pin(locks.lock("same"));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(next);
        drop(waiting.await);
        assert!(locks.entries.lock().unwrap().is_empty());
    }
}
