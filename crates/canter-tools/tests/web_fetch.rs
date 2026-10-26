mod common;
use canter_protocol::{Item, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use common::allow;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
}

async fn assert_fetch_text(content_type: &str, body: &[u8], expected: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let serve = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
    };
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::WebFetch,
            "web",
            &json!({"url":url}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let mut sink = |_: &str| {};
    let (_, result) = tokio::join!(serve, tools.execute(allow(&call), &cancel, &mut sink));
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert_eq!(result.text, expected);
    let Item::WebFetch { result, .. } = result.item else {
        panic!("web fetch item expected")
    };
    assert_eq!(result, expected);
}

#[tokio::test]
async fn html_title_precedes_the_markdown_body() {
    assert_fetch_text(
        "text/html",
        b"<html><head><title>  Canter &amp;\n\t tools &#x4E2D;&#x6587; </title><style>hidden</style></head><body><h1>Welcome</h1><p>Body.</p></body></html>",
        "Title: Canter & tools 中文\n\n# Welcome\n\nBody.",
    )
    .await;
}

#[tokio::test]
async fn absent_or_empty_html_titles_add_no_line() {
    for head in ["", "<title></title>", "<title> \n\t&nbsp; </title>"] {
        let body = format!("<html><head>{head}</head><body><p>Body.</p></body></html>");
        assert_fetch_text("text/html", body.as_bytes(), "Body.").await;
    }
}

#[tokio::test]
async fn html_title_is_bounded_without_splitting_unicode() {
    let body = format!(
        "<html><head><title>{}</title></head><body><p>Body.</p></body></html>",
        "文".repeat(600)
    );
    let expected = format!("Title: {}\n\nBody.", "文".repeat(512));
    assert_fetch_text("text/html", body.as_bytes(), &expected).await;
}

#[tokio::test]
async fn html_preflight_fallback_keeps_the_raw_title() {
    let body = format!(
        "<html><head><title>Raw page</title></head><body>{}Body.{} </body></html>",
        "<div>".repeat(257),
        "</div>".repeat(257)
    );
    let expected = format!(
        "[HTML not converted: HTML nesting limit exceeded (256); showing raw HTML.]\n\n{body}"
    );
    assert_fetch_text("text/html", body.as_bytes(), &expected).await;
}

#[tokio::test]
async fn charset_gbk_from_content_type() {
    assert_fetch_text(
        "text/html; charset=GBK",
        b"<html><body><p>\xd6\xd0\xce\xc4</p></body></html>",
        "中文",
    )
    .await;
}

#[tokio::test]
async fn charset_gbk_from_meta() {
    assert_fetch_text(
        "text/html",
        b"<html><head><meta charset=gb2312></head><body><p>\xd6\xd0\xce\xc4</p></body></html>",
        "中文",
    )
    .await;
}

#[tokio::test]
async fn charset_shift_jis_from_content_type() {
    assert_fetch_text(
        "text/html; charset=Shift_JIS",
        b"<html><body><p>\x93\xfa\x96\x7b\x8c\xea</p></body></html>",
        "日本語",
    )
    .await;
}

#[tokio::test]
async fn charset_utf16_bom_takes_priority() {
    let mut body = vec![0xff, 0xfe];
    for unit in
        "<html><head><meta charset=Shift_JIS></head><body><p>中文</p></body></html>".encode_utf16()
    {
        body.extend_from_slice(&unit.to_le_bytes());
    }
    assert_fetch_text("text/html; charset=GBK", &body, "中文").await;
}

#[tokio::test]
async fn charset_content_type_takes_priority_over_meta() {
    assert_fetch_text(
        "text/html; charset=GBK",
        b"<html><head><meta charset=Shift_JIS></head><body><p>\xd6\xd0\xce\xc4</p></body></html>",
        "中文",
    )
    .await;
}

#[tokio::test]
async fn charset_unknown_is_reported_and_decoded_as_utf8() {
    assert_fetch_text(
        "text/html; charset=unknown-encoding",
        "<html><body><p>中文</p></body></html>".as_bytes(),
        "[Unknown charset: unknown-encoding; decoded as UTF-8.]\n\n中文",
    )
    .await;
}

#[tokio::test]
async fn charset_undeclared_utf8_is_unchanged() {
    assert_fetch_text(
        "text/html",
        "<html><body><p>中文</p></body></html>".as_bytes(),
        "中文",
    )
    .await;
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
