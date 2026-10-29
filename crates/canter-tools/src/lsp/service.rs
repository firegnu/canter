use super::{transport::Connection, *};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Round {
    Modify,
    Plan,
    Review,
}

/// Client environment remains private, in memory, and is never formatted or serialized.
pub struct FileContext {
    pub cwd: PathBuf,
    pub additional_directories: Vec<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub round: Round,
}

#[derive(Clone)]
struct Limits {
    initialize: Duration,
    shutdown: Duration,
    idle: Duration,
    sample: Duration,
    single_bytes: u64,
    total_bytes: u64,
    backoff: Duration,
    crash_window: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(30),
            shutdown: Duration::from_secs(3),
            idle: Duration::from_secs(600),
            sample: Duration::from_secs(5),
            single_bytes: 4 * 1024 * 1024 * 1024,
            total_bytes: 8 * 1024 * 1024 * 1024,
            backoff: Duration::from_secs(60),
            crash_window: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, PartialEq)]
struct Key {
    root: PathBuf,
    id: String,
    config: ServerConfig,
    environment: BTreeMap<String, String>,
}

#[derive(Clone)]
enum State {
    Starting,
    Ready(Arc<Connection>, Value),
    Failed(LspError),
}

struct Entry {
    key: Key,
    state: watch::Sender<State>,
    stop: CancellationToken,
    done: CancellationToken,
    reason: Mutex<Option<LspError>>,
    users: AtomicUsize,
    active: AtomicUsize,
    last_used: Mutex<Instant>,
    changed: Arc<Notify>,
    executable: Mutex<Option<(PathBuf, BinaryStamp)>>,
    pid: AtomicU32,
    initialized: AtomicBool,
    restarting: bool,
}

type BinaryStamp = (u64, u64, u64, i64, i64);
fn executable(key: &Key) -> Result<(PathBuf, BinaryStamp), LspError> {
    let command = Path::new(&key.config.command);
    let candidates = if command.components().count() > 1 || command.is_absolute() {
        vec![key.root.join(command)]
    } else {
        key.environment
            .get("PATH")
            .map(|path| {
                std::env::split_paths(path)
                    .map(|part| key.root.join(part).join(command))
                    .collect()
            })
            .unwrap_or_default()
    };
    for candidate in candidates {
        if let Ok(metadata) = std::fs::metadata(&candidate)
            && metadata.is_file()
            && metadata.mode() & 0o111 != 0
        {
            // Keep argv[0]'s selected name: rustup and other multicall launchers
            // dispatch by it. Metadata follows symlinks to detect replacement.
            let path = candidate;
            return Ok((
                path,
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                ),
            ));
        }
    }
    Err(LspError::new(
        ErrorKind::Unavailable,
        "LSP executable is not installed in the session PATH",
    ))
}

impl Entry {
    fn stop(&self, reason: LspError) {
        let mut first = self.reason.lock().unwrap();
        let already_exited = matches!(&*self.state.borrow(),State::Ready(connection,_) if connection.closed.is_cancelled());
        if first.is_none() && !already_exited {
            *first = Some(reason);
        }
        self.stop.cancel();
    }
    async fn run(self: Arc<Self>, limits: Limits) {
        let start = async {
            let (program, stamp) = executable(&self.key)?;
            *self.executable.lock().unwrap() = Some((program.clone(), stamp));
            let connection = Connection::spawn(
                &program,
                &self.key.config.args,
                &self.key.root,
                &self.key.environment,
                &self.key.config.settings,
            )
            .await?;
            self.pid.store(connection.pid, Ordering::SeqCst);
            let uri = reqwest::Url::from_directory_path(&self.key.root).map_err(|_| {
                LspError::new(ErrorKind::Unsupported, "Unsupported LSP project root")
            })?;
            let initialize = connection.request("initialize", json!({
                "processId":std::process::id(), "clientInfo":{"name":"canter","version":env!("CARGO_PKG_VERSION")},
                "rootUri":uri.as_str(), "workspaceFolders":[{"uri":uri.as_str(),"name":self.key.id}],
                "initializationOptions":self.key.config.initialization_options,
                "capabilities":{"workspace":{"configuration":true,"applyEdit":false},"general":{"positionEncodings":["utf-16"]}}
            }), limits.initialize, &self.stop).await;
            let initialized = match initialize {
                Ok(value) if value["capabilities"].is_object() => value["capabilities"].clone(),
                Ok(_) => {
                    connection.kill().await;
                    return Err(LspError::new(
                        ErrorKind::Protocol,
                        "Invalid LSP initialize response",
                    ));
                }
                Err(error) => {
                    connection.kill().await;
                    return Err(error);
                }
            };
            if let Err(error) = connection.notify("initialized", json!({})).await {
                connection.kill().await;
                return Err(error);
            }
            Ok((connection, initialized))
        };
        // Cancellation is handled inside initialize; keep owning the process until it is reaped.
        let outcome = match start.await {
            Ok((connection, capabilities)) => {
                self.initialized.store(true, Ordering::SeqCst);
                *self.last_used.lock().unwrap() = Instant::now();
                self.state
                    .send_replace(State::Ready(connection.clone(), capabilities));
                tokio::select! {
                    _ = self.stop.cancelled() => {},
                    _ = connection.closed.cancelled() => {},
                }
                connection.shutdown(limits.shutdown).await;
                LspError::new(
                    ErrorKind::Unavailable,
                    "LSP server crashed; retry on the next modification",
                )
            }
            Err(error) => error,
        };
        let error = self.reason.lock().unwrap().clone().unwrap_or_else(|| {
            LspError::new(
                outcome.kind,
                format!("LSP server '{}': {}", self.key.id, outcome.message),
            )
        });
        self.state.send_replace(State::Failed(error));
        self.done.cancel();
        self.changed.notify_one();
    }
}

