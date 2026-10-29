// Standalone Rust JSON-RPC replay fixture. Compiled by tests, never reads user state.
use std::{fs::OpenOptions, io::{self, BufRead, Read, Write}, time::Duration};

fn send(body: &str) {
    let mut out = io::stdout().lock();
    // Small writes also exercise partial pipe reads and UTF-8 byte lengths.
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    for chunk in frame.as_bytes().chunks(7) { out.write_all(chunk).unwrap(); }
    out.flush().unwrap();
}
fn result(id: &str, value: &str) { send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{value}}}"#)); }
fn main() {
    let directory = std::env::var("LSP_TEST_DIR").unwrap();
    let pid = std::process::id();
    let mode = std::env::var("LSP_TEST_MODE").unwrap_or_default();
    let mut log = OpenOptions::new().create(true).append(true).open(format!("{directory}/{pid}.log")).unwrap();
    writeln!(log, "ARGS {:?}", std::env::args().collect::<Vec<_>>()).unwrap();
    let mut starts = OpenOptions::new().create(true).append(true).open(format!("{directory}/starts")).unwrap();
    writeln!(starts, "{pid}").unwrap();
    if mode == "fail" { std::process::exit(1); }
    if mode == "child" {
        let child = std::process::Command::new("/bin/sleep").arg("600").stdin(std::process::Stdio::null()).spawn().unwrap();
        writeln!(log, "CHILD {}", child.id()).unwrap();
    }
    let mut input = io::stdin().lock();
    let mut delayed = None;
    loop {
        let mut length = 0;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap() == 0 { return; }
            if line == "\r\n" { break; }
            if let Some(value) = line.strip_prefix("Content-Length:") { length = value.trim().parse().unwrap(); }
        }
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        writeln!(log, "{body}").unwrap();
        let id = body.split("\"id\":").nth(1).map(|s| s.split([',', '}']).next().unwrap().trim());
        if body.contains("\"method\":\"initialize\"") {
            if mode == "hang_init" { std::thread::sleep(Duration::from_secs(600)); }
            result(id.unwrap(), r#"{"capabilities":{"textDocumentSync":2},"serverInfo":{"name":"fake"}}"#);
        } else if body.contains("\"method\":\"initialized\"") {
            send(r#"{"jsonrpc":"2.0","id":"config","method":"workspace/configuration","params":{"items":[{"section":"test.answer"},{"section":"missing"},{}]}}"#);
            send(r#"{"jsonrpc":"2.0","id":700,"method":"workspace/applyEdit","params":{"edit":{}}}"#);
            send(r#"{"jsonrpc":"2.0","id":701,"method":"workspace/executeCommand","params":{"command":"do-not-execute"}}"#);
        } else if body.contains("\"method\":\"shutdown\"") {
            if mode != "hang_shutdown" { result(id.unwrap(), "null"); }
        } else if body.contains("\"method\":\"exit\"") {
            return;
        } else if body.contains("\"method\":\"fixture/crash\"") {
            std::process::exit(2);
        } else if body.contains("\"method\":\"fixture/wait\"") {
            // Only a cancellation notification will follow.
        } else if body.contains("\"method\":\"fixture/error\"") {
            send(&format!(r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32603,"message":"test-private-environment-value"}}}}"#, id.unwrap()));
        } else if body.contains("\"method\":\"fixture/first\"") {
            delayed = id.map(str::to_owned);
        } else if body.contains("\"method\":\"fixture/second\"") {
            result(id.unwrap(), "2");
            result(&delayed.take().unwrap(), "1");
        } else if body.contains("\"method\":\"fixture/echo\"") {
            result(id.unwrap(), r#""你好""#);
        }
    }
}
