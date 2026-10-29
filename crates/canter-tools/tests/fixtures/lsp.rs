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
    let mut diagnostic_results = std::collections::BTreeMap::<String, String>::new();
    let mut pending_diagnostic: Option<(String, String)> = None;
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
            if mode == "slow_sync" { std::thread::sleep(Duration::from_millis(250)); }
            if mode.contains("pull") {
                result(id.unwrap(), r#"{"capabilities":{"textDocumentSync":{"openClose":true,"change":1,"save":true},"diagnosticProvider":{"identifier":"fixture","interFileDependencies":false,"workspaceDiagnostics":false}}}"#);
            } else if mode.contains("sync") {
                result(id.unwrap(), r#"{"capabilities":{"textDocumentSync":{"openClose":true,"change":2,"save":{"includeText":true}}},"serverInfo":{"name":"fake"}}"#);
            } else { result(id.unwrap(), r#"{"capabilities":{"textDocumentSync":2},"serverInfo":{"name":"fake"}}"#); }
        } else if body.contains("\"method\":\"textDocument/didOpen\"") || body.contains("\"method\":\"textDocument/didChange\"") {
            if mode.contains("sync") {
                if mode.contains("late") { while !std::path::Path::new(&format!("{directory}/release-diagnostics")).exists() { std::thread::sleep(Duration::from_millis(5)); } }
                if mode.contains("edge") { std::thread::sleep(Duration::from_millis(500)); }
                let uri = scalar(&body, "uri");
                let version = scalar(&body, "version");
                let custom = std::fs::read_to_string(format!("{directory}/diagnostics.json")).ok();
                let diagnostics = if body.contains("bad") { custom.as_deref().unwrap_or(r#"[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"severity":1,"message":"fixture error"}]"#) } else { "[]" };
                diagnostic_results.insert(uri.into(), diagnostics.into());
                if !mode.contains("pull") || mode.contains("both") {
                    let version = if mode.contains("unversioned") { String::new() } else { format!("\"version\":{version},") };
                    send(&format!(r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":{uri},{version}"diagnostics":{diagnostics}}}}}"#));
                }
            }
        } else if body.contains("\"method\":\"textDocument/diagnostic\"") {
            let diagnostics = diagnostic_results.get(scalar(&body, "uri")).map(String::as_str).unwrap_or("[]");
            if mode.contains("delay") && pending_diagnostic.is_none() {
                pending_diagnostic = Some((id.unwrap().into(), diagnostics.into()));
                continue;
            }
            if let Some((old_id, old_diagnostics)) = pending_diagnostic.take() {
                result(&old_id, &format!(r#"{{"kind":"full","items":{old_diagnostics}}}"#));
            }
            if body.contains("\"previousResultId\"") {
                result(id.unwrap(), r#"{"kind":"unchanged","resultId":"fixture-result"}"#);
            } else { result(id.unwrap(), &format!(r#"{{"kind":"full","resultId":"fixture-result","items":{diagnostics}}}"#)); }
        } else if body.contains("\"method\":\"fixture/refresh\"") {
            send(r#"{"jsonrpc":"2.0","id":"refresh","method":"workspace/diagnostic/refresh","params":null}"#);
            result(id.unwrap(), "null");
        } else if body.contains("\"method\":\"fixture/publish\"") {
            let params = body.split_once("\"params\":").unwrap().1;
            send(&format!(r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{}}}"#, &params[..params.len()-1]));
            result(id.unwrap(), "null");
        } else if body.contains("\"method\":\"initialized\"") {
            if mode.contains("registered") {
                send(r#"{"jsonrpc":"2.0","id":"registration","method":"client/registerCapability","params":{"registrations":[{"id":"fixture-watch","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[{"globPattern":"**/*.{cfg,ini}","kind":7}]}}]}}"#);
            }
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

// Read one JSON scalar from the deterministic client frames used by this fixture.
fn scalar<'a>(body: &'a str, key: &str) -> &'a str {
    let rest = body.split_once(&format!("\"{key}\":")).unwrap().1.trim_start();
    if rest.starts_with('"') {
        let mut escape = false;
        for (i, byte) in rest.bytes().enumerate().skip(1) {
            if byte == b'"' && !escape { return &rest[..=i]; }
            escape = byte == b'\\' && !escape;
        }
    }
    rest.split([',', '}']).next().unwrap()
}
