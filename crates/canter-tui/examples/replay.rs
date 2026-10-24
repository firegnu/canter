//! Watch a scripted session in a real terminal:
//!
//!     cargo run -p canter-tui --example replay
//!
//! Starts the fake server on a temporary socket, then the conversation screen.
//! Once the replay ends, anything you send gets a canned reply. Ctrl+C quits.

use canter_tui::fake::{DEMO_SCRIPT, FakeServer, Pace, parse_script};
use canter_tui::{RunOptions, run};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("canter.sock");
    let script = parse_script(DEMO_SCRIPT)?;
    let session_id = script[0].session_id.clone();
    // The first two events (settings, idle) are the snapshot; the rest stream in.
    let server = FakeServer::start(&socket, script, 2, Pace::Demo).await?;
    let result = run(RunOptions { socket, session_id }).await;
    drop(server);
    Ok(result?)
}
