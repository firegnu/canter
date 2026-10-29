use super::*;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::os::unix::fs::MetadataExt;

const MAX_DIRECTORIES: usize = 4096;
const CONFIGS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "pyproject.toml",
    "pyrightconfig.json",
    "tsconfig.json",
    "jsconfig.json",
    "package.json",
    "compile_commands.json",
    ".clangd",
    "uv.lock",
    "poetry.lock",
];
#[derive(Default)]
struct Changes {
    paths: BTreeSet<PathBuf>,
    bytes: usize,
    failed: bool,
}
#[derive(Default)]
pub(super) struct WatchState {
    native: Mutex<Option<RecommendedWatcher>>,
    directories: Mutex<BTreeMap<PathBuf, (u64, u64)>>,
    configs: Mutex<BTreeMap<PathBuf, Option<u64>>>,
    changes: Mutex<Changes>,
    ready: Notify,
    initialized: tokio::sync::OnceCell<()>,
}
impl WatchState {
    pub(super) fn close(&self) {
        self.native.lock().unwrap().take();
    }
    pub(super) fn event(&self, event: notify::Result<notify::Event>) {
        let mut changes = self.changes.lock().unwrap();
        match event {
            Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                changes.failed |= event.need_rescan();
                for path in event.paths {
                    if changes.bytes + path.as_os_str().len() > CACHE_BYTES
                        || changes.paths.len() >= MAX_DIRECTORIES
                    {
                        changes.failed = true;
                        break;
                    }
                    if changes.paths.insert(path.clone()) {
                        changes.bytes += path.as_os_str().len();
                    }
                }
            }
            Err(_) => changes.failed = true,
            _ => return,
        }
        drop(changes);
        self.ready.notify_one();
    }
}
impl Workspace {
    pub(super) fn allowed(&self, path: &Path) -> bool {
        if !path.starts_with(&self.instance.root) {
            return false;
        }
        // Never traverse symlinks, including a formerly watched directory replaced by one.
        if path
            .ancestors()
            .take_while(|p| *p != self.instance.root)
            .any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
        {
            return false;
        }
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|s| s.allowed(path))
    }
    fn is_config(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| CONFIGS.contains(&s))
            || self
                .instance
                .config()
                .root_markers
                .iter()
                .any(|m| path.ends_with(m))
    }
    pub(super) fn matches(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| {
                self.instance.config().extensions.contains_key(ext)
                    || self
                        .instance
                        .config()
                        .extensions
                        .contains_key(&format!(".{ext}"))
            })
    }
    pub(super) async fn watch(self: &Arc<Self>) -> Result<(), LspError> {
        self.watcher.initialized.get_or_try_init(|| async {
            let workspace = self.clone();
            tokio::task::spawn_blocking(move || {
                for _ in 0..2 {
                    match workspace.install_watch() {
                        Ok(()) => return Ok(()),
                        Err(_) => {
                            workspace.watcher.close();
                            workspace.watcher.directories.lock().unwrap().clear();
                            workspace.watcher.configs.lock().unwrap().clear();
                        }
                    }
                }
                Err(watch_error())
            }).await.map_err(|_| watch_error())??;
            let weak = Arc::downgrade(self);
            let watcher = self.watcher.clone();
            let stop = self.stop.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! { _ = stop.cancelled() => break, _ = watcher.ready.notified() => {} }
                    tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(200)) => {} }
                    let changes = std::mem::take(&mut *watcher.changes.lock().unwrap());
                    let Some(workspace) = weak.upgrade() else { break; };
                    if changes.failed { workspace.restart("LSP file watcher failed; diagnostics may be stale").await; break; }
                    if workspace.external(changes.paths).await.is_err() {
                        workspace.restart("LSP file synchronization failed; diagnostics may be stale").await;
                        break;
                    }
                }
            });
            Ok(())
        }).await.map(|_| ())
    }
    fn install_watch(&self) -> Result<(), LspError> {
        let state = self.watcher.clone();
        let watcher = RecommendedWatcher::new(
            move |event| state.event(event),
            notify::Config::default().with_follow_symlinks(false),
        )
        .map_err(|_| watch_error())?;
        *self.watcher.native.lock().unwrap() = Some(watcher);
        self.add_directories(&self.instance.root, false)?;
        let mut configs = self.watcher.configs.lock().unwrap();
        for name in CONFIGS.iter().copied().chain(
            self.instance
                .config()
                .root_markers
                .iter()
                .map(String::as_str),
        ) {
            let path = self.instance.root.join(name);
            if self.allowed(&path) {
                configs.insert(path.clone(), config_stamp(&path));
            }
        }
        Ok(())
    }
    fn add_directories(&self, root: &Path, include_files: bool) -> Result<Vec<PathBuf>, LspError> {
        let mut todo = vec![root.to_owned()];
        let mut files = Vec::new();
        let mut visited = 0;
        while let Some(path) = todo.pop() {
            if !self.instance.is_running() || self.stop.is_cancelled() {
                return Err(watch_error());
            }
            visited += 1;
            if visited > 100_000 {
                return Err(watch_error());
            }
            if !self.allowed(&path) {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(watch_error()),
            };
            if metadata.is_dir() {
                let mut directories = self.watcher.directories.lock().unwrap();
                let stamp = (metadata.dev(), metadata.ino());
                if directories.insert(path.clone(), stamp) != Some(stamp) {
                    if directories.len() > MAX_DIRECTORIES {
                        return Err(watch_error());
                    }
                    self.watcher
                        .native
                        .lock()
                        .unwrap()
                        .as_mut()
                        .ok_or_else(watch_error)?
                        .watch(&path, RecursiveMode::NonRecursive)
                        .map_err(|_| watch_error())?;
                }
                drop(directories);
                for entry in std::fs::read_dir(path).map_err(|_| watch_error())? {
                    todo.push(entry.map_err(|_| watch_error())?.path());
                    if todo.len() > 100_000 {
                        return Err(watch_error());
                    }
                }
            } else if metadata.is_file() {
                if !include_files && self.is_config(&path) {
                    let mut configs = self.watcher.configs.lock().unwrap();
                    if configs.len() >= MAX_DIRECTORIES {
                        return Err(watch_error());
                    }
                    configs
                        .entry(path.clone())
                        .or_insert_with(|| config_stamp(&path));
                }
                if include_files && (self.matches(&path) || self.is_config(&path)) {
                    files.push(path);
                    if files.len() > MAX_DIRECTORIES {
                        return Err(watch_error());
                    }
                }
            }
        }
        Ok(files)
    }
    async fn external(self: &Arc<Self>, paths: BTreeSet<PathBuf>) -> Result<(), LspError> {
        let mut changed = BTreeSet::new();
        for path in paths {
            if !self.allowed(&path) {
                continue;
            }
            if path.is_dir() {
                if std::fs::metadata(&path).is_ok_and(|m| {
                    self.watcher.directories.lock().unwrap().get(&path) == Some(&(m.dev(), m.ino()))
                }) {
                    continue;
                }
                let workspace = self.clone();
                let new_files =
                    tokio::task::spawn_blocking(move || workspace.add_directories(&path, true))
                        .await
                        .map_err(|_| watch_error())??;
                changed.extend(new_files);
            } else {
                let removed: Vec<_> = self
                    .watcher
                    .directories
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|p| p.starts_with(&path))
                    .cloned()
                    .collect();
                for directory in removed {
                    self.watcher.directories.lock().unwrap().remove(&directory);
                    if let Some(watcher) = self.watcher.native.lock().unwrap().as_mut() {
                        let _ = watcher.unwatch(&directory);
                    }
                }
                changed.extend(
                    self.documents
                        .lock()
                        .await
                        .keys()
                        .filter(|p| p.starts_with(&path))
                        .cloned(),
                );
                changed.insert(path);
            }
        }
        for path in &changed {
            if self.is_config(path) {
                let stamp = config_stamp(path);
                let old = self
                    .watcher
                    .configs
                    .lock()
                    .unwrap()
                    .insert(path.clone(), stamp);
                if old != Some(stamp) {
                    self.restart("LSP project configuration changed; restarting")
                        .await;
                    return Ok(());
                }
            }
        }
        for path in changed {
            let kind = if path.exists() { 2 } else { 3 };
            if !self.matches(&path) {
                if self.registered(&path, if kind == 3 { 4 } else { 3 }) {
                    self.instance
                        .notify(
                            "workspace/didChangeWatchedFiles",
                            json!({"changes":[{"uri":uri(&path)?,"type":kind}]}),
                        )
                        .await?;
                }
                continue;
            }
            let opened = self.documents.lock().await.contains_key(&path);
            if !opened {
                self.instance.notify("workspace/didChangeWatchedFiles", json!({"changes":[{"uri":uri(&path)?,"type":if path.exists() {1} else {3}}]})).await?;
            }
            let mut interested = false;
            let mut limited = false;
            for session in self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter_map(Weak::upgrade)
            {
                if session.allowed(&path) {
                    if session.state.lock().unwrap().track(&path) {
                        interested = true;
                    } else {
                        limited = true;
                    }
                }
            }
            if limited {
                self.mark_stale("LSP session file limit exceeded; diagnostics are incomplete")
                    .await;
            }
            if !interested {
                continue;
            }
            if let Err(error) = self.synchronize(&path, false).await {
                self.mark_stale(&error.message).await;
            }
        }
        Ok(())
    }
    pub(super) async fn reconcile(self: &Arc<Self>) {
        let mut paths: BTreeSet<_> = self.documents.lock().await.keys().cloned().collect();
        paths.extend(self.watcher.configs.lock().unwrap().keys().cloned());
        if self.external(paths).await.is_err() {
            self.restart("LSP reconciliation failed; diagnostics may be stale")
                .await;
        }
    }
    pub(super) async fn mark_stale(&self, reason: &str) {
        let documents = self.documents.lock().await;
        for (path, doc) in documents.iter() {
            let batch = DiagnosticBatch {
                root: self.instance.root.clone(),
                received_at: tokio::time::Instant::now(),
                collected_at: 0,
                id: 0,
                path: path.clone(),
                server_id: self.instance.server_id.clone(),
                generation: self.instance.generation(),
                version: Some(doc.version),
                digest: doc.digest,
                source: "status".into(),
                diagnostics: vec![],
                freshness: Freshness::Stale,
                incomplete: true,
                reason: Some(reason.into()),
            };
            for session in self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter_map(Weak::upgrade)
            {
                session.deliver(batch.clone());
            }
        }
    }
    pub(super) async fn restart(&self, reason: &str) {
        self.mark_stale(reason).await;
        self.stop.cancel();
        self.watcher.native.lock().unwrap().take();
        self.instance.invalidate(reason).await;
        for session in self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            let mut state = session.state.lock().unwrap();
            state.workspaces.remove(&self.instance.generation());
            let paths: Vec<_> = state
                .interests
                .iter()
                .filter(|p| p.starts_with(&self.instance.root) && p.exists())
                .cloned()
                .collect();
            state.pending.extend(paths);
            session.changed.notify_one();
        }
    }
}
impl LspSession {
    /// Called after foreground or background shell completion; never activates LSP.
    pub async fn reconcile(&self) {
        let workspaces: Vec<_> = self
            .0
            .state
            .lock()
            .unwrap()
            .workspaces
            .values()
            .cloned()
            .collect();
        for workspace in workspaces {
            if workspace.instance.is_running() {
                workspace.reconcile().await;
            }
        }
    }
}
fn config_stamp(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut bytes = Vec::new();
    files::open_regular(path)
        .ok()?
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    Some(hash.finish())
}
fn watch_error() -> LspError {
    LspError::new(
        ErrorKind::Unavailable,
        "LSP file watcher failed or exceeded its limits; diagnostics may be stale",
    )
}
