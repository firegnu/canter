use super::retract_tests::Harness;
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

async fn open(test: &mut Harness, features: Value) {
    for c in "/doctor".chars() {
        test.key(KeyCode::Char(c));
    }
    test.key(KeyCode::Enter);
    test.ui.draw(0).unwrap();
    assert!(has(test, &["Doctor · checking"]), "{:#?}", rows(test));
    let info = test.request().await;
    assert_eq!(info.path, "/v1/info");
    info.json(json!({
        "features": features,
        "serverVersion": "test",
        "protocolVersion": 1,
        "sessions": []
    }));
}

fn check(id: &str, name: &str, category: &str, status: &str) -> Value {
    let mut check = json!({
        "id": id,
        "name": name,
        "category": category,
        "status": status,
    });
    if status != "ok" {
        check["reason"] = json!(format!("{name} needs attention"));
        if status == "fail" {
            check["fix"] = json!(format!("Fix {name}"));
        }
    }
    check
}

#[tokio::test]
async fn doctor_shows_grouped_results_after_the_background_check() {
    let mut test = Harness::new().await;
    open(&mut test, json!(["doctor"])).await;
    let request = test.request().await;
    assert_eq!(request.path, "/v1/doctor");
    test.ui.app_mut().handle_paste("still responsive");
    test.ui.draw(0).unwrap();
    assert!(has(&test, &["Doctor · checking"]), "{:#?}", rows(&test));
    request.json(json!({
        "data": [
            check("config", "config.json", "configuration", "ok"),
            check("version", "Client/server versions", "version", "ok"),
            check("provider.test", "Provider test", "provider", "fail"),
            check("mcp.test", "MCP test", "mcp", "skip"),
            check("sandbox", "Sandbox", "sandbox", "ok"),
        ],
        "serverVersion": "test",
        "serverProtocolVersion": 1
    }));
    test.message().await;
    let shown = rows(&test).join("\n");
    for expected in [
        "Doctor · 5 checks · 1 problem · 1 skipped",
        "Configuration",
        "Version",
        "Sign-in and models",
        "MCP",
        "Sandbox",
        "ok config.json",
        "fail Provider test",
        "Provider test needs attention",
        "Fix Provider test",
        "skip MCP test",
    ] {
        assert!(shown.contains(expected), "missing {expected}: {shown}");
    }
    // The results take the input box's place; the draft comes back after.
    test.key(KeyCode::Esc);
    test.ui.draw(0).unwrap();
    let shown = rows(&test).join("\n");
    assert!(shown.contains("still responsive"), "{shown}");
    test.stop().await;
}

#[tokio::test]
async fn doctor_ignores_a_result_that_arrives_after_escape() {
    let mut test = Harness::new().await;
    open(&mut test, json!(["doctor"])).await;
    let request = test.request().await;
    assert_eq!(request.path, "/v1/doctor");
    test.key(KeyCode::Esc);
    test.ui.draw(0).unwrap();
    assert!(!has(&test, &["Doctor ·"]), "{:#?}", rows(&test));
    request.json(json!({
        "data": [check("config", "config.json", "configuration", "ok")],
        "serverVersion": "test",
        "serverProtocolVersion": 1
    }));
    test.message().await;
    assert!(!has(&test, &["Doctor ·"]), "{:#?}", rows(&test));
    assert!(!has(&test, &["config.json"]), "{:#?}", rows(&test));
    test.stop().await;
}

#[tokio::test]
async fn doctor_tells_the_user_when_the_server_is_too_old() {
    let mut test = Harness::new().await;
    open(&mut test, json!([])).await;
    test.message().await;
    let shown = rows(&test).join("\n");
    // The text without the borders of the box it is in.
    let normalized = shown
        .split_whitespace()
        .filter(|word| *word != "│")
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        normalized
            .contains("This server is too old for /doctor; run canter serve --replace and retry"),
        "{shown}"
    );
    assert!(test.requests.try_recv().is_err(), "no /doctor request");
    test.stop().await;
}
