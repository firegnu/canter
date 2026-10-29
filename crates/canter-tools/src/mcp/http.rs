//! Observe authentication failures without exposing SDK error bodies. Delegate
//! both SSE limits to rmcp, preserving the existing transport behavior.
use futures_util::stream::BoxStream;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::{
        auth::AuthClient,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Headers = HashMap<HeaderName, HeaderValue>;
type Error = StreamableHttpError<reqwest::Error>;
type Stream = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

#[derive(Clone)]
pub(super) struct Client {
    pub inner: AuthClient<reqwest::Client>,
    pub needs_login: Arc<AtomicBool>,
    pub use_oauth: bool,
}
impl Client {
    fn observe<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        if self.use_oauth
            && matches!(
                &result,
                Err(Error::AuthRequired(_) | Error::InsufficientScope(_) | Error::Auth(_))
            )
        {
            self.needs_login.store(true, Ordering::Relaxed);
        }
        result
    }
}
impl StreamableHttpClient for Client {
    type Error = reqwest::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<StreamableHttpPostResponse, Error> {
        self.observe(
            self.inner
                .post_message(uri, message, session_id, auth_header, custom_headers)
                .await,
        )
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, Error> {
        self.observe(
            self.inner
                .post_message_with_max_sse_event_size(
                    uri,
                    message,
                    session_id,
                    auth_header,
                    custom_headers,
                    max_sse_event_size,
                )
                .await,
        )
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<(), Error> {
        self.observe(
            self.inner
                .delete_session(uri, session_id, auth_header, custom_headers)
                .await,
        )
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<Stream, Error> {
        self.observe(
            self.inner
                .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
                .await,
        )
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<Stream, Error> {
        self.observe(
            self.inner
                .get_stream_with_max_sse_event_size(
                    uri,
                    session_id,
                    last_event_id,
                    auth_header,
                    custom_headers,
                    max_sse_event_size,
                )
                .await,
        )
    }
}
