#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    RateLimited,
    Server,
    Network,
    Timeout,
    ContextLength,
    Authentication,
    InvalidRequest,
    Other,
}

/// Safe diagnostics only: never retains response bodies, URLs, headers or sources.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("model request failed: {kind:?} (HTTP status: {status:?}); retried {retries} times")]
pub struct ModelError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub retry_after: Option<std::time::Duration>,
    pub retries: u32,
}

impl ModelError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::RateLimited | ErrorKind::Server | ErrorKind::Network | ErrorKind::Timeout
        )
    }

    pub(crate) fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            retries: 0,
        }
    }
}
