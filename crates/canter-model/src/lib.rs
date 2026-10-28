//! Provider-independent model history and streaming interface.

mod chat_completions;
mod error;
mod retry;
mod types;
mod wire;

pub use chat_completions::{ApiKey, ChatCompletions};
pub use error::{ErrorKind, ModelError};
pub use retry::{RetryPolicy, RetrySleep};
pub use types::*;

use futures_util::Stream;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub type ModelStream<'a> = Pin<Box<dyn Stream<Item = Result<ModelEvent, ModelError>> + Send + 'a>>;

pub trait Model: Send + Sync {
    /// Poll to make progress. Dropping the stream drops the in-flight request.
    /// Cancellation ends the stream without further events (including errors).
    fn stream(&self, request: Request, cancellation: CancellationToken) -> ModelStream<'_>;
}
