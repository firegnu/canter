//! `canter-eval`: the deterministic parts of a canter eval round.
//!
//! The orchestrator starts the eval server and runs corral; this crate prepares
//! a run's sessions, finds each session, steers it after counted commits,
//! records its end, grades it, records stats, stops the eval server, cleans up
//! and writes the round's result table. See `eval/README.md` for the whole
//! procedure.

pub mod collect;
pub mod config;
pub mod finish;
pub mod grade;
pub mod memory;
pub mod paths;
pub mod prepare;
pub mod report;
pub mod server;
pub mod session_data;
pub mod steer;
pub mod task;

/// Errors here are single lines meant for the terminal; they never include
/// credentials or configuration contents.
pub type Result<T> = std::result::Result<T, String>;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
