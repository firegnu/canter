//! `/lsp` against a local HTTP server (P2-135): the list of language
//! servers with their states, a server's details with stop and retry, and
//! what an old server or a session without servers says.

use super::retract_tests::Harness;
use axum::response::IntoResponse;
use crossterm::event::KeyCode;
use serde_json::{Value, json};

fn rows(test: &Harness) -> Vec<String> {
    let buffer = test.ui.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn has(test: &Harness, parts: &[&str]) -> bool {
    rows(test)
        .iter()
        .any(|row| parts.iter().all(|part| row.contains(part)))
}

fn server(
    id: &str,
    name: &str,
    state: &str,
    rss: Option<u64>,
    shared: u32,
    reason: Option<&str>,
) -> Value {
    let mut server = json!({
        "id": id, "name": name, "root": "/srv/proj", "state": state,
        "sharedReferences": shared, "updatedAt": 1,
    });
    if let Some(rss) = rss {
        server["rssBytes"] = json!(rss);
    }
    if let Some(reason) = reason {
        server["reason"] = json!(reason);
    }
    server
}

async fn open(test: &mut Harness, features: Value) {
    for c in "/lsp".chars() {
        test.key(KeyCode::Char(c));
    }
    test.key(KeyCode::Enter);
    let info = test.request().await;
    assert_eq!(info.path, "/v1/info");
    info.json(
        json!({"features":features,"serverVersion":"test","protocolVersion":1,"sessions":[]}),
    );
}

#[tokio::test]
async fn lsp_lists_servers_and_a_servers_details_stop_and_retry() {
    let mut test = Harness::new().await;
    open(&mut test, json!(["lsp"])).await;
    let list = test.request().await;
    assert_eq!(list.path, "/v1/sessions/test/languageServers");
    list.json(json!({"data":[
        server("lsp-1", "rust-analyzer", "running", Some(312 * 1024 * 1024), 1, None),
        server("lsp-2", "pyright", "unavailable", None, 2, Some("pyright-langserver is not installed")),
    ]}));
    test.message().await;
    let shown = rows(&test).join("\n");
    assert!(has(&test, &[" LSP  2 servers"]), "{shown}");
    assert!(
        has(
            &test,
            &[
                "● rust-analyzer",
                "/srv/proj",
                "running · 312 MB · 1 session"
            ]
        ),
        "{shown}"
    );
    assert!(
        has(
            &test,
            &["● pyright", "/srv/proj", "unavailable · 2 sessions"]
        ),
        "{shown}"
    );
    assert!(
        has(&test, &["    pyright-langserver is not installed"]),
        "{shown}"
    );
    assert!(has(&test, &[" Enter details · Esc close"]), "{shown}");

    test.key(KeyCode::Enter);
    test.ui.draw(0).unwrap();
    let shown = rows(&test).join("\n");
    assert!(
        has(&test, &["rust-analyzer", "running · 312 MB · 1 session"]),
        "{shown}"
    );
    assert!(!has(&test, &["pyright"]), "{shown}");
    assert!(
        has(
            &test,
            &[" Stop applies to this session only; other sessions keep their own."]
        ),
        "{shown}"
    );
    assert!(
        has(&test, &[" s stop · r retry · Backspace list"]),
        "{shown}"
    );

    test.key(KeyCode::Char('s'));
    let stop = test.request().await;
    assert_eq!(stop.path, "/v1/sessions/test/languageServers/lsp-1/stop");
    let failed = (
        axum::http::StatusCode::CONFLICT,
        axum::Json(json!({"code":"conflict","message":"the server went away"})),
    )
        .into_response();
    assert!(stop.reply.send(failed).is_ok());
    test.message().await;
    let shown = rows(&test).join("\n");
    assert!(has(&test, &["the server went away"]), "{shown}");
    assert!(
        has(&test, &[" s stop · r retry · Backspace list"]),
        "{shown}"
    );

    test.key(KeyCode::Char('r'));
    let retry = test.request().await;
    assert_eq!(retry.path, "/v1/sessions/test/languageServers/lsp-1/retry");
    retry.json(json!({"data":[
        server("lsp-1", "rust-analyzer", "starting", None, 1, None),
        server("lsp-2", "pyright", "unavailable", None, 2, Some("pyright-langserver is not installed")),
    ]}));
    test.message().await;
    let shown = rows(&test).join("\n");
    assert!(
        has(&test, &["rust-analyzer", "starting · 1 session"]),
        "{shown}"
    );
    assert!(!has(&test, &["the server went away"]), "{shown}");

    test.key(KeyCode::Backspace);
    test.ui.draw(0).unwrap();
    let shown = rows(&test).join("\n");
    assert!(has(&test, &["● pyright"]), "{shown}");
    assert!(has(&test, &[" Enter details · Esc close"]), "{shown}");
    test.stop().await;
}

#[tokio::test]
async fn lsp_says_when_the_server_is_too_old_or_has_no_servers() {
    let mut test = Harness::new().await;
    open(&mut test, json!(["goals"])).await;
    test.message().await;
    let shown = rows(&test).join("\n");
    assert!(
        has(
            &test,
            &[" This canter server does not support LSP. Run canter serve --replace."]
        ),
        "{shown}"
    );

    test.key(KeyCode::Esc);
    open(&mut test, json!(["lsp"])).await;
    let list = test.request().await;
    assert_eq!(list.path, "/v1/sessions/test/languageServers");
    list.json(json!({"data":[]}));
    test.message().await;
    let shown = rows(&test).join("\n");
    assert!(has(&test, &[" LSP  0 servers"]), "{shown}");
    assert!(
        has(&test, &[" No language servers for this session."]),
        "{shown}"
    );
    assert!(has(&test, &[" LSP is off by default."]), "{shown}");
    assert!(shown.contains("lsp.enabled"), "{shown}");
    test.stop().await;
}
