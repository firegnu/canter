use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use tokio::{net::TcpListener, task::JoinHandle};

pub const KEY: &str = "test-only-credential-never-real";

pub struct Reply {
    pub status: StatusCode,
    pub body: String,
    pub retry_after: Option<String>,
    pub hang: bool,
}

impl Reply {
    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: StatusCode::OK,
            body: body.into(),
            retry_after: None,
            hang: false,
        }
    }
}

#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    arrivals: Arc<Mutex<Vec<Instant>>>,
}

pub struct Server {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<Value>>>,
    pub arrivals: Arc<Mutex<Vec<Instant>>>,
    task: JoinHandle<()>,
}

impl Server {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let script = Script {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::default(),
            arrivals: Arc::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let requests = script.requests.clone();
        let arrivals = script.arrivals.clone();
        let app = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(script);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url,
            requests,
            arrivals,
            task,
        }
    }

    pub async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(
    State(script): State<Script>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    // Compare without ever putting the credential in assertion diagnostics.
    assert!(
        headers
            .get("authorization")
            .is_some_and(|value| value == format!("Bearer {KEY}").as_str())
    );
    script.requests.lock().unwrap().push(body);
    script.arrivals.lock().unwrap().push(Instant::now());
    let reply = script.replies.lock().unwrap().pop_front().unwrap_or(Reply {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: String::new(),
        retry_after: None,
        hang: false,
    });
    let mut builder = Response::builder()
        .status(reply.status)
        .header("content-type", "text/event-stream");
    if let Some(delay) = reply.retry_after {
        builder = builder.header("retry-after", delay);
    }
    // Split at byte boundaries, including inside UTF-8 and SSE fields.
    let chunks: Vec<_> = reply
        .body
        .as_bytes()
        .chunks(7)
        .map(Bytes::copy_from_slice)
        .map(Ok::<_, Infallible>)
        .collect();
    let tail = stream::pending().take(if reply.hang { 1 } else { 0 });
    builder
        .body(Body::from_stream(stream::iter(chunks).chain(tail)))
        .unwrap()
}
