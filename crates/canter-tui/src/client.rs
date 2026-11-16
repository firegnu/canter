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
    SessionUpdateResponse, UserInput, routes,
};
use reqwest::header::ACCEPT;
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;

/// The host is ignored on a unix socket; it only makes the URL valid.
const ORIGIN: &str = "http://canter/";

/// Only HTTPS or literal loopback HTTP may carry a remote credential.
/// Credentials and query strings belong in neither the URL nor diagnostics.
pub fn remote_url(value: &str) -> Result<Url, ClientError> {
    let invalid = || {
        ClientError::Protocol("Use an HTTPS server URL, or HTTP with a loopback IP address; tokens must not travel over unencrypted remote HTTP".into())
    };
    let url = Url::parse(value).map_err(|_| invalid())?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}

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

impl ClientError {
    /// Whether the server refused a model it does not list.
    pub fn unknown_model(&self) -> bool {
        matches!(self, Self::Server { code: ErrorCode::InvalidRequest, message, .. }
            if message == "model is not in the configured model list")
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    origin: Url,
    remote: Option<(String, String)>,
    pub remote_cwd: Option<String>,
    sandbox_disabled: bool,
    pub(crate) managed_id: Option<String>,
}

impl Client {
    /// A TCP client uses one fixed origin, never follows redirects, and never
    /// sends its sensitive Authorization header through a system proxy.
    pub fn remote(name: &str, address: &str, token: &str) -> Result<Self, ClientError> {
        let origin = remote_url(address)?;
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ClientError::Protocol("invalid remote token".into()))?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(10))
            // Server heartbeats arrive every 15s, even without session events.
            .read_timeout(std::time::Duration::from_secs(45))
            .build()?;
        Ok(Self {
            http,
            origin,
            remote: Some((name.into(), "unknown".into())),
            remote_cwd: None,
            sandbox_disabled: false,
            managed_id: None,
        })
    }

    pub fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    pub fn with_server_version(mut self, version: String) -> Self {
        if let Some((_, current)) = &mut self.remote {
            *current = version;
        }
        self
    }

    pub fn upgrade_hint(&self, local: &str) -> String {
        match &self.remote {
            Some((name, version)) => {
                format!("Server '{name}' runs canter {version}; upgrade it on that machine")
            }
            None => local.into(),
        }
    }

    /// Each route segment is encoded independently, preserving a proxy prefix.
    fn url(&self, route: &str, values: &[&str]) -> Url {
        let mut url = self.origin.clone();
        let mut values = values.iter();
        url.path_segments_mut()
            .expect("HTTP URL")
            .pop_if_empty()
            .extend(route.trim_start_matches('/').split('/').map(|part| {
                if part.starts_with('{') {
                    values.next().copied().unwrap_or(part)
                } else {
                    part
                }
            }));
        url
    }

    pub async fn memories(
        &self,
        session: &str,
    ) -> Result<canter_protocol::MemoryListResponse, ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "memory")
        {
            return Err(ClientError::Protocol(self.upgrade_hint(
                "This server does not support /memory; run canter serve --replace and retry",
            )));
        }
        decode(
            self.http
                .get(self.url(routes::MEMORIES, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn delete_memory(
        &self,
        session: &str,
        params: &canter_protocol::MemoryDeleteParams,
    ) -> Result<(), ClientError> {
        let response = self
            .http
            .post(self.url(routes::MEMORY_DELETE, &[session]))
            .json(params)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(server_error(status, &response.bytes().await?))
        }
    }

    pub async fn folder_trust(
        &self,
        cwd: &str,
    ) -> Result<canter_protocol::FolderTrustResponse, ClientError> {
        let mut endpoint = self.url(routes::FOLDER_TRUST, &[]);
        endpoint.query_pairs_mut().append_pair("cwd", cwd);
        decode(self.http.get(endpoint).send().await?).await
    }

    pub async fn trust_folder(
        &self,
        cwd: &str,
    ) -> Result<canter_protocol::FolderTrustResponse, ClientError> {
        decode(
            self.http
                .post(self.url(routes::FOLDER_TRUST, &[]))
                .json(&canter_protocol::FolderTrustParams { cwd: cwd.into() })
                .send()
                .await?,
        )
        .await
    }

    pub async fn goal(
        &self,
        session: &str,
        command: &crate::GoalCommand,
    ) -> Result<canter_protocol::GoalResponse, ClientError> {
        use crate::GoalCommand;
        let endpoint = self.url(routes::GOAL, &[session]);
        let request = match command {
            GoalCommand::Read => self.http.get(endpoint),
            GoalCommand::Set(params) => self.http.post(endpoint).json(params),
            GoalCommand::Clear => self.http.delete(endpoint),
            GoalCommand::Pause => self.http.post(self.url(routes::GOAL_PAUSE, &[session])),
            GoalCommand::Resume => self.http.post(self.url(routes::GOAL_RESUME, &[session])),
        };
        decode(request.send().await?).await
    }
    pub async fn background_commands(
        &self,
        session: &str,
    ) -> Result<canter_protocol::BackgroundCommandListResponse, ClientError> {
        decode(
            self.http
                .get(self.url(routes::BACKGROUND_COMMANDS, &[session]))
                .send()
                .await?,
        )
        .await
    }
    /// The sub-agents `session` started.
    pub async fn agents(
        &self,
        session: &str,
    ) -> Result<canter_protocol::AgentListResponse, ClientError> {
        decode(
            self.http
                .get(self.url(routes::SESSION_AGENTS, &[session]))
                .send()
                .await?,
        )
        .await
    }
    /// The user stops sub-agent `agent` of `session` (P2-179).
    pub async fn stop_agent(
        &self,
        session: &str,
        agent: &str,
    ) -> Result<canter_protocol::AgentSummary, ClientError> {
        decode(
            self.http
                .post(self.url(routes::SESSION_AGENT_STOP, &[session, agent]))
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
                .get(self.url(routes::BACKGROUND_OUTPUT, &[session, &id.to_string()]))
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
            Some(id) => self.url(routes::BACKGROUND_STOP, &[session, &id.to_string()]),
            None => self.url(routes::BACKGROUND_STOP_ALL, &[session]),
        };
        decode(self.http.post(endpoint).send().await?).await
    }
    pub async fn language_servers(
        &self,
        session: &str,
    ) -> Result<canter_protocol::LanguageServerListResponse, ClientError> {
        decode(
            self.http
                .get(self.url(routes::LANGUAGE_SERVERS, &[session]))
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
                .post(self.url(route, &[session, server]))
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
                .post(self.url(routes::SHELL_BACKGROUND, &[session]))
                .json(&canter_protocol::ShellBackgroundParams { item_id })
                .send()
                .await?,
        )
        .await
    }
    pub async fn info(&self) -> Result<canter_protocol::InfoResponse, ClientError> {
        decode(self.http.get(self.url(routes::INFO, &[])).send().await?).await
    }

    pub async fn require_workspace(&self) -> Result<(), ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "workspaceQueries")
        {
            return Err(ClientError::Protocol(self.upgrade_hint("This server does not support workspace queries; run canter serve --replace and retry")));
        }
        Ok(())
    }

    pub async fn workspace_summary(
        &self,
        session: &str,
        changed_lines: bool,
    ) -> Result<canter_protocol::WorkspaceSummary, ClientError> {
        let mut url = self.url(routes::WORKSPACE_SUMMARY, &[session]);
        url.query_pairs_mut()
            .append_pair("changedLines", if changed_lines { "true" } else { "false" });
        decode(self.http.get(url).send().await?).await
    }

    pub async fn workspace_diff(
        &self,
        session: &str,
    ) -> Result<canter_protocol::WorkspaceDiff, ClientError> {
        self.require_workspace().await?;
        decode(
            self.http
                .get(self.url(routes::WORKSPACE_DIFF, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn workspace_status(
        &self,
        session: &str,
    ) -> Result<canter_protocol::WorkspaceStatus, ClientError> {
        self.require_workspace().await?;
        decode(
            self.http
                .get(self.url(routes::WORKSPACE_STATUS, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn workspace_review(
        &self,
        session: &str,
        target: &str,
        client_name: &str,
    ) -> Result<SessionInputParams, ClientError> {
        self.require_workspace().await?;
        let mut url = self.url(routes::WORKSPACE_REVIEW, &[session]);
        url.query_pairs_mut()
            .append_pair("target", target)
            .append_pair("clientName", client_name);
        decode(self.http.get(url).send().await?).await
    }

    pub async fn workspace_entries(
        &self,
        session: &str,
    ) -> Result<WorkspaceEntriesStream, ClientError> {
        self.require_workspace().await?;
        let response = self
            .http
            .get(self.url(routes::WORKSPACE_ENTRIES, &[session]))
            .header(ACCEPT, "text/event-stream")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(server_error(response.status(), &response.bytes().await?));
        }
        Ok(WorkspaceEntriesStream {
            response,
            parser: SseParser::default(),
            done: false,
        })
    }

    pub async fn doctor(
        &self,
        client_version: &str,
    ) -> Result<canter_protocol::DoctorResponse, ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "doctor")
        {
            return Err(ClientError::Protocol(self.upgrade_hint(
                "This server is too old for /doctor; run canter serve --replace and retry",
            )));
        }
        let mut endpoint = self.url(routes::DOCTOR, &[]);
        endpoint
            .query_pairs_mut()
            .append_pair("clientVersion", client_version)
            .append_pair(
                "clientProtocolVersion",
                &canter_protocol::PROTOCOL_VERSION.to_string(),
            );
        let mut report: canter_protocol::DoctorResponse =
            decode(self.http.get(endpoint).send().await?).await?;
        if let Some((name, _)) = &self.remote {
            for check in &mut report.data {
                if check.id == "version"
                    && report.server_protocol_version == canter_protocol::PROTOCOL_VERSION
                {
                    check.status = canter_protocol::DoctorStatus::Ok;
                    check.reason = Some(format!(
                        "This client runs canter {client_version}; server '{name}' runs canter {}.",
                        report.server_version
                    ));
                    check.fix = None;
                } else if check
                    .fix
                    .as_ref()
                    .is_some_and(|fix| fix.contains("canter serve --replace"))
                {
                    check.fix = Some(self.upgrade_hint(""));
                }
            }
        }
        Ok(report)
    }

    pub async fn trash_session(&self, session: &str) -> Result<(), ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "sessionTrash")
        {
            return Err(ClientError::Protocol(self.upgrade_hint("This server does not support deleting sessions; run canter serve --replace and retry")));
        }
        let response = self
            .http
            .post(self.url(routes::SESSION_TRASH, &[session]))
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(server_error(status, &response.bytes().await?))
        }
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
            .post(self.url(route, &[session]))
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
        decode(self.http.get(self.url(routes::MCP, &[])).send().await?).await
    }

    pub async fn command_rules(
        &self,
    ) -> Result<canter_protocol::CommandRuleListResponse, ClientError> {
        decode(
            self.http
                .get(self.url(routes::COMMAND_RULES, &[]))
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
                .post(self.url(routes::COMMAND_RULE_DELETE, &[]))
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
                .post(self.url(routes::FILE_RULE_DELETE, &[]))
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
                .get(self.url(routes::CONTEXT, &[session]))
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
                .get(self.url(routes::SANDBOX, &[session]))
                .send()
                .await?,
        )
        .await
    }

    pub async fn set_sandbox(
        &self,
        session: &str,
        enabled: bool,
    ) -> Result<canter_protocol::SessionUpdateResponse, ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "sessionSandboxSwitch")
        {
            return Err(ClientError::Protocol(self.upgrade_hint("This server does not support /sandbox on|off; run canter serve --replace and retry")));
        }
        decode(
            self.http
                .post(self.url(routes::SANDBOX, &[session]))
                .json(&canter_protocol::SessionSandboxParams {
                    enabled,
                    confirmed: !enabled,
                })
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
            Some(id) => self.url(routes::SESSION_USAGE, &[id]),
            None => self.url(routes::USAGE, &[]),
        };
        decode(self.http.get(endpoint).send().await?).await
    }

    async fn require_stats(&self) -> Result<(), ClientError> {
        if self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "stats")
        {
            Ok(())
        } else {
            Err(ClientError::Protocol(self.upgrade_hint(
                "this server does not support stats; run canter serve --replace",
            )))
        }
    }

    pub async fn stats_list(
        &self,
        all: bool,
    ) -> Result<canter_protocol::StatsListResponse, ClientError> {
        self.require_stats().await?;
        let mut endpoint = self.url(routes::STATS, &[]);
        endpoint
            .query_pairs_mut()
            .append_pair("all", if all { "true" } else { "false" });
        decode(self.http.get(endpoint).send().await?).await
    }

    pub async fn stats(
        &self,
        session: &str,
    ) -> Result<canter_protocol::StatsResponse, ClientError> {
        self.require_stats().await?;
        decode(
            self.http
                .get(self.url(routes::SESSION_STATS, &[session]))
                .send()
                .await?,
        )
        .await
    }

    /// GET /v1/sessions/{id}/roles: available roles and per-file errors.
    pub async fn list_roles(
        &self,
        session: &str,
    ) -> Result<canter_protocol::AgentRoleListResponse, ClientError> {
        let response = self
            .http
            .get(self.url(routes::ROLES, &[session]))
            .send()
            .await?;
        decode(response).await
    }

    /// GET /v1/sessions/{id}/skills: the session's fixed skill list.
    pub async fn list_skills(
        &self,
        session: &str,
    ) -> Result<Vec<canter_protocol::Skill>, ClientError> {
        let response = self
            .http
            .get(self.url(routes::SKILLS, &[session]))
            .send()
            .await?;
        Ok(decode::<canter_protocol::SkillListResponse>(response)
            .await?
            .data)
    }

    /// GET /v1/models: the server's configured and account model catalogs.
    pub async fn list_models(&self) -> Result<canter_protocol::ModelListResponse, ClientError> {
        let response = self.http.get(self.url(routes::MODELS, &[])).send().await?;
        decode::<canter_protocol::ModelListResponse>(response).await
    }

    /// Builds a client for the server listening on `socket`. Does not connect yet.
    pub fn new(socket: &Path) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .unix_socket(socket.to_path_buf())
            .build()?;
        Ok(Self {
            http,
            origin: Url::parse(ORIGIN).expect("static origin"),
            remote: None,
            remote_cwd: None,
            sandbox_disabled: false,
            managed_id: None,
        })
    }

    /// Apply this terminal's sandbox choice to every session it opens.
    pub fn with_sandbox_disabled(mut self, disabled: bool) -> Self {
        self.sandbox_disabled = disabled;
        self
    }

    async fn require_session_sandbox(&self, disabled: bool) -> Result<(), ClientError> {
        if disabled
            && !self
                .info()
                .await?
                .features
                .unwrap_or_default()
                .iter()
                .any(|feature| feature == "sessionSandbox")
        {
            return Err(ClientError::Protocol(
                self.upgrade_hint("This server does not support --yolo without a sandbox; run canter serve --replace and retry"),
            ));
        }
        Ok(())
    }

    /// Give this terminal run a fresh, memory-only closing responsibility.
    pub fn with_close_on_exit(mut self) -> std::io::Result<Self> {
        use std::io::Read;
        let mut bytes = [0; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        self.managed_id = Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
        Ok(self)
    }

    fn opening_request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.managed_id {
            Some(id) => request.header(routes::MANAGED_CLIENT_HEADER, id),
            None => request,
        }
    }

    pub(crate) async fn managed_connect(&self, id: &str) -> Result<reqwest::Response, ClientError> {
        let response = self
            .http
            .get(self.url(routes::MANAGED_CLIENT, &[id]))
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(server_error(status, &response.bytes().await?))
        }
    }

    pub(crate) async fn managed_close(&self, id: &str) -> Result<(), ClientError> {
        self.http
            .delete(self.url(routes::MANAGED_CLIENT, &[id]))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// GET /v1/sessions: every session, most recently updated first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, ClientError> {
        let response = self
            .http
            .get(self.url(routes::SESSIONS, &[]))
            .send()
            .await?;
        Ok(decode::<SessionListResponse>(response).await?.data)
    }

    /// Read-only content search, with a useful message for older servers.
    pub async fn search_sessions(
        &self,
        params: &canter_protocol::SessionSearchParams,
    ) -> Result<canter_protocol::SessionSearchResponse, ClientError> {
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "sessionSearch")
        {
            return Err(ClientError::Protocol(self.upgrade_hint(
                "This server does not support session search; run canter upgrade and retry",
            )));
        }
        let mut target = self.url(routes::SESSION_SEARCH, &[]);
        target.query_pairs_mut().append_pair("query", &params.query);
        if let Some(cwd) = &params.cwd {
            target.query_pairs_mut().append_pair("cwd", cwd);
        }
        decode(self.http.get(target).send().await?).await
    }

    /// POST /v1/sessions: a new session. The body carries the environment,
    /// so it is never logged.
    pub async fn create_session(
        &self,
        params: &SessionCreateParams,
    ) -> Result<SessionCreateResponse, ClientError> {
        let mut params = params.clone();
        params.sandbox_disabled |= self.sandbox_disabled;
        if let Some(input) = &mut params.input {
            self.prepare_images(input).await?;
        }
        if self.is_remote() {
            params.env = None;
        }
        self.require_session_sandbox(params.sandbox_disabled)
            .await?;
        let response = self
            .opening_request(self.http.post(self.url(routes::SESSIONS, &[])))
            .json(&params)
            .send()
            .await?;
        decode(response).await
    }

    /// GET /v1/sessions/{id}: the session's full snapshot and its `lastEventSeq`.
    pub async fn read_session(&self, session: &str) -> Result<SessionReadResponse, ClientError> {
        let response = self
            .http
            .get(self.url(routes::SESSION, &[session]))
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
        self.resume_in_mode(session, env, false).await
    }

    pub async fn resume_in_mode(
        &self,
        session: &str,
        env: &BTreeMap<String, String>,
        restricted_mode: bool,
    ) -> Result<SessionResumeResponse, ClientError> {
        let params = SessionResumeParams {
            sandbox_disabled: self.sandbox_disabled,
            env: (!self.is_remote()).then(|| env.clone()),
            restricted_mode,
        };
        self.require_session_sandbox(params.sandbox_disabled)
            .await?;
        let response = self
            .opening_request(self.http.post(self.url(routes::RESUME, &[session])))
            .json(&params)
            .send()
            .await?;
        decode(response).await
    }

    /// POST /v1/sessions/{id}/fork: copy completed turns into a new session.
    pub async fn fork_session(
        &self,
        session: &str,
        params: &SessionResumeParams,
    ) -> Result<canter_protocol::SessionCreateResponse, ClientError> {
        let mut params = params.clone();
        params.sandbox_disabled |= self.sandbox_disabled;
        if self.is_remote() {
            params.env = None;
        }
        self.require_session_sandbox(params.sandbox_disabled)
            .await?;
        let response = self
            .opening_request(self.http.post(self.url(routes::SESSION_FORK, &[session])))
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
            .patch(self.url(routes::SESSION, &[session]))
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
        let mut params = std::borrow::Cow::Borrowed(params);
        if params
            .input
            .iter()
            .any(|part| matches!(part, UserInput::LocalImage { .. }))
        {
            self.prepare_images(&mut params.to_mut().input).await?;
        }
        let response = self
            .http
            .post(self.url(routes::INPUT, &[session]))
            .json(params.as_ref())
            .send()
            .await?;
        decode(response).await
    }

    async fn prepare_images(&self, input: &mut Vec<UserInput>) -> Result<(), ClientError> {
        if !input
            .iter()
            .any(|part| matches!(part, UserInput::LocalImage { .. }))
        {
            return Ok(());
        }
        if !self
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "imageData")
        {
            return Err(ClientError::Protocol(self.upgrade_hint(
                "This server does not support image data; run canter serve --replace and retry",
            )));
        }
        let parts = std::mem::take(input);
        *input = tokio::task::spawn_blocking(move || crate::images::prepare(parts))
            .await
            .map_err(|_| ClientError::Protocol("could not prepare images".into()))?
            .map_err(ClientError::Protocol)?;
        Ok(())
    }

    pub async fn shell_command(
        &self,
        session: &str,
        params: &SessionShellCommandParams,
    ) -> Result<SessionShellCommandResponse, ClientError> {
        let response = self
            .http
            .post(self.url(routes::SHELL_COMMAND, &[session]))
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
            .post(self.url(routes::INTERRUPT, &[session]))
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
            .post(self.url(routes::COMPACT, &[session]))
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
                .get(self.url(routes::REWIND, &[session]))
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
                .post(self.url(routes::REWIND_PREVIEW, &[session]))
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
                .post(self.url(routes::REWIND, &[session]))
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
            .post(self.url(routes::REQUEST, &[session, request]))
            .json(params)
            .send()
            .await?;
        decode(response).await
    }

    /// GET /v1/events: the session's events with a sequence greater than `after`.
    pub async fn subscribe(&self, session: &str, after: u64) -> Result<EventStream, ClientError> {
        let mut url = self.url(routes::EVENTS, &[]);
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

/// One walk, with no replay or pagination that would restart the traversal.
pub struct WorkspaceEntriesStream {
    response: reqwest::Response,
    parser: SseParser,
    done: bool,
}
impl WorkspaceEntriesStream {
    pub async fn next(
        &mut self,
    ) -> Option<Result<canter_protocol::WorkspaceEntriesBatch, ClientError>> {
        if self.done {
            return None;
        }
        loop {
            if let Some(data) = self.parser.ready.pop_front() {
                return Some(
                    match serde_json::from_str::<canter_protocol::WorkspaceEntriesBatch>(&data) {
                        Ok(batch) => {
                            self.done = batch.done;
                            match &batch.error {
                                Some(error) => Err(ClientError::Protocol(error.clone())),
                                None => Ok(batch),
                            }
                        }
                        Err(error) => Err(ClientError::Protocol(format!(
                            "bad workspace file batch: {error}"
                        ))),
                    },
                );
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.parser.feed(&bytes),
                Ok(None) => {
                    self.done = true;
                    return Some(Err(ClientError::Protocol(
                        "Workspace file scan ended before completion".into(),
                    )));
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error.into()));
                }
            }
        }
    }
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
                    Ok(event) if event.payload.is_ephemeral() => return Some(Ok(event)),
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
        Err(_) if status == StatusCode::UNAUTHORIZED => ClientError::Server {
            status,
            code: ErrorCode::Unauthorized,
            message: "unauthorized".into(),
        },
        Err(_) => ClientError::Protocol(format!("HTTP {status} without an error body")),
    }
}