/// A retained subscription. Drop the last subscription to release an unused instance.
/// Acquiring/status queries do not count as document or diagnostic activity.
pub struct Instance {
    pub pid: u32,
    pub root: PathBuf,
    pub server_id: String,
    pub language_id: String,
    pub capabilities: Value,
    connection: Arc<Connection>,
    lease: Arc<Lease>,
}
struct Lease(Arc<Entry>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.users.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_one();
    }
}
impl Clone for Instance {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            root: self.root.clone(),
            server_id: self.server_id.clone(),
            language_id: self.language_id.clone(),
            capabilities: self.capabilities.clone(),
            connection: self.connection.clone(),
            lease: self.lease.clone(),
        }
    }
}
struct Activity(Arc<Entry>);
impl Drop for Activity {
    fn drop(&mut self) {
        *self.0.last_used.lock().unwrap() = Instant::now();
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_one();
    }
}
impl Instance {
    pub fn is_running(&self) -> bool {
        !self.lease.0.stop.is_cancelled() && !self.connection.closed.is_cancelled()
    }
    pub fn error(&self) -> Option<LspError> {
        if let Some(reason) = self.lease.0.reason.lock().unwrap().clone() {
            return Some(reason);
        }
        if let State::Failed(error) = &*self.lease.0.state.borrow() {
            return Some(error.clone());
        }
        (!self.is_running()).then(|| LspError::new(ErrorKind::Unavailable, "LSP server exited"))
    }
    fn activity(&self) -> Result<Activity, LspError> {
        if !self.is_running() {
            return Err(self
                .error()
                .unwrap_or_else(|| LspError::new(ErrorKind::Stopped, "LSP instance is stopped")));
        }
        self.lease.0.active.fetch_add(1, Ordering::SeqCst);
        Ok(Activity(self.lease.0.clone()))
    }
    /// For document synchronization/diagnostic requests, not status polling.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, LspError> {
        let _activity = self.activity()?;
        self.connection
            .request(method, params, timeout.min(Duration::from_secs(30)), cancel)
            .await
            .map_err(|error| self.error().unwrap_or(error))
    }
    /// Called only after a real document change; no model tool exposes this API.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        let _activity = self.activity()?;
        self.connection.notify(method, params).await
    }
}

