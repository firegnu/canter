//! Canter's HTTP command and SSE event types. No transport or agent runtime.
//!
//! Independently implemented, referencing the stable structure and field names
//! of Codex app-server v2 at rust-v0.163.0; no upstream code is copied.

mod event;
mod stats;
mod telemetry;
pub use stats::*;
pub use telemetry::*;
mod lsp;
mod types;
pub use lsp::*;

pub use event::*;
pub use types::*;

/// Supported inline image formats, identified from bytes rather than a suffix.
pub fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

pub const PROTOCOL_VERSION: u32 = 1;

mod reducer;
pub use reducer::*;

mod commands;
pub use commands::*;

mod export;
pub use export::export_protocol;
