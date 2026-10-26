// Standalone Rust fixture, compiled by the integration test; no user configuration is read.
use std::io::{self, BufRead, Write};
fn main() {
    assert_eq!(std::env::args().nth(1).as_deref(), Some("--fixture"));
    if let Ok(path) = std::env::var("PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    if let Ok(path) = std::env::var("ENV_FILE") {
        let environment = std::env::vars()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, environment).unwrap();
    }
    if let Ok(path) = std::env::var("CHILD_PID_FILE") {
        let child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(path, child.id().to_string()).unwrap();
    }
    eprintln!("{}", std::env::var("MCP_TEST_KEY").unwrap_or_default());
    if std::env::var_os("HANG_INITIALIZE").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(600));
    }
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let Some(tail) = line.split("\"id\":").nth(1) else {
            continue;
        };
        let id: String = tail
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if id.is_empty() {
            continue;
        }
        let result = if line.contains("\"initialize\"") {
            r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fixture","version":"1"}}"#
        } else if line.contains("\"tools/list\"") {
            r#"{"tools":[{"name":"echo","description":"stdio echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}"#
        } else if line.contains("\"tools/call\"") {
            if std::env::var_os("RETURN_HOME_PATH").is_some() {
                r#"{"content":[{"type":"text","text":"/mcp/test-home/project/file.txt"}]}"#
            } else {
                r#"{"content":[{"type":"text","text":"stdio result"}]}"#
            }
        } else {
            "{}"
        };
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}");
        io::stdout().flush().unwrap();
    }
}