#[derive(Default)]
struct Pool {
    entries: Vec<Arc<Entry>>,
    blocked: Vec<(Key, LspError)>,
    failures: Vec<Failure>,
}
struct Failure {
    key: Key,
    error: LspError,
    until: Instant,
    needs_restart: bool,
    restarts: VecDeque<Instant>,
}
impl Pool {
    fn reap(&mut self, limits: &Limits) {
        let entries = std::mem::take(&mut self.entries);
        for entry in entries {
            if !entry.done.is_cancelled() {
                self.entries.push(entry);
                continue;
            }
            if entry.reason.lock().unwrap().is_some() {
                continue;
            }
            let State::Failed(error) = entry.state.borrow().clone() else {
                continue;
            };
            let index = self
                .failures
                .iter()
                .position(|failure| failure.key == entry.key)
                .unwrap_or_else(|| {
                    self.failures.push(Failure {
                        key: entry.key.clone(),
                        error: error.clone(),
                        until: Instant::now(),
                        needs_restart: false,
                        restarts: VecDeque::new(),
                    });
                    self.failures.len() - 1
                });
            let failure = &mut self.failures[index];
            failure.error = error;
            failure.needs_restart = entry.initialized.load(Ordering::SeqCst) || entry.restarting;
            failure.until = Instant::now()
                + if entry.initialized.load(Ordering::SeqCst) {
                    Duration::ZERO
                } else {
                    limits.backoff
                };
            failure
                .restarts
                .retain(|time| time.elapsed() < limits.crash_window);
            if failure.needs_restart && failure.restarts.len() >= 2 {
                self.blocked.push((
                    entry.key.clone(),
                    LspError::new(
                        ErrorKind::Stopped,
                        "LSP automatic restart limit reached; explicit retry is required",
                    ),
                ));
            }
        }
    }
}

