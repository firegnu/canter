//! Local ChatGPT registrations. Only the daemon owns this module; no tokens
//! belong in session settings, events, command arguments or child environments.
mod http;
mod login;
mod manager;
mod preferences;
mod storage;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ChatgptAuth {
    inner: Arc<Inner>,
}
struct Inner {
    host_id: String,
    store: storage::Store,
    http: http::Http,
    records: Mutex<BTreeMap<String, Arc<Entry>>>,
    logins: Mutex<BTreeMap<String, CancellationToken>>,
    preferences: Mutex<preferences::Preferences>,
    consent_changes: tokio::sync::watch::Sender<()>,
}

impl ChatgptAuth {
    /// Open once in the daemon, after acquiring its server lease. A second
    /// manager for this directory is rejected, even in the same process.
    pub fn open(home: &Path) -> Result<Self, AuthError> {
        Self::with_http(home, http::Http::new()?)
    }
    fn with_http(home: &Path, http: http::Http) -> Result<Self, AuthError> {
        let (store, host_id) = storage::Store::open(home)?;
        let saved = store.load_records(&host_id)?;
        let preferences = store.load_preferences()?;
        if saved
            .iter()
            .any(|record| record.info.issuer != http.issuer())
        {
            return Err(ErrorKind::Storage.into());
        }
        let records = saved
            .into_iter()
            .map(|record| (record.info.id.clone(), Arc::new(Entry::new(record))))
            .collect();
        Ok(Self {
            inner: Arc::new(Inner {
                host_id,
                store,
                http,
                records: Mutex::new(records),
                logins: Mutex::new(BTreeMap::new()),
                preferences: Mutex::new(preferences),
                consent_changes: tokio::sync::watch::channel(()).0,
            }),
        })
    }

    pub fn host_id(&self) -> &str {
        &self.inner.host_id
    }

    pub(crate) fn consent_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.inner.consent_changes.subscribe()
    }

    pub async fn records(&self) -> Vec<RecordInfo> {
        let entries: Vec<_> = self.inner.records.lock().await.values().cloned().collect();
        let mut records = Vec::new();
        for entry in entries {
            records.push(entry.record.lock().await.info());
        }
        records
    }

    async fn entry(&self, record_id: &str) -> Result<Arc<Entry>, AuthError> {
        self.inner
            .records
            .lock()
            .await
            .get(record_id)
            .cloned()
            .ok_or(ErrorKind::NotLoggedIn.into())
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn for_test(
        home: &Path,
        origin: String,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self::with_http(home, http::Http::for_test(origin, clock)).unwrap()
    }
}

struct Entry {
    record: Mutex<Record>,
    blocked: AtomicBool,
    epoch: AtomicU64,
    publish: std::sync::Mutex<()>,
    refreshes: AtomicU64,
    refresh_error: std::sync::Mutex<Option<AuthError>>,
}
impl Entry {
    fn new(record: Record) -> Self {
        Self {
            blocked: AtomicBool::new(record.info.status == RecordStatus::SignedOut),
            record: Mutex::new(record),
            epoch: AtomicU64::new(0),
            publish: std::sync::Mutex::new(()),
            refreshes: AtomicU64::new(0),
            refresh_error: std::sync::Mutex::new(None),
        }
    }
}
#[derive(Clone)]
struct Record {
    info: RecordInfo,
    tokens: Option<TokenSet>,
    pending_revocation: Option<Secret>,
}
impl Record {
    fn info(&self) -> RecordInfo {
        RecordInfo {
            pending_revocation: self.pending_revocation.is_some(),
            ..self.info.clone()
        }
    }
    fn sign_out(&mut self) {
        self.pending_revocation = self
            .tokens
            .take()
            .and_then(|tokens| tokens.refresh)
            .or(self.pending_revocation.take());
        self.info.status = RecordStatus::SignedOut;
    }
}
#[derive(Clone, Deserialize)]
#[serde(transparent)]
struct Secret(String);
#[derive(Clone, Deserialize)]
struct TokenSet {
    access: Secret,
    refresh: Option<Secret>,
    id: Option<Secret>,
    scopes: Vec<String>,
    received_at: u64,
    expires_in: u64,
    earliest_refresh_at: Option<serde_json::Value>,
}
impl TokenSet {
    fn status(&self) -> RecordStatus {
        if self
            .scopes
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct")
        {
            RecordStatus::LoggedIn
        } else {
            RecordStatus::PlanNotAuthorized
        }
    }
}

/// No Debug, Display or Serialize: the URL includes ephemeral OAuth secrets.
/// Dropping the attempt cancels it. Keep it until `wait` completes.
pub struct LoginAttempt {
    pub id: String,
    url: String,
    cancel: CancellationToken,
    result: oneshot::Receiver<Result<RecordInfo, AuthError>>,
}
impl LoginAttempt {
    pub fn authorization_url(&self) -> &str {
        &self.url
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn wait(mut self) -> Result<RecordInfo, AuthError> {
        (&mut self.result)
            .await
            .map_err(|_| AuthError::from(ErrorKind::Cancelled))?
    }
}
impl Drop for LoginAttempt {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Never printable or generally serializable. Only explicit HTTP use is exposed.
pub struct AccessToken {
    secret: Secret,
    record_id: String,
    entry: Arc<Entry>,
    epoch: u64,
    recovered: bool,
}
impl AccessToken {
    pub fn authorization_header(&self) -> Result<reqwest::header::HeaderValue, AuthError> {
        if self.entry.blocked.load(Ordering::SeqCst)
            || self.epoch != self.entry.epoch.load(Ordering::SeqCst)
        {
            return Err(ErrorKind::NotLoggedIn.into());
        }
        let mut value =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.secret.0))
                .map_err(|_| AuthError::from(ErrorKind::InvalidResponse))?;
        value.set_sensitive(true);
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RecordInfo {
    pub id: String,
    pub issuer: String,
    pub subject: String,
    pub client_id: String,
    pub email: Option<String>,
    pub status: RecordStatus,
    #[serde(default)]
    pub background_consent: bool,
    #[serde(skip)]
    pub pending_revocation: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    LoggedIn,
    PlanNotAuthorized,
    NeedsLogin,
    SignedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutResult {
    Revoked,
    Pending,
    NoToken,
}

/// Safe diagnostics only: no response text, URLs, query values or error sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("ChatGPT credentials: {kind:?} (HTTP {status:?}, code {code:?})")]
pub struct AuthError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub code: Option<OAuthCode>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Storage,
    Network,
    InvalidResponse,
    NotLoggedIn,
    NeedsLogin,
    PlanNotAuthorized,
    ClientConfiguration,
    StateMismatch,
    AccessDenied,
    ClientMismatch,
    IdentityMismatch,
    InvalidIdToken,
    Cancelled,
    TimedOut,
    Unauthorized,
    RefreshTooEarly,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthCode {
    InvalidGrant,
    InvalidRefreshToken,
    TokenExpired,
    RefreshTokenExpired,
    RefreshTokenInvalidated,
    RefreshTokenReused,
    InvalidClient,
    AccessDenied,
}
impl From<ErrorKind> for AuthError {
    fn from(kind: ErrorKind) -> Self {
        Self {
            kind,
            status: None,
            code: None,
        }
    }
}

#[cfg(test)]
mod tests;
