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

impl ErrorKind {
    /// For people, in the middle of a sentence: `server error`.
    fn words(self) -> &'static str {
        match self {
            Self::RateLimited => "rate limited",
            Self::Server => "server error",
            Self::Network => "network error",
            Self::Timeout => "timed out",
            Self::ContextLength => "context too long",
            Self::Authentication => "authentication failed",
            Self::InvalidRequest => "invalid request",
            Self::Other => "unexpected error",
        }
    }
}

/// Safe diagnostics only: never retains response bodies, URLs, headers or sources.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Model request failed: {}{}", self.retry_reason(), self.after_retries())]
pub struct ModelError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub retry_after: Option<std::time::Duration>,
    pub retries: u32,
}

impl ModelError {
    /// What went wrong, for people, in the middle of a sentence:
    /// `server error (HTTP 500)`, `network error`. The retry notice adds
    /// its own attempt counter.
    pub fn retry_reason(&self) -> String {
        match self.status {
            Some(status) => format!("{} (HTTP {status})", self.kind.words()),
            None => self.kind.words().to_owned(),
        }
    }

    fn after_retries(&self) -> String {
        match self.retries {
            0 => String::new(),
            1 => " after 1 retry".to_owned(),
            retries => format!(" after {retries} retries"),
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_words_with_the_status_when_there_is_one() {
        let error = |kind, status, retries| ModelError {
            kind,
            status,
            retry_after: None,
            retries,
        };
        let server = error(ErrorKind::Server, Some(500), 10);
        assert_eq!(server.retry_reason(), "server error (HTTP 500)");
        assert_eq!(
            server.to_string(),
            "Model request failed: server error (HTTP 500) after 10 retries"
        );
        let limited = error(ErrorKind::RateLimited, Some(429), 1);
        assert_eq!(
            limited.to_string(),
            "Model request failed: rate limited (HTTP 429) after 1 retry"
        );
        assert_eq!(
            error(ErrorKind::Network, None, 0).to_string(),
            "Model request failed: network error"
        );
        assert_eq!(
            error(ErrorKind::Timeout, None, 3).retry_reason(),
            "timed out"
        );
    }
}
