//! Provider-independent model history and streaming interface.

mod chat_completions;
mod error;
mod responses;
mod retry;
mod types;
mod wire;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
#[allow(dead_code)]
mod test_support;

pub use chat_completions::{ApiKey, ChatCompletions, RequestHeaders};
pub use error::{ErrorKind, ModelError};
pub use responses::{Responses, ResponsesAuth, ResponsesCredential};
pub use retry::{RetryPolicy, RetrySleep};
pub use types::*;

use futures_util::Stream;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub type ModelStream<'a> = Pin<Box<dyn Stream<Item = Result<ModelEvent, ModelError>> + Send + 'a>>;

/// Local request policy, never part of history, wire payloads or public events.
#[derive(Clone)]
pub struct ModelContext {
    pub foreground: tokio::sync::watch::Receiver<bool>,
    /// The credential owner resolves its local record once for this work scope.
    pub credential_binding: std::sync::Arc<tokio::sync::OnceCell<String>>,
}

pub trait Model: Send + Sync {
    /// Poll to make progress. Dropping the stream drops the in-flight request.
    /// Cancellation ends the stream without further events (including errors).
    fn stream(&self, request: Request, cancellation: CancellationToken) -> ModelStream<'_>;

    fn stream_with_context(
        &self,
        request: Request,
        cancellation: CancellationToken,
        _context: ModelContext,
    ) -> ModelStream<'_> {
        self.stream(request, cancellation)
    }
}
