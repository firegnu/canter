//! Server-owned LSP lifecycle. No tools, document synchronization or protocol events.
mod config;
pub use config::{LspConfig, ServerConfig};
mod memory;
mod service;
mod transport;
pub use service::{FileContext, Instance, LspService, Round};
#[cfg(test)]
mod test_support;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Unavailable,
    Cancelled,
    Timeout,
    Protocol,
    ResourceLimited,
    Stopped,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspError {
    pub kind: ErrorKind,
    pub message: String,
}
impl LspError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for LspError {}
