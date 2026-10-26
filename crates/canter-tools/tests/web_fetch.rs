use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
}

#[tokio::test]
async fn fetch_rejects_backslashes_whitespace_and_control_characters_before_approval() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    for url in [
        "http://127.0.0.1:1234\\@good.example/",
        "http://local\thost/",
        "http://localhost/\npath",
        "http://localhost/a b",
        "http://localhost/\u{a0}",
        "http://localhost/\u{7f}",
    ] {
        assert!(
            tools
                .prepare(
                    Tool::WebFetch,
                    "web",
                    &json!({"url":url}).to_string(),
                    &CancellationToken::new()
                )
                .await
                .is_err(),
            "accepted {url:?}"
        );
    }
}

#[tokio::test]
async fn normalized_fetch_url_is_visible_in_approval_and_project_items() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    for (url, actual) in [
        (
            "http://bücher.example/page",
            "http://xn--bcher-kva.example/page",
        ),
        ("HTTP://LOCALHOST:80", "http://localhost/"),
        ("http://2130706433/page", "http://127.0.0.1/page"),
    ] {
        let raw = json!({"url":url}).to_string();
        let call = tools
            .prepare(Tool::WebFetch, "web", &raw, &CancellationToken::new())
            .await
            .unwrap();
        let PermissionCheck::Ask(approval) =
            PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk)
        else {
            panic!("approval expected")
        };
        assert_eq!(approval.arguments["url"], url);
        assert_eq!(approval.arguments["resolvedUrl"], actual);
        let item = serde_json::to_value(call.started_item()).unwrap();
        assert_eq!(item["url"], url);
        assert_eq!(item["resolvedUrl"], actual);
        assert_eq!(call.raw_arguments(), raw);
    }
}
