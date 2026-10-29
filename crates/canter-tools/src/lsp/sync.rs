use super::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;
mod files;
use files::read_text;
mod control;
mod delivery;
mod pull;
pub(super) mod registration;
mod watcher;
pub use delivery::DiagnosticSubscription;

const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 64;
const CACHE_BYTES: usize = 1024 * 1024;
const FILE_DIAGNOSTICS_BYTES: usize = 256 * 1024;
static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);

type Reply = watch::Sender<Option<Result<DiagnosticBatch, LspError>>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Current,
    VersionUnconfirmed,
    Deleted,
    Stale,
}
#[derive(Clone, Debug)]
pub struct DiagnosticBatch {
    pub root: PathBuf,
    pub received_at: tokio::time::Instant,
    pub collected_at: i64,
    pub id: u64,
    pub path: PathBuf,
    pub server_id: String,
    pub generation: u64,
    pub version: Option<i64>,
    pub digest: u64,
    /// Push replaces one whole URI; pull is separately keyed by provider identifier.
    pub source: String,
    pub diagnostics: Vec<Value>,
    pub freshness: Freshness,
    pub incomplete: bool,
    pub reason: Option<String>,
}
#[derive(Clone)]
pub struct LspSession(Arc<SessionInner>);
#[derive(Clone)]
pub struct ChangeTicket {
    pub path: PathBuf,
    pub changed_at: tokio::time::Instant,
    expected_digest: Option<(u64, FileStamp)>,
    receiver: watch::Receiver<Option<Result<DiagnosticBatch, LspError>>>,
    session: Weak<SessionInner>,
}
impl std::fmt::Debug for ChangeTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChangeTicket")
    }
}
struct SessionInner {
    service: Arc<LspService>,
    context: Mutex<FileContext>,
    protected: Vec<PathBuf>,
    state: Mutex<SessionState>,
    changed: Arc<Notify>,
    delivered: Arc<Notify>,
    stop: CancellationToken,
}
#[derive(Default)]
struct SessionState {
    restored_ids: BTreeMap<(String, PathBuf), String>,
    subscriptions: Vec<control::Subscription>,
    delivery: delivery::Delivery,
    workspaces: BTreeMap<u64, Arc<Workspace>>,
    interests: BTreeSet<PathBuf>,
    tickets: BTreeMap<PathBuf, Reply>,
    pending: BTreeSet<PathBuf>,
}
impl SessionState {
    fn track(&mut self, path: &Path) -> bool {
        if self.interests.contains(path) {
            return true;
        }
        if self.interests.len() >= 1024
            || self
                .interests
                .iter()
                .map(|p| p.as_os_str().len() + 256)
                .sum::<usize>()
                + path.as_os_str().len()
                + 256
                > CACHE_BYTES
        {
            return false;
        }
        self.interests.insert(path.to_owned());
        true
    }
}
impl Drop for SessionInner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl LspService {
    pub fn session(self: &Arc<Self>, context: FileContext, protected: Vec<PathBuf>) -> LspSession {
        let session = Arc::new(SessionInner {
            service: self.clone(),
            context: Mutex::new(context),
            protected: protected.into_iter().map(|p| canonical(&p)).collect(),
            state: Mutex::default(),
            changed: Arc::new(Notify::new()),
            delivered: Arc::new(Notify::new()),
            stop: CancellationToken::new(),
        });
        let weak = Arc::downgrade(&session);
        let stop = session.stop.clone();
        tokio::spawn(async move {
            loop {
                let Some(session) = weak.upgrade() else {
                    break;
                };
                let path = session.state.lock().unwrap().pending.pop_first();
                if let Some(path) = path {
                    let existing = session
                        .state
                        .lock()
                        .unwrap()
                        .workspaces
                        .values()
                        .find(|w| {
                            !path.exists()
                                && w.instance.is_running()
                                && path.starts_with(&w.instance.root)
                                && w.matches(&path)
                        })
                        .cloned();
                    if let Some(workspace) = existing {
                        if let Err(error) = workspace.synchronize(&path, true).await {
                            session.fail(&path, error);
                        }
                        continue;
                    }
                    let context = session.context.lock().unwrap().clone();
                    let service = session.service.clone();
                    let subscription = session.subscription(&path);
                    drop(session);
                    let result = match subscription {
                        Ok(token) => service.for_file(&path, &context, &token).await,
                        Err(error) => Err(error),
                    };
                    let Some(session) = weak.upgrade() else {
                        break;
                    };
                    let result = match result {
                        Ok(Some(instance)) if session.allowed(&path) => {
                            let workspace = instance.workspace();
                            workspace.attach(&session);
                            match workspace.watch().await {
                                Ok(()) => workspace.synchronize(&path, true).await,
                                Err(error) => {
                                    workspace.mark_stale(&error.message).await;
                                    Err(error)
                                }
                            }
                        }
                        Ok(_) => Err(LspError::new(
                            ErrorKind::Unsupported,
                            "LSP is disabled for this modification",
                        )),
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        session.fail(&path, error);
                    }
                } else {
                    // Do not keep a session alive while waiting for the next modification.
                    let changed = session.changed.clone();
                    drop(session);
                    tokio::select! { _ = stop.cancelled() => break, _ = changed.notified() => {} }
                }
                if stop.is_cancelled() {
                    break;
                }
            }
        });
        LspSession(session)
    }
}
impl LspSession {
    pub fn configure(&self, additional_directories: Vec<PathBuf>, round: Round) {
        self.0.context.lock().unwrap().additional_directories = additional_directories;
        self.set_round(round);
    }
    pub fn set_round(&self, round: Round) {
        let previous = std::mem::replace(&mut self.0.context.lock().unwrap().round, round);
        if previous != Round::Modify && round == Round::Modify {
            let session = self.clone();
            tokio::spawn(async move {
                session.reconcile().await;
            });
        }
    }
    pub fn changed(&self, path: &Path) -> ChangeTicket {
        let changed_at = tokio::time::Instant::now();
        let (reply, receiver) = watch::channel(None);
        let path = canonical(&self.0.context.lock().unwrap().cwd.join(path));
        if self.0.stop.is_cancelled() || !self.0.allowed(&path) {
            reply.send_replace(Some(Err(LspError::new(
                ErrorKind::Unsupported,
                "File is outside LSP synchronization scope",
            ))));
            return ChangeTicket {
                path,
                changed_at,
                expected_digest: None,
                receiver,
                session: Arc::downgrade(&self.0),
            };
        }
        if let Err(error) = self.0.subscription(&path) {
            reply.send_replace(Some(Err(error)));
            return ChangeTicket {
                path,
                changed_at,
                expected_digest: None,
                receiver,
                session: Arc::downgrade(&self.0),
            };
        }
        let mut state = self.0.state.lock().unwrap();
        if !state.track(&path) {
            reply.send_replace(Some(Err(LspError::new(
                ErrorKind::ResourceLimited,
                "LSP session file limit exceeded; diagnostics are incomplete",
            ))));
            return ChangeTicket {
                path,
                changed_at,
                expected_digest: None,
                receiver,
                session: Arc::downgrade(&self.0),
            };
        }
        if let Some(old) = state.tickets.insert(path.clone(), reply) {
            old.send_replace(Some(Err(LspError::new(
                ErrorKind::Cancelled,
                "File changed again",
            ))));
        }
        state.delivery.clear_file(&path);
        state.pending.insert(path.clone());
        drop(state);
        self.0.changed.notify_one();
        ChangeTicket {
            path,
            changed_at,
            expected_digest: None,
            receiver,
            session: Arc::downgrade(&self.0),
        }
    }
    pub async fn close(&self) {
        self.0.stop.cancel();
        let mut state = self.0.state.lock().unwrap();
        state.workspaces.clear();
        state.pending.clear();
        for (_, ticket) in std::mem::take(&mut state.tickets) {
            ticket.send_replace(Some(Err(LspError::new(
                ErrorKind::Stopped,
                "LSP session is closed",
            ))));
        }
    }
}
impl ChangeTicket {
    pub fn failure(&self) -> Option<LspError> {
        self.receiver
            .borrow()
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned()
    }
    pub fn with_content(mut self, content: &str) -> Self {
        self.expected_digest = std::fs::metadata(&self.path)
            .ok()
            .map(|metadata| (digest(content), file_stamp(&metadata)));
        self
    }
    pub async fn is_current(&self) -> bool {
        match self.expected_digest {
            Some((expected, stamp)) => {
                read_text(&self.path)
                    .await
                    .is_ok_and(|text| digest(&text) == expected)
                    && std::fs::metadata(&self.path)
                        .is_ok_and(|metadata| file_stamp(&metadata) == stamp)
            }
            None => false,
        }
    }
    pub async fn wait(&self, timeout: Duration) -> Result<DiagnosticBatch, LspError> {
        let mut receiver = self.receiver.clone();
        tokio::time::timeout(timeout, async {
            loop {
                let result = receiver.borrow_and_update().clone();
                if let Some(result) = result {
                    let batch = result?;
                    if let Some(session) = self.session.upgrade()
                        && session.current(&batch).await
                        && (self.expected_digest.is_none() || self.is_current().await)
                    {
                        return Ok(batch);
                    }
                    return Err(LspError::new(
                        ErrorKind::Cancelled,
                        "LSP diagnostics are no longer current",
                    ));
                }
                receiver
                    .changed()
                    .await
                    .map_err(|_| LspError::new(ErrorKind::Stopped, "LSP session is closed"))?;
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(LspError::new(
                ErrorKind::Timeout,
                "Waiting for LSP diagnostics timed out",
            ))
        })
    }
}
type FileStamp = (u64, u64, i64, i64, u64);
fn file_stamp(metadata: &std::fs::Metadata) -> FileStamp {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.len(),
    )
}
impl SessionInner {
    fn allowed(&self, path: &Path) -> bool {
        let context = self.context.lock().unwrap();
        context.round == Round::Modify
            && !self.state.lock().unwrap().subscriptions.iter().any(|s| {
                s.stopped
                    && path.starts_with(&s.root)
                    && self
                        .service
                        .selection_info(path, &context)
                        .is_ok_and(|(name, root)| name == s.name && root == s.root)
            })
            && std::iter::once(&context.cwd)
                .chain(&context.additional_directories)
                .any(|root| {
                    let root = canonical(root);
                    path.strip_prefix(root).is_ok_and(|relative| {
                        !relative.components().any(|c| {
                            matches!(
                                c.as_os_str().to_str(),
                                Some(
                                    ".git"
                                        | "target"
                                        | "node_modules"
                                        | ".venv"
                                        | "venv"
                                        | "__pycache__"
                                )
                            )
                        })
                    })
                })
            && !self.protected.iter().any(|p| path.starts_with(p))
    }
    fn fail(&self, path: &Path, error: LspError) {
        let mut state = self.state.lock().unwrap();
        for subscription in &mut state.subscriptions {
            if path == subscription.file {
                subscription.error = Some(error.message.clone());
            }
        }
        if let Some(reply) = state.tickets.remove(path) {
            reply.send_replace(Some(Err(error)));
        }
    }
    fn deliver(&self, mut batch: DiagnosticBatch) {
        let mut state = self.state.lock().unwrap();
        if state
            .subscriptions
            .iter()
            .any(|s| s.stopped && s.name == batch.server_id && s.root == batch.root)
        {
            return;
        }
        if !state.interests.contains(&batch.path) {
            return;
        }
        batch.id = NEXT_BATCH.fetch_add(1, Ordering::Relaxed);
        batch.received_at = tokio::time::Instant::now();
        batch.collected_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let batch = state.delivery.insert(batch);
        if let Some(reply) = state.tickets.remove(&batch.path) {
            reply.send_replace(Some(Ok(batch)));
        }
        self.delivered.notify_waiters();
    }
}

