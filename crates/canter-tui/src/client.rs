//! Client for the canter server: HTTP + SSE over a unix socket, following
//! `crates/canter-protocol/README.md`.
//!
//! The usual flow: [`Client::read_session`] for a snapshot, then
//! [`Client::subscribe`] from the snapshot's `lastEventSeq`, and feed each
//! event to `canter_protocol::apply_event_mut`. After a dropped stream,
//! [`Client::resume`] (a restarted server has not opened the session yet) and
//! subscribe again from [`EventStream::last_seq`].

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use canter_protocol::{
    ErrorCode, ErrorResponse, Event, RequestResolveParams, RequestResolveResponse,
    SessionCompactParams, SessionCompactResponse, SessionCreateParams, SessionCreateResponse,
    SessionInputParams, SessionInputResponse, SessionInterruptParams, SessionInterruptResponse,
    SessionListResponse, SessionReadResponse, SessionResumeParams, SessionResumeResponse,
    SessionShellCommandParams, SessionShellCommandResponse, SessionSummary, SessionUpdateParams,
    SessionUpdateResponse, routes,
};
use reqwest::header::ACCEPT;
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;

/// The host is ignored on a unix socket; it only makes the URL valid.
const ORIGIN: &str = "http://canter/";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with an `ErrorResponse`.
    #[error("{message} ({code:?}, HTTP {status})")]
    Server {
        status: StatusCode,
        code: ErrorCode,
        message: String,
    },
    #[error("unexpected response: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub async fn goal(
        &self,
        session: &str,
        command: &crate::GoalCommand,
    ) -> Result<canter_protocol::GoalResponse, ClientError> {
        use crate::GoalCommand;
        let endpoint = url(routes::GOAL, &[session]);
        let request = match command {
            GoalCommand::Read => self.http.get(endpoint),
            GoalCommand::Set(params) => self.http.post(endpoint).json(params),
            GoalCommand::Clear => self.http.delete(endpoint),
            GoalCommand::Pause => self.http.post(url(routes::GOAL_PAUSE, &[session])),
            GoalCommand::Resume => self.http.post(url(routes::GOAL_RESUME, &[session])),
        };
        decode(request.send().await?).await
    }
    pub async fn background_commands(
        &self,
        session: &str,
    ) -> Result<canter_protocol::BackgroundCommandListResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::BACKGROUND_COMMANDS, &[session]))
                .send()
                .await?,
        )
        .await
    }
    pub async fn background_output(
        &self,
        session: &str,
        id: u64,
    ) -> Result<canter_protocol::BackgroundCommandOutput, ClientError> {
        decode(
            self.http
                .get(url(routes::BACKGROUND_OUTPUT, &[session, &id.to_string()]))
                .send()
                .await?,
        )
        .await
    }
    pub async fn background_stop(
        &self,
        session: &str,
        id: Option<u64>,
    ) -> Result<canter_protocol::BackgroundCommandListResponse, ClientError> {
        let endpoint = match id {
            Some(id) => url(routes::BACKGROUND_STOP, &[session, &id.to_string()]),
            None => url(routes::BACKGROUND_STOP_ALL, &[session]),
        };
        decode(self.http.post(endpoint).send().await?).await
    }
    pub async fn language_servers(
        &self,
        session: &str,
    ) -> Result<canter_protocol::LanguageServerListResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::LANGUAGE_SERVERS, &[session]))
                .send()
                .await?,
        )
        .await
    }
    /// Stops (`retry: false`) or retries this session's subscription `server`.
    pub async fn language_server_control(
        &self,
        session: &str,
        server: &str,
        retry: bool,
    ) -> Result<canter_protocol::LanguageServerListResponse, ClientError> {
        let route = if retry {
            routes::LANGUAGE_SERVER_RETRY
        } else {
            routes::LANGUAGE_SERVER_STOP
        };
        decode(
            self.http
                .post(url(route, &[session, server]))
                .send()
                .await?,
        )
        .await
    }
    pub async fn shell_background(
        &self,
        session: &str,
        item_id: String,
    ) -> Result<canter_protocol::ShellBackgroundResponse, ClientError> {
        decode(
            self.http
                .post(url(routes::SHELL_BACKGROUND, &[session]))
                .json(&canter_protocol::ShellBackgroundParams { item_id })
                .send()
                .await?,
        )
        .await
    }
    pub async fn info(&self) -> Result<canter_protocol::InfoResponse, ClientError> {
        decode(self.http.get(url(routes::INFO, &[])).send().await?).await
    }

    pub async fn btw(&self, session: &str, question: String) -> Result<BtwStream, ClientError> {
        self.private_answer(
            session,
            routes::BTW,
            &canter_protocol::SessionBtwParams { question },
        )
        .await
    }

    pub async fn handoff(
        &self,
        session: &str,
        direction: String,
    ) -> Result<BtwStream, ClientError> {
        self.private_answer(
            session,
            routes::HANDOFF,
            &canter_protocol::SessionHandoffParams {
                direction: Some(direction),
            },
        )
        .await
    }

    async fn private_answer(
        &self,
        session: &str,
        route: &str,
        params: &impl serde::Serialize,
    ) -> Result<BtwStream, ClientError> {
        let response = self
            .http
            .post(url(route, &[session]))
            .json(params)
            .header(ACCEPT, "text/event-stream")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(server_error(status, &response.bytes().await?));
        }
        Ok(BtwStream {
            response,
            parser: SseParser::default(),
        })
    }

    pub async fn mcp(&self) -> Result<canter_protocol::McpListResponse, ClientError> {
        decode(self.http.get(url(routes::MCP, &[])).send().await?).await
    }

    pub async fn command_rules(
        &self,
    ) -> Result<canter_protocol::CommandRuleListResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::COMMAND_RULES, &[]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn delete_command_rule(
        &self,
        rule: &canter_protocol::CommandRule,
    ) -> Result<canter_protocol::CommandRuleDeleteResponse, ClientError> {
        decode(
            self.http
                .post(url(routes::COMMAND_RULE_DELETE, &[]))
                .json(rule)
                .send()
                .await?,
        )
        .await
    }

    pub async fn delete_file_rule(
        &self,
        rule: &canter_protocol::FileRule,
    ) -> Result<canter_protocol::CommandRuleDeleteResponse, ClientError> {
        decode(
            self.http
                .post(url(routes::FILE_RULE_DELETE, &[]))
                .json(rule)
                .send()
                .await?,
        )
        .await
    }

    pub async fn context(
        &self,
        session: &str,
    ) -> Result<canter_protocol::ContextResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::CONTEXT, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn sandbox(
        &self,
        session: &str,
    ) -> Result<canter_protocol::SessionSandboxResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::SANDBOX, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn usage(
        &self,
        session: Option<&str>,
    ) -> Result<canter_protocol::UsageResponse, ClientError> {
        let endpoint = match session {
            Some(id) => url(routes::SESSION_USAGE, &[id]),
            None => url(routes::USAGE, &[]),
        };
        decode(self.http.get(endpoint).send().await?).await
    }

    /// GET /v1/sessions/{id}/skills: the session's fixed skill list.
    pub async fn list_skills(
        &self,
        session: &str,
    ) -> Result<Vec<canter_protocol::Skill>, ClientError> {
        let response = self
            .http
            .get(url(routes::SKILLS, &[session]))
            .send()
            .await?;
        Ok(decode::<canter_protocol::SkillListResponse>(response)
            .await?
            .data)
    }

    /// GET /v1/models: the server's configured model list.
    pub async fn list_models(&self) -> Result<Vec<canter_protocol::Model>, ClientError> {
        let response = self.http.get(url(routes::MODELS, &[])).send().await?;
        Ok(decode::<canter_protocol::ModelListResponse>(response)
            .await?
            .data)
    }

    /// Builds a client for the server listening on `socket`. Does not connect yet.
    pub fn new(socket: &Path) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .unix_socket(socket.to_path_buf())
            .build()?;
        Ok(Self { http })
    }

    /// GET /v1/sessions: every session, most recently updated first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, ClientError> {
        let response = self.http.get(url(routes::SESSIONS, &[])).send().await?;
        Ok(decode::<SessionListResponse>(response).await?.data)
    }

    /// POST /v1/sessions: a new session. The body carries the environment,
    /// so it is never logged.
    pub async fn create_session(
        &self,
        params: &SessionCreateParams,
    ) -> Result<SessionCreateResponse, ClientError> {
        let response = self
            .http
            .post(url(routes::SESSIONS, &[]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    /// GET /v1/sessions/{id}: the session's full snapshot and its `lastEventSeq`.
    pub async fn read_session(&self, session: &str) -> Result<SessionReadResponse, ClientError> {
        let response = self
            .http
            .get(url(routes::SESSION, &[session]))
            .send()
            .await?;
        decode(response).await
    }

    /// POST /v1/sessions/{id}/resume: opens the session with this terminal's
    /// environment (kept if it is already open) and returns its snapshot.
    pub async fn resume(
        &self,
        session: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<SessionResumeResponse, ClientError> {
        let params = SessionResumeParams { env: env.clone() };
        let response = self
            .http
            .post(url(routes::RESUME, &[session]))
            .json(&params)
            .send()
            .await?;
        decode(response).await
    }

    /// PATCH /v1/sessions/{id}: change the name, model or permission mode.
    pub async fn update(
        &self,
        session: &str,
        params: &SessionUpdateParams,
    ) -> Result<SessionUpdateResponse, ClientError> {
        let response = self
            .http
            .patch(url(routes::SESSION, &[session]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    /// POST /v1/sessions/{id}/input: send what the user typed.
    pub async fn send_input(
        &self,
        session: &str,
        params: &SessionInputParams,
    ) -> Result<SessionInputResponse, ClientError> {
        let response = self
            .http
            .post(url(routes::INPUT, &[session]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    pub async fn shell_command(
        &self,
        session: &str,
        params: &SessionShellCommandParams,
    ) -> Result<SessionShellCommandResponse, ClientError> {
        let response = self
            .http
            .post(url(routes::SHELL_COMMAND, &[session]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    /// POST /v1/sessions/{id}/interrupt: stop the turn. Answers once the turn
    /// has ended.
    pub async fn interrupt(
        &self,
        session: &str,
        turn_id: &str,
        retract: bool,
    ) -> Result<SessionInterruptResponse, ClientError> {
        let params = SessionInterruptParams {
            retract: retract.then_some(true),
            turn_id: turn_id.to_owned(),
        };
        let response = self
            .http
            .post(url(routes::INTERRUPT, &[session]))
            .json(&params)
            .send()
            .await?;
        decode(response).await
    }

    /// POST /v1/sessions/{id}/compact: compact the session's context in a
    /// turn of its own. Answers once that turn has started; the server
    /// refuses while another turn runs.
    pub async fn compact(&self, session: &str) -> Result<SessionCompactResponse, ClientError> {
        let response = self
            .http
            .post(url(routes::COMPACT, &[session]))
            .json(&SessionCompactParams::default())
            .send()
            .await?;
        decode(response).await
    }

    pub async fn rewind_list(
        &self,
        session: &str,
    ) -> Result<canter_protocol::RewindListResponse, ClientError> {
        decode(
            self.http
                .get(url(routes::REWIND, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn rewind_preview(
        &self,
        session: &str,
        params: &canter_protocol::SessionRewindParams,
    ) -> Result<canter_protocol::RewindPreviewResponse, ClientError> {
        decode(
            self.http
                .post(url(routes::REWIND_PREVIEW, &[session]))
                .json(params)
                .send()
                .await?,
        )
        .await
    }

    pub async fn rewind(
        &self,
        session: &str,
        params: &canter_protocol::SessionRewindParams,
    ) -> Result<canter_protocol::SessionRewindResponse, ClientError> {
        decode(
            self.http
                .post(url(routes::REWIND, &[session]))
                .json(params)
                .send()
                .await?,
        )
        .await
    }

    /// POST /v1/sessions/{id}/requests/{rid}: answer an approval. The first
    /// answer wins; a later one gets `alreadyResolved: true`.
    pub async fn resolve(
        &self,
        session: &str,
        request: &str,
        params: &RequestResolveParams,
    ) -> Result<RequestResolveResponse, ClientError> {
        let response = self
            .http
            .post(url(routes::REQUEST, &[session, request]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    /// GET /v1/events: the session's events with a sequence greater than `after`.
    pub async fn subscribe(&self, session: &str, after: u64) -> Result<EventStream, ClientError> {
        let mut url = url(routes::EVENTS, &[]);
        url.query_pairs_mut()
            .append_pair("session", session)
            .append_pair("after", &after.to_string());
        let response = self
            .http
            .get(url)
            .header(ACCEPT, "text/event-stream")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.bytes().await?;
            return Err(server_error(status, &body));
        }
        Ok(EventStream {
            response,
            parser: SseParser::default(),
            last_seq: after,
        })
    }
}

/// Private, non-replayable answer. Drop it to cancel the server request.
pub struct BtwStream {
    response: reqwest::Response,
    parser: SseParser,
}
impl BtwStream {
    pub async fn next(&mut self) -> Option<Result<canter_protocol::BtwEvent, ClientError>> {
        loop {
            if let Some(data) = self.parser.ready.pop_front() {
                return Some(
                    serde_json::from_str(&data)
                        .map_err(|error| ClientError::Protocol(format!("bad btw event: {error}"))),
                );
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.parser.feed(&bytes),
                Ok(None) => return None,
                Err(error) => return Some(Err(error.into())),
            }
        }
    }
}

/// Events from one SSE subscription, in order.
pub struct EventStream {
    response: reqwest::Response,
    parser: SseParser,
    last_seq: u64,
}

impl EventStream {
    /// The next event, or `None` once the server ends the stream. Events at or
    /// below the cursor are skipped, so a resumed stream never repeats one.
    pub async fn next(&mut self) -> Option<Result<Event, ClientError>> {
        loop {
            while let Some(data) = self.parser.ready.pop_front() {
                match serde_json::from_str::<Event>(&data) {
                    Ok(event) if event.seq <= self.last_seq => {}
                    Ok(event) => {
                        self.last_seq = event.seq;
                        return Some(Ok(event));
                    }
                    Err(error) => {
                        return Some(Err(ClientError::Protocol(format!("bad event: {error}"))));
                    }
                }
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.parser.feed(&bytes),
                Ok(None) => return None,
                Err(error) => return Some(Err(error.into())),
            }
        }
    }

    /// The last event delivered (or the starting cursor); resume from here.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }
}

/// Splits an SSE byte stream into the `data` of each frame. The `id` and
/// `event` fields are not needed: the JSON carries `seq` and `type`.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    data: Option<String>,
    ready: VecDeque<String>,
}

impl SseParser {
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(end) = self.buffer[start..].iter().position(|&b| b == b'\n') {
            let mut line = &self.buffer[start..start + end];
            if let [rest @ .., b'\r'] = line {
                line = rest;
            }
            let line = String::from_utf8_lossy(line).into_owned();
            self.line(&line);
            start += end + 1;
        }
        self.buffer.drain(..start);
    }

    fn line(&mut self, line: &str) {
        if line.is_empty() {
            if let Some(data) = self.data.take() {
                self.ready.push_back(data);
            }
            return;
        }
        if line.starts_with(':') {
            return; // comment or heartbeat
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field == "data" {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            }
        }
    }
}

/// Fills a route's `{…}` segments with `values` in order; each value is
/// percent-encoded as a path segment.
fn url(route: &str, values: &[&str]) -> Url {
    let mut url = Url::parse(ORIGIN).expect("static origin parses");
    let mut values = values.iter();
    url.path_segments_mut()
        .expect("http URLs have a path")
        .clear()
        .extend(route.trim_start_matches('/').split('/').map(|part| {
            if part.starts_with('{') {
                values.next().copied().unwrap_or(part)
            } else {
                part
            }
        }));
    url
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ClientError> {
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        return Err(server_error(status, &body));
    }
    serde_json::from_slice(&body)
        .map_err(|error| ClientError::Protocol(format!("bad response body: {error}")))
}

fn server_error(status: StatusCode, body: &[u8]) -> ClientError {
    match serde_json::from_slice::<ErrorResponse>(body) {
        Ok(error) => ClientError::Server {
            status,
            code: error.code,
            message: error.message,
        },
        Err(_) => ClientError::Protocol(format!("HTTP {status} without an error body")),
    }
}
