//! Conversation sessions independent of HTTP and terminal clients.
mod btw;
mod compaction;
mod context;
mod images;
mod journal;
mod runner;
mod session;
mod skills;
pub mod usage;

pub use btw::BtwStream;
use canter_protocol::ErrorCode;
pub use journal::Subscription;
pub use session::Session;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default)]
pub struct RuntimeContext {
    pub environment: BTreeMap<String, String>,
    pub protected_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct CoreOptions {
    pub context_window: u64,
    pub max_output_tokens: u32,
    pub image_models: Vec<String>,
    pub prices: usage::PriceBook,
    /// Clock at the model-request boundary; replaceable for deterministic replay tests.
    pub request_time: fn() -> i64,
}
impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            context_window: 1_048_576,
            max_output_tokens: 65_536,
            image_models: vec!["deepseek-flash".into()],
            prices: usage::builtin_prices(),
            request_time: journal::now,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
}
impl CoreError {
    pub(crate) fn new(code: ErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn internal() -> Self {
        Self::new(ErrorCode::Internal, "session operation failed")
    }
}
impl From<canter_store::StoreError> for CoreError {
    fn from(error: canter_store::StoreError) -> Self {
        use canter_store::StoreError;
        match error {
            StoreError::InvalidId => Self::new(ErrorCode::InvalidRequest, "invalid session id"),
            StoreError::Locked => Self::new(ErrorCode::Conflict, "session already open"),
            StoreError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::new(ErrorCode::SessionNotFound, "session not found")
            }
            StoreError::Io(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::new(ErrorCode::Conflict, "session already exists")
            }
            _ => Self::internal(),
        }
    }
}
