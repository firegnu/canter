use canter_tools::{Risk, Tool};
mod common;
use canter_tools::{LocalExecutor, ToolOutcome, Tools, web_search::WebSearch};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

struct FixtureSearch;
#[async_trait::async_trait]
impl SearchProvider for FixtureSearch {
    async fn search(
        &self,
        query: &str,
        count: u32,
    ) -> Result<Vec<canter_tools::web_search::WebSearchResult>, String> {
        assert_eq!(query, "Rust 文档");
        assert_eq!(count, 10);
        Ok(vec![canter_tools::web_search::WebSearchResult {
            title: "Rust book".into(),
            url: "https://doc.rust-lang.org/book/".into(),
            date: Some("2026-10-01T00:00:00Z".into()),
            summary: "Learn Rust".into(),
        }])
    }
}

fn tools(root: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
    .with_web_search(Some(WebSearch::new(Arc::new(FixtureSearch))))
}

#[tokio::test]
async fn search_executes_through_the_tool_boundary_with_default_count() {
    let root = tempfile::tempdir().unwrap();
    let tools = tools(&root);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::from_name("web_search").unwrap(),
            "s1",
            r#"{"query":"Rust 文档"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let output = tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await;
    assert_eq!(output.outcome, ToolOutcome::Completed);
    for text in [
        "untrusted",
        "Rust book",
        "https://doc.rust-lang.org/book/",
        "2026-10-01",
        "Learn Rust",
    ] {
        assert!(output.text.contains(text), "{text}");
    }
    assert_eq!(
        serde_json::to_value(output.item).unwrap()["results"][0]["title"],
        "Rust book"
    );
}
use canter_tools::web_search::{SearchProvider, deepseek::DeepSeek};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn search_validates_unicode_length_and_count_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let tools = tools(&root);
    let cancel = CancellationToken::new();
    for args in [
        json!({"query":""}),
        json!({"query":" \n\t"}),
        json!({"query":"字".repeat(129)}),
        json!({"query":"Rust","count":0}),
        json!({"query":"Rust","count":21}),
        json!({"query":"Rust","count":1.5}),
    ] {
        let error = tools
            .prepare(Tool::WebSearch, "s", &args.to_string(), &cancel)
            .await
            .err()
            .expect("invalid search arguments");
        assert_eq!(error.outcome, ToolOutcome::Failed);
    }
    for count in [1, 20] {
        assert!(
            tools
                .prepare(
                    Tool::WebSearch,
                    "s",
                    &json!({"query":"字".repeat(128),"count":count}).to_string(),
                    &cancel
                )
                .await
                .is_ok()
        );
    }
    let disabled = Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    assert_eq!(disabled.find("web_search"), None);
    assert!(
        disabled
            .prepare(Tool::WebSearch, "s", r#"{"query":"Rust"}"#, &cancel)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn search_approval_grants_all_queries_but_review_stays_read_only_and_plan_asks() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    let root = tempfile::tempdir().unwrap();
    let tools = tools(&root);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::WebSearch, "s", r#"{"query":"Rust docs"}"#, &cancel)
        .await
        .unwrap();
    let next = tools
        .prepare(
            Tool::WebSearch,
            "next",
            r#"{"query":"Other docs"}"#,
            &cancel,
        )
        .await
        .unwrap();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        let mut gate = PermissionGate::default();
        for check in [gate.evaluate(&call, mode), gate.evaluate_plan(&call, mode)] {
            if mode == PermissionMode::AllowAll {
                assert!(matches!(check, PermissionCheck::Execute(_)));
            } else {
                let PermissionCheck::Ask(request) = check else {
                    panic!("search approval required");
                };
                assert_eq!(request.tool, "web_search");
                assert_eq!(request.arguments["query"], "Rust docs");
                assert!(
                    request
                        .available_decisions
                        .contains(&ApprovalDecision::AcceptForSession)
                );
            }
        }
        assert!(tools.allowed_in_plan_mode("web_search", call.raw_arguments()));
        gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
            .unwrap();
        assert!(matches!(
            gate.evaluate(&next, mode),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            gate.evaluate_plan(&next, mode),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            gate.evaluate_review(&next, PermissionMode::AlwaysAsk),
            PermissionCheck::Deny {
                cancel_turn: false,
                ..
            }
        ));
        assert!(matches!(
            PermissionGate::default().evaluate(&next, PermissionMode::AlwaysAsk),
            PermissionCheck::Ask(_)
        ));
    }
}

#[tokio::test]
async fn deepseek_posts_the_query_and_normalizes_search_results() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/tools/search", listener.local_addr().unwrap());
    let provider = DeepSeek::new("test-only-search-key".into(), endpoint.parse().unwrap());
    let serve = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(socket.read_u8().await.unwrap());
        }
        let headers = String::from_utf8(headers).unwrap().to_lowercase();
        assert!(headers.starts_with("post /v1/tools/search http/1.1\r\n"));
        assert!(headers.contains("authorization: bearer test-only-search-key\r\n"));
        assert!(headers.contains("content-type: application/json\r\n"));
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"query":"Rust site:doc.rust-lang.org","count":10})
        );
        let body = json!({"results":[{"title":"Rust book","url":"https://doc.rust-lang.org/book/","date":null,"summary":"Learn Rust","snippet":"short","authority":4,"rerank_score":2.8}]}).to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    };
    // A failed adapter must report its error immediately, without waiting on the fixture.
    let run = async {
        let results = provider
            .search("Rust site:doc.rust-lang.org", 10)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(results).unwrap(),
            json!([{"title":"Rust book","url":"https://doc.rust-lang.org/book/","summary":"Learn Rust"}])
        );
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(serve, run);
    })
    .await
    .unwrap();
}

#[test]
fn web_search_is_an_execution_tool_with_provider_independent_parameters() {
    let tool = Tool::from_name("web_search").expect("built-in web_search");
    let definition = tool.definition();
    assert_eq!(definition.risk, Risk::Execute);
    assert_eq!(
        definition.parameters["required"],
        serde_json::json!(["query"])
    );
    assert_eq!(
        definition.parameters["properties"]["query"]["maxLength"],
        128
    );
    assert_eq!(definition.parameters["properties"]["count"]["default"], 10);
    assert_eq!(definition.parameters["properties"]["count"]["maximum"], 20);
    for text in [
        "web_fetch",
        "site:",
        "untrusted",
        "secrets",
        "code",
        "personal data",
    ] {
        assert!(definition.description.contains(text), "{text}");
    }
}