pub struct LspService {
    servers: BTreeMap<String, ServerConfig>,
    pool: Mutex<Pool>,
    stop: CancellationToken,
    changed: Arc<Notify>,
    limits: Limits,
}
impl LspService {
    pub fn new(config: LspConfig) -> Result<Arc<Self>, LspError> {
        Self::with_limits(config, Limits::default())
    }
    /// Clear this file's failure/backoff latch. Does not start a process or disturb a healthy shared instance.
    pub async fn retry(&self, file: &Path, context: &FileContext) -> Result<(), LspError> {
        let (key, _) = self.selection(file, context)?;
        let retiring = {
            let pool = self.pool.lock().unwrap();
            pool.entries.iter().find(|entry| entry.key == key && (entry.stop.is_cancelled() || matches!(&*entry.state.borrow(), State::Ready(connection,_) if connection.closed.is_cancelled())))
                .map(|entry| entry.done.clone())
        };
        if let Some(done) = retiring {
            done.cancelled().await;
        }
        let mut pool = self.pool.lock().unwrap();
        pool.reap(&self.limits);
        pool.failures.retain(|failure| failure.key != key);
        pool.blocked.retain(|(blocked, _)| *blocked != key);
        Ok(())
    }
    fn selection(&self, file: &Path, context: &FileContext) -> Result<(Key, String), LspError> {
        let extension = file.extension().and_then(|s| s.to_str()).unwrap_or("");
        let Some((id, config, language)) = self.servers.iter().find_map(|(id, config)| {
            config
                .extensions
                .get(extension)
                .or_else(|| config.extensions.get(&format!(".{extension}")))
                .map(|language| (id, config, language))
        }) else {
            return Err(LspError::new(
                ErrorKind::Unsupported,
                "No LSP server supports this file extension",
            ));
        };
        let root = project_root(file, context, &config.root_markers)?;
        Ok((
            Key {
                root,
                id: id.clone(),
                config: config.clone(),
                environment: context.environment.clone(),
            },
            language.clone(),
        ))
    }
    fn with_limits(config: LspConfig, limits: Limits) -> Result<Arc<Self>, LspError> {
        let service = Arc::new(Self {
            servers: config
                .resolve()
                .map_err(|s| LspError::new(ErrorKind::Unavailable, s))?,
            pool: Mutex::new(Pool::default()),
            stop: CancellationToken::new(),
            changed: Arc::new(Notify::new()),
            limits,
        });
        if !service.servers.is_empty() {
            let weak = Arc::downgrade(&service);
            let changed = service.changed.clone();
            let stop = service.stop.clone();
            let sample = service.limits.sample;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(sample);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut over = std::collections::HashMap::<u32, u8>::new();
                let mut total_over = 0_u8;
                loop {
                    let tick = tokio::select! { _ = stop.cancelled() => break, _ = changed.notified() => false, _ = interval.tick() => true };
                    let Some(service) = weak.upgrade() else {
                        break;
                    };
                    let entries = {
                        let mut pool = service.pool.lock().unwrap();
                        pool.reap(&service.limits);
                        pool.entries.clone()
                    };
                    for entry in &entries {
                        if entry.users.load(Ordering::SeqCst) == 0 {
                            entry.stop(LspError::new(
                                ErrorKind::Stopped,
                                "LSP instance has no subscribers",
                            ));
                        } else if matches!(*entry.state.borrow(), State::Ready(..))
                            && entry.active.load(Ordering::SeqCst) == 0
                            && entry.last_used.lock().unwrap().elapsed() >= service.limits.idle
                        {
                            entry.stop(LspError::new(
                                ErrorKind::Stopped,
                                "LSP instance reclaimed after inactivity",
                            ));
                        }
                    }
                    if tick {
                        let running: Vec<_> = entries
                            .iter()
                            .filter_map(|entry| {
                                let pid = entry.pid.load(Ordering::SeqCst);
                                (pid != 0 && !entry.stop.is_cancelled())
                                    .then(|| (entry.clone(), pid))
                            })
                            .collect();
                        let pids: Vec<_> = running.iter().map(|(_, pid)| *pid).collect();
                        if let Ok(memory) = super::memory::sample(&pids).await {
                            over.retain(|pid, _| pids.contains(pid));
                            let mut total =
                                memory.values().copied().fold(0_u64, u64::saturating_add);
                            total_over = if total > service.limits.total_bytes {
                                total_over.saturating_add(1)
                            } else {
                                0
                            };
                            for (_, pid) in &running {
                                let count = over.entry(*pid).or_default();
                                *count = if memory[pid] > service.limits.single_bytes {
                                    count.saturating_add(1)
                                } else {
                                    0
                                };
                            }
                            if total_over >= 2 || over.values().any(|count| *count >= 2) {
                                let mut ordered = running;
                                ordered.sort_by_key(|(entry, _)| {
                                    (
                                        entry.active.load(Ordering::SeqCst) != 0,
                                        *entry.last_used.lock().unwrap(),
                                    )
                                });
                                for (entry, pid) in ordered {
                                    if entry.active.load(Ordering::SeqCst) == 0
                                        || over.get(&pid).copied().unwrap_or(0) >= 2
                                        || (total_over >= 2 && total > service.limits.total_bytes)
                                    {
                                        let reason = LspError::new(
                                            ErrorKind::ResourceLimited,
                                            "LSP memory soft limit exceeded; use retry to start again",
                                        );
                                        service
                                            .pool
                                            .lock()
                                            .unwrap()
                                            .blocked
                                            .push((entry.key.clone(), reason.clone()));
                                        entry.stop(reason);
                                        total = total.saturating_sub(memory[&pid]);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }
        Ok(service)
    }
    pub async fn for_file(
        self: &Arc<Self>,
        file: &Path,
        context: &FileContext,
        cancel: &CancellationToken,
    ) -> Result<Option<Instance>, LspError> {
        if self.servers.is_empty() || context.round != Round::Modify {
            return Ok(None);
        }
        if self.stop.is_cancelled() {
            return Err(LspError::new(ErrorKind::Stopped, "LSP service is stopped"));
        }
        if cancel.is_cancelled() {
            return Err(LspError::new(ErrorKind::Cancelled, "LSP startup cancelled"));
        }
        let (key, language) = self.selection(file, context)?;
        let id = key.id.clone();
        let entry = loop {
            let retiring = {
                let mut pool = self.pool.lock().unwrap();
                if self.stop.is_cancelled() {
                    return Err(LspError::new(ErrorKind::Stopped, "LSP service is stopped"));
                }
                pool.reap(&self.limits);
                if let Some((_, error)) = pool.blocked.iter().find(|(blocked, _)| *blocked == key) {
                    return Err(error.clone());
                }
                if let Some(entry) = pool.entries.iter().find(|entry| entry.key == key) {
                    let closed = matches!(&*entry.state.borrow(),State::Ready(connection,_) if connection.closed.is_cancelled());
                    let changed = entry
                        .executable
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|old| executable(&key).as_ref().ok() != Some(old));
                    if changed {
                        entry.stop(LspError::new(
                            ErrorKind::Stopped,
                            "LSP executable changed; restarting",
                        ));
                    }
                    if entry.stop.is_cancelled() || closed {
                        entry.done.clone()
                    } else {
                        entry.users.fetch_add(1, Ordering::SeqCst);
                        break entry.clone();
                    }
                } else if pool.entries.len() >= 2 {
                    let idle = pool
                        .entries
                        .iter()
                        .filter(|entry| {
                            entry.active.load(Ordering::SeqCst) == 0
                                && !matches!(*entry.state.borrow(), State::Starting)
                        })
                        .min_by_key(|entry| *entry.last_used.lock().unwrap());
                    let Some(idle) = idle else {
                        return Err(LspError::new(
                            ErrorKind::ResourceLimited,
                            "All LSP instance slots are busy; skipping this request",
                        ));
                    };
                    idle.stop(LspError::new(
                        ErrorKind::Stopped,
                        "LSP instance reclaimed to free a slot",
                    ));
                    idle.done.clone()
                } else {
                    let mut restarting = false;
                    if let Some(failure) =
                        pool.failures.iter_mut().find(|failure| failure.key == key)
                    {
                        if Instant::now() < failure.until {
                            return Err(LspError::new(
                                failure.error.kind.clone(),
                                format!("{}; LSP startup is in backoff", failure.error.message),
                            ));
                        }
                        failure
                            .restarts
                            .retain(|time| time.elapsed() < self.limits.crash_window);
                        if failure.needs_restart {
                            restarting = true;
                            failure.restarts.push_back(Instant::now());
                            failure.needs_restart = false;
                        }
                    }
                    let (state, _) = watch::channel(State::Starting);
                    let entry = Arc::new(Entry {
                        key,
                        state,
                        stop: self.stop.child_token(),
                        done: CancellationToken::new(),
                        reason: Mutex::new(None),
                        users: AtomicUsize::new(1),
                        active: AtomicUsize::new(0),
                        last_used: Mutex::new(Instant::now()),
                        changed: self.changed.clone(),
                        executable: Mutex::new(None),
                        pid: AtomicU32::new(0),
                        initialized: AtomicBool::new(false),
                        restarting,
                    });
                    pool.entries.push(entry.clone());
                    tokio::spawn(entry.clone().run(self.limits.clone()));
                    break entry;
                }
            };
            tokio::select! {
                _ = retiring.cancelled() => {},
                _ = cancel.cancelled() => return Err(LspError::new(ErrorKind::Cancelled,"LSP startup cancelled")),
                _ = self.stop.cancelled() => return Err(LspError::new(ErrorKind::Stopped,"LSP service is stopped")),
            }
        };
        let lease = Arc::new(Lease(entry.clone()));
        let mut state = entry.state.subscribe();
        loop {
            if cancel.is_cancelled() {
                return Err(LspError::new(ErrorKind::Cancelled, "LSP startup cancelled"));
            }
            if self.stop.is_cancelled() {
                return Err(LspError::new(ErrorKind::Stopped, "LSP service is stopped"));
            }
            let current = state.borrow_and_update().clone();
            match current {
                State::Ready(connection, capabilities) => {
                    if let Some(reason) = entry.reason.lock().unwrap().clone() {
                        return Err(reason);
                    }
                    if connection.closed.is_cancelled() {
                        return Err(LspError::new(
                            ErrorKind::Unavailable,
                            "LSP server exited during startup",
                        ));
                    }
                    return Ok(Some(Instance {
                        pid: connection.pid,
                        root: entry.key.root.clone(),
                        server_id: id.clone(),
                        language_id: language.clone(),
                        capabilities,
                        connection,
                        lease,
                    }));
                }
                State::Failed(error) => return Err(error),
                State::Starting => {}
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(LspError::new(ErrorKind::Cancelled,"LSP startup cancelled")),
                _ = self.stop.cancelled() => return Err(LspError::new(ErrorKind::Stopped,"LSP service is stopped")),
                _ = state.changed() => {},
            }
        }
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let entries = self.pool.lock().unwrap().entries.clone();
        for entry in &entries {
            entry.stop(LspError::new(ErrorKind::Stopped, "LSP service is stopped"));
        }
        for entry in entries {
            entry.done.cancelled().await;
        }
        *self.pool.lock().unwrap() = Pool::default();
    }
}
impl Drop for LspService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn project_root(
    file: &Path,
    context: &FileContext,
    markers: &[String],
) -> Result<PathBuf, LspError> {
    let unsupported = || {
        LspError::new(
            ErrorKind::Unsupported,
            "No supported LSP project root within the session directories",
        )
    };
    if !context.cwd.is_absolute()
        || context
            .additional_directories
            .iter()
            .any(|dir| !dir.is_absolute())
    {
        return Err(unsupported());
    }
    let path = std::fs::canonicalize(context.cwd.join(file)).map_err(|_| unsupported())?;
    let boundaries: Vec<_> = std::iter::once(&context.cwd)
        .chain(&context.additional_directories)
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    let boundary = boundaries
        .iter()
        .filter(|dir| path.starts_with(dir))
        .max_by_key(|dir| dir.components().count())
        .ok_or_else(unsupported)?;
    for directory in path
        .parent()
        .ok_or_else(unsupported)?
        .ancestors()
        .take_while(|dir| dir.starts_with(boundary))
    {
        if markers.iter().any(|marker| directory.join(marker).exists()) {
            return Ok(directory.into());
        }
        // Do not cross the physical checkout/worktree boundary looking for a marker.
        if directory.join(".git").exists() {
            break;
        }
    }
    Err(unsupported())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::test_support::*;
    use serde_json::json;
    fn config() -> LspConfig {
        serde_json::from_value(json!({"enabled":true,"servers":{"fake":{
            "command":"fake-lsp","args":[],"extensions":{"fake":"fake"},"root_markers":["project.marker"]
        }}})).unwrap()
    }
    fn context(dir: &Path) -> FileContext {
        std::fs::write(dir.join("project.marker"), "").unwrap();
        std::fs::write(dir.join("main.fake"), "").unwrap();
        FileContext {
            cwd: dir.into(),
            additional_directories: vec![],
            environment: environment(dir, ""),
            round: Round::Modify,
        }
    }
    #[tokio::test]
    async fn path_lookup_preserves_symlink_launcher_argv_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(dir.path());
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let launcher = bin.join("fake-lsp");
        std::os::unix::fs::symlink(crate::lsp::test_support::executable(), &launcher).unwrap();
        context
            .environment
            .insert("PATH".into(), bin.to_string_lossy().into_owned());
        let service = LspService::new(config()).unwrap();
        let instance = service
            .for_file(
                &dir.path().join("main.fake"),
                &context,
                &CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        service.shutdown().await;
        let log =
            std::fs::read_to_string(dir.path().join(format!("{}.log", instance.pid))).unwrap();
        assert!(
            log.lines()
                .next()
                .unwrap()
                .contains(launcher.to_str().unwrap()),
            "argv[0] must retain the selected launcher name for multicall executables"
        );
    }
    #[tokio::test]
    async fn last_subscriber_release_stops_the_instance_and_a_remaining_subscriber_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path());
        let file = dir.path().join("main.fake");
        let service = LspService::new(config()).unwrap();
        let cancel = CancellationToken::new();
        let first = service
            .for_file(&file, &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        let second = service
            .for_file(&file, &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        let pid = first.pid;
        drop(first);
        assert_eq!(
            second
                .request("fixture/echo", json!({}), Duration::from_secs(5), &cancel)
                .await
                .unwrap(),
            "你好"
        );
        drop(second);
        until(|| gone(pid)).await;
        service.shutdown().await;
    }
    #[tokio::test]
    async fn a_third_instance_is_rejected_when_both_existing_instances_are_busy() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(dir.path());
        let file = dir.path().join("main.fake");
        let cancel = CancellationToken::new();
        let service = LspService::new(config()).unwrap();
        let mut running = Vec::new();
        for value in ["one", "two"] {
            context.environment.insert("TOOLCHAIN".into(), value.into());
            let instance = service
                .for_file(&file, &context, &cancel)
                .await
                .unwrap()
                .unwrap();
            let pid = instance.pid;
            let stop = cancel.clone();
            running.push(tokio::spawn(async move {
                instance
                    .request("fixture/wait", json!({}), Duration::from_secs(30), &stop)
                    .await
            }));
            until(|| {
                std::fs::read_to_string(dir.path().join(format!("{pid}.log")))
                    .unwrap_or_default()
                    .contains("fixture/wait")
            })
            .await;
        }
        context
            .environment
            .insert("TOOLCHAIN".into(), "three".into());
        let result = service.for_file(&file, &context, &cancel).await;
        cancel.cancel();
        for task in running {
            let _ = task.await.unwrap();
        }
        service.shutdown().await;
        assert_eq!(
            result
                .err()
                .expect("a third busy instance must be skipped")
                .kind,
            ErrorKind::ResourceLimited
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
    #[tokio::test]
    async fn idle_reclamation_ignores_acquires_and_status_queries() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path());
        let service = LspService::with_limits(
            config(),
            Limits {
                idle: Duration::from_millis(120),
                sample: Duration::from_millis(20),
                ..Limits::default()
            },
        )
        .unwrap();
        let file = dir.path().join("main.fake");
        let cancel = CancellationToken::new();
        let instance = service
            .for_file(&file, &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(instance.is_running());
            let shared = service
                .for_file(&file, &context, &cancel)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(instance.pid, shared.pid);
        }
        let stopped = tokio::time::timeout(
            Duration::from_secs(2),
            instance.connection.closed.cancelled(),
        )
        .await;
        service.shutdown().await;
        assert!(
            stopped.is_ok(),
            "status/acquires must not keep an idle instance alive"
        );
    }
    #[tokio::test]
    async fn rss_limits_stop_busy_process_trees_and_require_explicit_retry() {
        for (single_bytes, total_bytes) in [(1, u64::MAX), (u64::MAX, 1)] {
            let dir = tempfile::tempdir().unwrap();
            let context = context(dir.path());
            let service = LspService::with_limits(
                config(),
                Limits {
                    sample: Duration::from_millis(300),
                    single_bytes,
                    total_bytes,
                    ..Limits::default()
                },
            )
            .unwrap();
            let file = dir.path().join("main.fake");
            let cancel = CancellationToken::new();
            let instance = service
                .for_file(&file, &context, &cancel)
                .await
                .unwrap()
                .unwrap();
            let waiting = instance.clone();
            let token = cancel.clone();
            let request = tokio::spawn(async move {
                waiting
                    .request("fixture/wait", json!({}), Duration::from_secs(30), &token)
                    .await
            });
            let limited = tokio::time::timeout(
                Duration::from_secs(3),
                instance.connection.closed.cancelled(),
            )
            .await;
            cancel.cancel();
            let _ = request.await.unwrap();
            let again = service
                .for_file(&file, &context, &CancellationToken::new())
                .await;
            service.shutdown().await;
            assert!(limited.is_ok(), "RSS soft limit must stop the process");
            assert_eq!(
                again.err().expect("memory exit requires retry").kind,
                ErrorKind::ResourceLimited
            );
        }
    }
    #[tokio::test]
    async fn failed_start_backs_off_and_three_crashes_latch_until_retry() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(dir.path());
        let file = dir.path().join("main.fake");
        let cancel = CancellationToken::new();
        let service = LspService::with_limits(
            config(),
            Limits {
                backoff: Duration::from_millis(100),
                ..Limits::default()
            },
        )
        .unwrap();
        context
            .environment
            .insert("LSP_TEST_MODE".into(), "fail".into());
        assert!(service.for_file(&file, &context, &cancel).await.is_err());
        assert!(service.for_file(&file, &context, &cancel).await.is_err());
        let starts = std::fs::read_to_string(dir.path().join("starts"))
            .unwrap()
            .lines()
            .count();
        service.shutdown().await;
        assert_eq!(
            starts, 1,
            "a failed launch must not be retried during backoff"
        );
        let service = LspService::new(config()).unwrap();
        context
            .environment
            .insert("LSP_TEST_MODE".into(), "".into());
        for _ in 0..3 {
            let instance = service
                .for_file(&file, &context, &cancel)
                .await
                .unwrap()
                .unwrap();
            let _ = instance
                .request("fixture/crash", json!({}), Duration::from_secs(2), &cancel)
                .await;
            instance.lease.0.done.cancelled().await;
        }
        let fourth = service.for_file(&file, &context, &cancel).await;
        assert_eq!(
            fourth
                .err()
                .expect("automatic restart limit must latch")
                .kind,
            ErrorKind::Stopped
        );
        service.retry(&file, &context).await.unwrap();
        let retried = service
            .for_file(&file, &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert!(retried.is_running());
        service.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_during_initialize_and_unresponsive_shutdown_reap_processes() {
        for mode in ["hang_init", "hang_shutdown"] {
            let dir = tempfile::tempdir().unwrap();
            let mut context = context(dir.path());
            context
                .environment
                .insert("LSP_TEST_MODE".into(), mode.into());
            let file = dir.path().join("main.fake");
            let service = LspService::with_limits(
                config(),
                Limits {
                    shutdown: Duration::from_millis(100),
                    ..Limits::default()
                },
            )
            .unwrap();
            let starting = service.clone();
            let started = tokio::spawn(async move {
                starting
                    .for_file(&file, &context, &CancellationToken::new())
                    .await
            });
            until(|| dir.path().join("starts").exists()).await;
            let pid: u32 = std::fs::read_to_string(dir.path().join("starts"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let instance = if mode == "hang_shutdown" {
                Some(started.await.unwrap().unwrap().unwrap())
            } else {
                None
            };
            tokio::time::timeout(Duration::from_secs(3), service.shutdown())
                .await
                .unwrap();
            assert!(gone(pid));
            if let Some(instance) = instance {
                assert!(!instance.is_running());
            }
        }
    }

    #[tokio::test]
    async fn missing_executable_and_initialize_timeout_report_reasons_and_backoff() {
        for mode in ["missing", "hang_init"] {
            let dir = tempfile::tempdir().unwrap();
            let mut context = context(dir.path());
            context
                .environment
                .insert("LSP_TEST_MODE".into(), mode.into());
            if mode == "missing" {
                context.environment.remove("PATH");
            }
            let service = LspService::with_limits(
                config(),
                Limits {
                    initialize: if mode == "hang_init" {
                        Duration::from_millis(150)
                    } else {
                        Duration::from_secs(5)
                    },
                    backoff: Duration::from_secs(1),
                    ..Limits::default()
                },
            )
            .unwrap();
            let file = dir.path().join("main.fake");
            let first = service
                .for_file(&file, &context, &CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert_eq!(
                first.kind,
                if mode == "missing" {
                    ErrorKind::Unavailable
                } else {
                    ErrorKind::Timeout
                }
            );
            let second = service
                .for_file(&file, &context, &CancellationToken::new())
                .await
                .err()
                .unwrap();
            assert!(second.message.contains("backoff"));
            tokio::time::sleep(Duration::from_millis(1100)).await;
            context.environment.insert(
                "PATH".into(),
                crate::lsp::test_support::executable()
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if mode == "hang_init" {
                let _ = service
                    .for_file(&file, &context, &CancellationToken::new())
                    .await;
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("starts"))
                        .unwrap()
                        .lines()
                        .count(),
                    2
                );
            } else {
                let instance = service
                    .for_file(&file, &context, &CancellationToken::new())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(instance.is_running());
            }
            service.shutdown().await;
        }
    }

    #[tokio::test]
    async fn environment_worktrees_and_real_path_boundaries_are_respected() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let outside = dir.path().join("outside");
        for path in [&a, &b, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        let mut context = context(&a);
        for path in [&a, &b] {
            std::fs::write(path.join(".git"), "gitdir: same-common-dir").unwrap();
        }
        std::fs::write(b.join("project.marker"), "").unwrap();
        std::fs::write(b.join("main.fake"), "").unwrap();
        std::fs::write(outside.join("main.fake"), "").unwrap();
        std::os::unix::fs::symlink(outside.join("main.fake"), a.join("link.fake")).unwrap();
        let cancel = CancellationToken::new();
        let service = LspService::new(config()).unwrap();
        let first = service
            .for_file(&a.join("main.fake"), &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            service
                .for_file(&a.join("link.fake"), &context, &cancel)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::Unsupported
        );
        assert!(
            service
                .for_file(&b.join("main.fake"), &context, &cancel)
                .await
                .is_err()
        );
        context.additional_directories.push(b.clone());
        let second = service
            .for_file(&b.join("main.fake"), &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(first.root, second.root);
        assert_ne!(first.pid, second.pid);
        context
            .environment
            .get_mut("PATH")
            .unwrap()
            .push_str(":/bin");
        let third = service
            .for_file(&b.join("main.fake"), &context, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            second.pid, third.pid,
            "different PATH cannot share even the same binary"
        );
        assert!(gone(first.pid), "oldest idle instance frees the third slot");
        context.additional_directories.push(outside.clone());
        assert!(
            service
                .for_file(&outside.join("main.fake"), &context, &cancel)
                .await
                .is_err(),
            "no marker means unsupported"
        );
        service.shutdown().await;
    }
    #[tokio::test]
    async fn disabled_and_readonly_rounds_do_not_start_and_concurrent_acquires_share() {
        let dir = tempfile::tempdir().unwrap();
        let mut context = context(dir.path());
        let file = dir.path().join("main.fake");
        let cancel = CancellationToken::new();
        let disabled = LspService::new(LspConfig::default()).unwrap();
        assert!(
            disabled
                .for_file(&file, &context, &cancel)
                .await
                .unwrap()
                .is_none()
        );
        let service = LspService::new(config()).unwrap();
        for round in [Round::Plan, Round::Review] {
            context.round = round;
            assert!(
                service
                    .for_file(&file, &context, &cancel)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert!(!dir.path().join("starts").exists());
        context.round = Round::Modify;
        let (a, b) = tokio::join!(
            service.for_file(&file, &context, &cancel),
            service.for_file(&file, &context, &cancel)
        );
        let a = a.unwrap().expect("enabled LSP must start");
        let b = b.unwrap().unwrap();
        assert_eq!(a.pid, b.pid);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("starts"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        service.shutdown().await;
        assert!(gone(a.pid));
    }
}
