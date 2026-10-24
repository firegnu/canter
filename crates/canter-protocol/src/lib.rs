//! Canter's HTTP command and SSE event types. No transport or agent runtime.
//!
//! Independently implemented, referencing the stable structure and field names
//! of Codex app-server v2 at rust-v0.163.0; no upstream code is copied.

mod event;
mod types;

pub use event::*;
pub use types::*;

pub const PROTOCOL_VERSION: u32 = 1;

mod reducer;
pub use reducer::*;

mod commands;
pub use commands::*;

mod export;
pub use export::export_protocol;