pub(super) struct Workspace {
    registrations: Mutex<BTreeMap<String, Vec<registration::Pattern>>>,
    watcher: Arc<watcher::WatchState>,
    next_version: AtomicU64,
    instance: Instance,
    documents: tokio::sync::Mutex<BTreeMap<PathBuf, Document>>,
    sessions: Mutex<Vec<Weak<SessionInner>>>,
    stop: CancellationToken,
}
struct Document {
    text: String,
    digest: u64,
    version: i64,
    cancel: CancellationToken,
    result_id: Option<String>,
}
impl Drop for Document {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.stop.cancel();
        self.watcher.close();
    }
}
impl Workspace {
    pub(super) fn subscribers(&self) -> u32 {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|s| {
                s.state
                    .lock()
                    .unwrap()
                    .workspaces
                    .contains_key(&self.instance.generation())
            })
            .count() as u32
    }
    pub(super) fn new(instance: Instance) -> Arc<Self> {
        let workspace = Arc::new(Self {
            instance,
            registrations: Mutex::default(),
            watcher: Arc::default(),
            next_version: AtomicU64::new(1),
            documents: tokio::sync::Mutex::new(BTreeMap::new()),
            sessions: Mutex::default(),
            stop: CancellationToken::new(),
        });
        let weak = Arc::downgrade(&workspace);
        let events = workspace.instance.events();
        let stop = workspace.stop.clone();
        let closed = workspace.instance.closed();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! { _ = stop.cancelled() => break, _ = closed.cancelled() => {
                    if let Some(workspace) = weak.upgrade() { workspace.mark_stale("LSP transport closed; diagnostics are incomplete and may be stale").await; workspace.watcher.close(); }
                    break;
                }, event = events.next() => event };
                let Some(workspace) = weak.upgrade() else {
                    break;
                };
                match event {
                    Ok(event) if event["method"] == "workspace/diagnostic/refresh" => {
                        workspace.refresh().await
                    }
                    Ok(event)
                        if matches!(
                            event["method"].as_str(),
                            Some("client/registerCapability" | "client/unregisterCapability")
                        ) =>
                    {
                        workspace.registration(event).await
                    }
                    Ok(event) => workspace.publish(event).await,
                    Err(error) => workspace.mark_stale(&error.message).await,
                }
            }
        });
        workspace
    }
    fn attach(self: &Arc<Self>, session: &Arc<SessionInner>) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|s| s.strong_count() > 0);
        if !sessions.iter().any(|s| s.ptr_eq(&Arc::downgrade(session))) {
            sessions.push(Arc::downgrade(session));
        }
        let mut state = session.state.lock().unwrap();
        state
            .workspaces
            .retain(|_, workspace| workspace.instance.is_running());
        state
            .workspaces
            .insert(self.instance.generation(), self.clone());
    }
    async fn synchronize(self: &Arc<Self>, path: &Path, explicit: bool) -> Result<(), LspError> {
        if !self.allowed(path) {
            return Err(LspError::new(
                ErrorKind::Unsupported,
                "File is outside LSP synchronization scope",
            ));
        }
        let sync = &self.instance.capabilities["textDocumentSync"];
        let kind = sync
            .as_u64()
            .or_else(|| sync["change"].as_u64())
            .unwrap_or(0);
        if !matches!(kind, 1 | 2)
            || self.instance.capabilities["positionEncoding"]
                .as_str()
                .is_some_and(|encoding| encoding != "utf-16")
        {
            return Err(LspError::new(
                ErrorKind::Unsupported,
                "Unsupported LSP document synchronization capabilities",
            ));
        }
        let open_close = sync.is_number() || sync["openClose"] == true;
        let mut documents = self.documents.lock().await;
        if !path
            .try_exists()
            .map_err(|_| LspError::new(ErrorKind::Unavailable, "Cannot inspect LSP document"))?
        {
            if documents.remove(path).is_some() {
                if open_close {
                    self.instance
                        .notify(
                            "textDocument/didClose",
                            json!({"textDocument":{"uri":uri(path)?}}),
                        )
                        .await?;
                }
                self.instance
                    .notify(
                        "workspace/didChangeWatchedFiles",
                        json!({"changes":[{"uri":uri(path)?,"type":3}]}),
                    )
                    .await?;
                let batch = DiagnosticBatch {
                    root: self.instance.root.clone(),
                    received_at: tokio::time::Instant::now(),
                    collected_at: 0,
                    id: 0,
                    path: path.into(),
                    server_id: self.instance.server_id.clone(),
                    generation: self.instance.generation(),
                    version: Some(self.next_version.fetch_add(1, Ordering::Relaxed) as i64),
                    digest: 0,
                    source: "push".into(),
                    diagnostics: vec![],
                    freshness: Freshness::Deleted,
                    incomplete: false,
                    reason: None,
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
            return Ok(());
        }
        let text = read_text(path).await?;
        let digest = digest(&text);
        if documents.get(path).is_some_and(|d| d.digest == digest) {
            if !explicit {
                return Ok(());
            }
            // A second session may subscribe after the original push. Its own
            // successful modification still needs an attributable result.
            let pending = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter_map(Weak::upgrade)
                .any(|s| s.state.lock().unwrap().tickets.contains_key(path));
            if !pending {
                return Ok(());
            }
        }
        if let Some(old) = documents.get(path) {
            old.cancel.cancel();
        }
        let version = self.next_version.fetch_add(1, Ordering::Relaxed) as i64;
        let document_uri = uri(path)?;
        if let Some(old) = documents.get(path) {
            let change = if kind == 2 {
                let lines: Vec<_> = old.text.split('\n').collect();
                json!({"range":{"start":{"line":0,"character":0},"end":{"line":lines.len()-1,"character":lines.last().unwrap().encode_utf16().count()}},"text":text})
            } else if kind == 1 {
                json!({"text":text})
            } else {
                return Err(LspError::new(
                    ErrorKind::Unsupported,
                    "LSP server does not support document changes",
                ));
            };
            self.instance.notify("textDocument/didChange", json!({"textDocument":{"uri":document_uri,"version":version},"contentChanges":[change]})).await?;
        } else {
            if documents.len() >= MAX_DOCUMENTS {
                let oldest = documents
                    .iter()
                    .min_by_key(|(_, d)| d.version)
                    .map(|(p, _)| p.clone())
                    .unwrap();
                let old = documents.remove(&oldest).unwrap();
                if open_close {
                    self.instance
                        .notify(
                            "textDocument/didClose",
                            json!({"textDocument":{"uri":uri(&oldest)?}}),
                        )
                        .await?;
                }
                let batch = DiagnosticBatch {
                    root: self.instance.root.clone(),
                    received_at: tokio::time::Instant::now(),
                    collected_at: 0,
                    id: 0,
                    path: oldest,
                    server_id: self.instance.server_id.clone(),
                    generation: self.instance.generation(),
                    version: Some(old.version),
                    digest: old.digest,
                    source: "status".into(),
                    diagnostics: vec![],
                    freshness: Freshness::Stale,
                    incomplete: true,
                    reason: Some(
                        "LSP open document limit exceeded; this file is no longer covered".into(),
                    ),
                };
                for session in self
                    .sessions
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(Weak::upgrade)
                {
                    session
                        .state
                        .lock()
                        .unwrap()
                        .delivery
                        .clear_file(&batch.path);
                    session.deliver(batch.clone());
                }
            }
            let language = path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|e| {
                    self.instance
                        .config()
                        .extensions
                        .get(e)
                        .or_else(|| self.instance.config().extensions.get(&format!(".{e}")))
                })
                .ok_or_else(|| {
                    LspError::new(ErrorKind::Unsupported, "Unsupported LSP file extension")
                })?;
            if open_close {
                self.instance.notify("textDocument/didOpen", json!({"textDocument":{"uri":document_uri,"languageId":language,"version":version,"text":text}})).await?;
            } else {
                self.instance.notify("textDocument/didChange", json!({"textDocument":{"uri":document_uri,"version":version},"contentChanges":[{"text":text}]})).await?;
            }
        }
        let save = &self.instance.capabilities["textDocumentSync"]["save"];
        if save == true || save.is_object() {
            let mut params = json!({"textDocument":{"uri":document_uri}});
            if save["includeText"] == true {
                params["text"] = json!(text);
            }
            self.instance.notify("textDocument/didSave", params).await?;
        }
        documents.insert(
            path.into(),
            Document {
                text,
                digest,
                version,
                cancel: self.stop.child_token(),
                result_id: None,
            },
        );
        drop(documents);
        self.pull(path).await;
        Ok(())
    }
    async fn publish(&self, event: Value) {
        if !self.instance.is_running() {
            return;
        }
        let params = &event["params"];
        let Some(path) = params["uri"]
            .as_str()
            .and_then(|s| reqwest::Url::parse(s).ok())
            .and_then(|u| u.to_file_path().ok())
        else {
            return;
        };
        if !self.allowed(&path) {
            return;
        }
        let documents = self.documents.lock().await;
        let Some(document) = documents.get(&path) else {
            return;
        };
        let version = params["version"].as_i64();
        if version.is_some_and(|v| v != document.version) {
            return;
        }
        if !read_text(&path)
            .await
            .is_ok_and(|text| digest(&text) == document.digest)
        {
            return;
        }
        let Some(diagnostics) = params["diagnostics"].as_array() else {
            return;
        };
        let batch = DiagnosticBatch {
            root: self.instance.root.clone(),
            received_at: tokio::time::Instant::now(),
            collected_at: 0,
            id: 0,
            path,
            server_id: self.instance.server_id.clone(),
            generation: self.instance.generation(),
            version,
            digest: document.digest,
            source: "push".into(),
            diagnostics: diagnostics.clone(),
            freshness: if version.is_some() {
                Freshness::Current
            } else {
                Freshness::VersionUnconfirmed
            },
            incomplete: false,
            reason: None,
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
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .zip(path.file_name())
            .map_or_else(|| path.into(), |(p, n)| p.join(n))
    })
}
fn digest(text: &str) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    hash.finish()
}
fn uri(path: &Path) -> Result<String, LspError> {
    reqwest::Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|_| LspError::new(ErrorKind::Unsupported, "Unsupported LSP file path"))
}
#[cfg(test)]
mod tests;
