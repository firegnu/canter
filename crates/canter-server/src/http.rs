use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    convert::Infallible,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use crate::session_locks::{SessionGuard, SessionLocks};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        FromRequestParts, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use canter_core::{CoreError, CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::Model;
use canter_protocol::*;
use canter_store::Store;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

#[path = "agent_sessions.rs"]
mod agent_sessions;

#[path = "cleanup.rs"]
pub(crate) mod cleanup;

#[path = "managed.rs"]
mod managed;

#[path = "account.rs"]
pub(crate) mod account;

pub(crate) struct App {
    pub reload: Option<Arc<crate::reload::LiveConfig>>,
    pub home: PathBuf,
    pub store: Arc<Store>,
    pub model: Arc<dyn Model>,
    pub default_model: String,
    pub models: Vec<String>,
    pub options: CoreOptions,
    pub mcp: canter_core::mcp::Registry,
    pub lsp: Arc<canter_tools::lsp::LspService>,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub session_locks: SessionLocks,
    pub registry_changed: Notify,
    pub managed: managed::Clients,
    #[cfg(test)]
    pub pauses: tests::session_management::registry::Pauses,
    pub draining: RwLock<bool>,
    pub stopped: CancellationToken,
}

impl App {
    async fn opened_session(&self, id: &str) -> Option<Session> {
        self.sessions.lock().await.get(id).cloned()
    }

    pub(crate) async fn opened_sessions(&self) -> Vec<Session> {
        self.sessions.lock().await.values().cloned().collect()
    }

    async fn register_session(&self, id: String, session: Session) -> Result<(), ApiError> {
        self.sessions
            .lock()
            .await
            .insert(id.clone(), session.clone());
        if let Some(source) = session.snapshot().await.session.parent {
            if let Some(parent) = self.opened_session(&source.session_id).await {
                parent.attach_agent(&session).await?;
            } else {
                session.restore_agent_policy().await?;
            }
        } else {
            for child in self.opened_sessions().await {
                if child
                    .snapshot()
                    .await
                    .session
                    .parent
                    .as_ref()
                    .is_some_and(|source| source.session_id == id)
                {
                    session.attach_agent(&child).await?;
                }
            }
        }
        self.registry_changed.notify_waiters();
        Ok(())
    }

    pub(crate) async fn begin_drain(&self) -> Vec<Session> {
        self.managed.disarm();
        *self.draining.write().await = true;
        let sessions = self.opened_sessions().await;
        for session in &sessions {
            session.drain().await;
        }
        sessions
    }

    fn metadata(&self) -> (String, Vec<String>, CoreOptions) {
        self.reload.as_ref().map_or_else(
            || {
                (
                    self.default_model.clone(),
                    self.models.clone(),
                    self.options.clone(),
                )
            },
            |reload| reload.metadata(),
        )
    }

    async fn configuration(&self) -> Result<canter_core::Configuration, ApiError> {
        if let Some(reload) = &self.reload {
            use canter_core::ConfigurationSource;
            return Ok(reload.snapshot());
        }
        Ok(canter_core::Configuration {
            revision: Some(0),
            options: self.session_options().await?,
            model: self.model.clone(),
            mcp_tools: self.mcp.snapshot(),
            lsp: Some(self.lsp.clone()),
            lease: Arc::new(()),
        })
    }

    async fn configure_session(
        self: &Arc<Self>,
        session: &Session,
        revision: Option<u64>,
    ) -> Result<(), ApiError> {
        session.set_agent_host(Arc::new(agent_sessions::Host(Arc::downgrade(self))));
        if let Some(reload) = &self.reload {
            session.set_configuration_source(reload.clone(), revision);
            if let Some(message) = reload.warning().await {
                session.configuration_warning(message).await?;
            }
        }
        Ok(())
    }

    async fn session_options(&self) -> Result<CoreOptions, ApiError> {
        let home = self.home.clone();
        let enabled =
            tokio::task::spawn_blocking(move || crate::config::Config::session_switches(&home))
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(|error| ApiError::new(ErrorCode::InvalidRequest, &error.to_string()))?;
        let mut options = self.options.clone();
        options.command_rules_path = Some(self.home.join("rules.json"));
        if let Some((plan_enabled, mcp_tool_search)) = enabled {
            options.plan_enabled = plan_enabled;
            options.mcp_tool_search = mcp_tool_search;
        }
        Ok(options)
    }

    fn validate_model(&self, model: &str) -> Result<(), ApiError> {
        if self.metadata().1.iter().any(|available| available == model) {
            Ok(())
        } else {
            Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "model is not in the configured model list",
            ))
        }
    }

    fn runtime(
        &self,
        environment: std::collections::BTreeMap<String, String>,
        config: &canter_core::Configuration,
        restricted_mode: bool,
        sandbox_disabled: bool,
    ) -> RuntimeContext {
        RuntimeContext {
            sandbox_disabled,
            memory_home: Some(self.home.clone()),
            restricted_mode,
            lsp_file_access: Some(crate::trust::file_access(self.home.clone())),
            lsp: config.lsp.clone(),
            environment,
            mcp_tools: config.mcp_tools.clone(),
            protected_paths: vec![self.home.clone(), self.home.join("config.json")],
        }
    }

    async fn session(&self, id: &str) -> Result<Session, ApiError> {
        let operation = self.session_locks.lock(id).await;
        if let Some(session) = self.opened_session(id).await {
            return Ok(session);
        }
        self.disk_snapshot(id, operation).await?;
        Err(ApiError::new(
            ErrorCode::SessionNotResumed,
            "resume the session first: POST /v1/sessions/{id}/resume with env again",
        ))
    }

    async fn disk_events(
        &self,
        id: &str,
        after: u64,
        operation: Option<Arc<SessionGuard>>,
    ) -> Result<Vec<Event>, ApiError> {
        #[cfg(test)]
        self.pauses.hit(id, "events").await;
        let store = self.store.clone();
        let id = id.to_string();
        #[cfg(test)]
        let pauses = self.pauses.clone();
        tokio::task::spawn_blocking(move || {
            // Keep the exclusion alive even if the HTTP read is cancelled.
            let _operation = operation;
            #[cfg(test)]
            tokio::runtime::Handle::current().block_on(pauses.hit(&id, "disk_io"));
            store.events_after(&id, after)
        })
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|error| CoreError::from(error).into())
    }

    async fn disk_snapshot(
        &self,
        id: &str,
        operation: Arc<SessionGuard>,
    ) -> Result<SessionReadResponse, ApiError> {
        #[cfg(test)]
        self.pauses.hit(id, "snapshot").await;
        let store = self.store.clone();
        let prices = self.metadata().2.prices;
        let id = id.to_string();
        #[cfg(test)]
        let pauses = self.pauses.clone();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            #[cfg(test)]
            tokio::runtime::Handle::current().block_on(pauses.hit(&id, "disk_io"));
            let mut snapshot = SessionSnapshot::empty(&id);
            let events = store.events_after(&id, 0).map_err(CoreError::from)?;
            for event in &events {
                apply_event_mut(&mut snapshot, event).map_err(|_| ApiError::internal())?;
            }
            canter_core::usage::Ledger::replay(&events, &prices).fill_costs(&mut snapshot);
            if snapshot.last_event_seq == 0 {
                return Err(ApiError::new(
                    ErrorCode::SessionNotFound,
                    "session not found",
                ));
            }
            Ok(SessionReadResponse {
                event_seq: snapshot.last_event_seq,
                session: snapshot,
            })
        })
        .await
        .map_err(|_| ApiError::internal())?
    }
}

pub(crate) fn router(app: Arc<App>) -> Router {
    let auth = app.reload.as_ref().map_or_else(
        || crate::model_auth::ModelAuth::new(app.home.clone()),
        |reload| reload.auth.clone(),
    );
    let accounts = account::router(auth, app.stopped.clone());
    router_with_accounts(app, accounts)
}

fn router_with_accounts(app: Arc<App>, accounts: Router) -> Router {
    Router::new()
        .route(routes::INFO, get(info))
        .route(routes::RUNNING_TURNS, get(running_turns))
        .route(routes::FOLDER_TRUST, get(folder_trust).post(trust_folder))
        .route(routes::MCP, get(mcp))
        .route(routes::COMMAND_RULES, get(command_rules))
        .route(routes::MEMORIES, get(memories))
        .route(routes::MEMORY_DELETE, post(delete_memory))
        .route(routes::COMMAND_RULE_DELETE, post(delete_command_rule))
        .route(routes::FILE_RULE_DELETE, post(delete_file_rule))
        .route(routes::MODELS, get(models))
        .route(routes::USAGE, get(usage))
        .route(routes::SESSION_USAGE, get(session_usage))
        .route(routes::STATS, get(stats_list))
        .route(routes::SESSION_STATS, get(session_stats))
        .route(routes::SESSIONS, post(create).get(list))
        .route(
            routes::MANAGED_CLIENT,
            get(managed::connect).delete(managed::close),
        )
        .route(routes::SESSION, get(read).patch(update).delete(close))
        .route(routes::SESSION_TRASH, post(trash))
        .route(routes::SESSION_FORK, post(fork))
        .route(routes::SESSION_AGENTS, get(agents))
        .route(routes::SESSION_AGENT_STOP, post(stop_agent))
        .route(routes::GOAL, get(goal).post(set_goal).delete(clear_goal))
        .route(routes::GOAL_PAUSE, post(pause_goal))
        .route(routes::GOAL_RESUME, post(resume_goal))
        .route(routes::SKILLS, get(skills))
        .route(routes::CONTEXT, get(context))
        .route(routes::SANDBOX, get(sandbox))
        .route(routes::BTW, post(btw))
        .route(routes::HANDOFF, post(handoff))
        .route(routes::RESUME, post(resume))
        .route(routes::INPUT, post(input))
        .route(routes::SHELL_COMMAND, post(shell_command))
        .route(routes::SHELL_BACKGROUND, post(shell_background))
        .route(routes::BACKGROUND_COMMANDS, get(background_commands))
        .route(routes::LANGUAGE_SERVERS, get(language_servers))
        .route(routes::DIAGNOSTICS, get(diagnostics))
        .route(routes::LANGUAGE_SERVER_STOP, post(language_server_stop))
        .route(routes::LANGUAGE_SERVER_RETRY, post(language_server_retry))
        .route(routes::BACKGROUND_OUTPUT, get(background_output))
        .route(routes::BACKGROUND_STOP, post(background_stop))
        .route(routes::BACKGROUND_STOP_ALL, post(background_stop_all))
        .route(routes::INTERRUPT, post(interrupt))
        .route(routes::COMPACT, post(compact))
        .route(routes::REWIND, get(rewind_list).post(rewind))
        .route(routes::REWIND_PREVIEW, post(rewind_preview))
        .route(routes::REQUEST, post(resolve))
        .route(routes::EVENTS, get(events))
        .fallback(|| async { ApiError::invalid() })
        .method_not_allowed_fallback(|| async { ApiError::invalid() })
        .with_state(app)
        .merge(accounts)
}

async fn goal(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<GoalResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.goal().await))
}

async fn set_goal(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<GoalSetParams>, JsonRejection>,
) -> Result<Json<GoalResponse>, ApiError> {
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.set_goal(body(params)?).await?))
}

async fn clear_goal(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<GoalResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.control_goal("clear").await?))
}
async fn pause_goal(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<GoalResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.control_goal("pause").await?))
}
async fn resume_goal(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<GoalResponse>, ApiError> {
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.control_goal("resume").await?))
}

pub(crate) struct ApiError(ErrorResponse);
impl ApiError {
    fn new(code: ErrorCode, message: &str) -> Self {
        Self(ErrorResponse {
            code,
            message: message.into(),
        })
    }
    fn invalid() -> Self {
        Self::new(ErrorCode::InvalidRequest, "invalid request")
    }
    fn internal() -> Self {
        Self::new(ErrorCode::Internal, "server operation failed")
    }
}
impl From<CoreError> for ApiError {
    fn from(error: CoreError) -> Self {
        Self(ErrorResponse {
            code: error.code,
            message: error.message,
        })
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ErrorCode::InvalidRequest | ErrorCode::InvalidDecision => StatusCode::BAD_REQUEST,
            ErrorCode::SessionNotFound | ErrorCode::RequestNotFound => StatusCode::NOT_FOUND,
            ErrorCode::TurnMismatch
            | ErrorCode::SessionClosed
            | ErrorCode::SessionNotResumed
            | ErrorCode::Conflict => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0)).into_response()
    }
}

fn body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(value)| value)
        .map_err(|_| ApiError::invalid())
}

// Axum's default path rejection is plain text; keep malformed URLs on the JSON error contract too.
struct ApiPath<T>(T);
impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| ApiError::invalid())
    }
}

async fn info(State(app): State<Arc<App>>) -> Result<Json<InfoResponse>, ApiError> {
    let store = app.store.clone();
    let sessions = tokio::task::spawn_blocking(move || store.list())
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(CoreError::from)?;
    Ok(Json(InfoResponse {
        features: Some(vec![
            "sessionSandbox".into(),
            "managedClients".into(),
            "accounts".into(),
            "subagentSessions".into(),
            "subagentTools".into(),
            "subagentStop".into(),
            "memory".into(),
            "runningTurns".into(),
            "sessionFork".into(),
            "conditionalName".into(),
            "sessionTrash".into(),
            "folderTrust".into(),
            "nonInteractiveInput".into(),
            "review".into(),
            "addDirectories".into(),
            "tags".into(),
            "stats".into(),
            "goals".into(),
            "lsp".into(),
        ]),
        server_version: crate::VERSION.into(),
        protocol_version: PROTOCOL_VERSION,
        sessions,
    }))
}

async fn folder_trust(
    State(app): State<Arc<App>>,
    params: Result<Query<FolderTrustParams>, QueryRejection>,
) -> Result<Json<FolderTrustResponse>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    folder_trust_operation(app.home.clone(), params.cwd, false)
        .await
        .map(Json)
}

async fn trust_folder(
    State(app): State<Arc<App>>,
    params: Result<Json<FolderTrustParams>, JsonRejection>,
) -> Result<Json<FolderTrustResponse>, ApiError> {
    let params = body(params)?;
    folder_trust_operation(app.home.clone(), params.cwd, true)
        .await
        .map(Json)
}

async fn folder_trust_operation(
    home: PathBuf,
    cwd: String,
    grant: bool,
) -> Result<FolderTrustResponse, ApiError> {
    tokio::task::spawn_blocking(move || {
        if grant {
            crate::trust::grant(&home, std::path::Path::new(&cwd))
        } else {
            crate::trust::query(&home, std::path::Path::new(&cwd))
        }
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|error| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            &format!("Could not access folder trust: {error}"),
        )
    })
}

async fn command_rules(
    State(app): State<Arc<App>>,
) -> Result<Json<CommandRuleListResponse>, ApiError> {
    let path = app.home.join("rules.json");
    let data = tokio::task::spawn_blocking({
        let path = path.clone();
        move || canter_tools::rules::list(&path)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|error| command_rules_error(&path, error))?;
    Ok(Json(data))
}

async fn memories(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<MemoryListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.memories().await?))
}

async fn delete_memory(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<MemoryDeleteParams>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    app.session(&id).await?.delete_memory(body(params)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn command_rules_error(path: &std::path::Path, error: std::io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::Internal,
        &format!(
            "Could not read or update rules at {}: {error}",
            path.display()
        ),
    )
}

async fn delete_command_rule(
    State(app): State<Arc<App>>,
    params: Result<Json<CommandRule>, JsonRejection>,
) -> Result<Json<CommandRuleDeleteResponse>, ApiError> {
    let rule = body(params)?;
    let path = app.home.join("rules.json");
    let removed = tokio::task::spawn_blocking({
        let path = path.clone();
        move || canter_tools::rules::delete(&path, &rule)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|error| command_rules_error(&path, error))?;
    Ok(Json(CommandRuleDeleteResponse {
        removed,
        message: match removed {
            0 => "The command rule no longer exists.".into(),
            1 => "Deleted 1 command rule.".into(),
            count => format!("Deleted {count} matching command rules."),
        },
    }))
}

async fn delete_file_rule(
    State(app): State<Arc<App>>,
    params: Result<Json<FileRule>, JsonRejection>,
) -> Result<Json<CommandRuleDeleteResponse>, ApiError> {
    let rule = body(params)?;
    let path = app.home.join("rules.json");
    let removed = tokio::task::spawn_blocking({
        let path = path.clone();
        move || canter_tools::rules::delete_file(&path, &rule)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|error| command_rules_error(&path, error))?;
    Ok(Json(CommandRuleDeleteResponse {
        removed,
        message: match removed {
            0 => "The file rule no longer exists.".into(),
            1 => "Deleted 1 file rule.".into(),
            count => format!("Deleted {count} matching file rules."),
        },
    }))
}

async fn models(State(app): State<Arc<App>>) -> Json<ModelListResponse> {
    if let Some(reload) = &app.reload
        && let Ok(auth) = reload.auth.get().await
    {
        reload.auth.catalog().refresh_stale(auth);
    }
    let (default_model, models, options) = app.metadata();
    let catalogs = app.reload.as_ref().and_then(|reload| {
        let provider = reload.catalog_provider()?;
        reload.auth.catalog().status(&provider)
    });
    Json(ModelListResponse {
        data: models
            .iter()
            .map(|model| {
                let details = app
                    .reload
                    .as_ref()
                    .and_then(|reload| reload.auth.catalog().details(model));
                let configured = app
                    .reload
                    .as_ref()
                    .is_none_or(|reload| reload.configured_model(model));
                let missing = if configured {
                    Vec::new()
                } else {
                    details
                        .as_ref()
                        .map(|(_, missing)| missing.clone())
                        .unwrap_or_default()
                };
                canter_protocol::Model {
                    id: model.clone(),
                    model: model.clone(),
                    display_name: details
                        .as_ref()
                        .map(|(name, _)| name.clone())
                        .unwrap_or_else(|| model.clone()),
                    is_default: model == &default_model,
                    provider: options
                        .model_limits
                        .contains_key(model)
                        .then(|| model.split_once('/').expect("qualified model").0.to_owned()),
                    context_window: Some(options.limits(model).context_window),
                    supported_reasoning_efforts: Some(options.reasoning(model).supported),
                    default_reasoning_effort: Some(options.reasoning(model).default),
                    input_modalities: Some(if options.image_models.contains(model) {
                        vec!["text".into(), "image".into()]
                    } else {
                        vec!["text".into()]
                    }),
                    verified: Some(missing.is_empty()),
                    missing_capabilities: (!missing.is_empty()).then_some(missing),
                    from_catalog: Some(details.is_some()),
                }
            })
            .collect(),
        next_cursor: None,
        catalogs: catalogs.map(|status| vec![status]),
    })
}

async fn list(state: State<Arc<App>>) -> Result<Json<SessionListResponse>, ApiError> {
    Ok(Json(SessionListResponse {
        data: info(state).await?.0.sessions,
    }))
}

async fn agents(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<AgentListResponse>, ApiError> {
    let operation = app.session_locks.lock(&id).await;
    if app.opened_session(&id).await.is_none() {
        app.disk_snapshot(&id, operation.clone()).await?;
    }
    let store = app.store.clone();
    let children = tokio::task::spawn_blocking(move || store.list_all())
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(CoreError::from)?;
    let mut data = Vec::new();
    #[cfg(test)]
    app.pauses.hit(&id, "agents.listed").await;
    for child in children.into_iter().filter(|child| {
        child
            .parent
            .as_ref()
            .is_some_and(|parent| parent.session_id == id)
    }) {
        let snapshot = match read(State(app.clone()), ApiPath(child.id)).await {
            Ok(response) => response.0.session,
            Err(error) if error.0.code == ErrorCode::SessionNotFound => continue,
            Err(error) => return Err(error),
        };
        if let Some(agent) = AgentSummary::from_snapshot(&snapshot) {
            data.push(agent);
        }
    }
    Ok(Json(AgentListResponse { data }))
}

/// P2-179: stop a running sub-agent without waking the parent.
async fn stop_agent(
    State(app): State<Arc<App>>,
    ApiPath((id, agent_id)): ApiPath<(String, String)>,
) -> Result<Json<AgentSummary>, ApiError> {
    Ok(Json(app.stop_agent(&id, &agent_id).await?))
}

async fn running_turns(State(app): State<Arc<App>>) -> Json<RunningTurnsResponse> {
    let mut data = Vec::new();
    for session in app.opened_sessions().await {
        if let Some(work) = session.running_work().await {
            data.push(work);
        }
    }
    data.sort_by(|a, b| a.id.cmp(&b.id));
    Json(RunningTurnsResponse { data })
}

async fn ledger(app: &App, id: &str) -> Result<canter_core::usage::Ledger, ApiError> {
    let operation = app.session_locks.lock(id).await;
    let (mut events, through) = if let Some(session) = app.opened_session(id).await {
        drop(operation);
        let through = session.snapshot().await.event_seq;
        (app.disk_events(id, 0, None).await?, Some(through))
    } else {
        // An unopened session must stay unopened until the disk read completes.
        let events = app.disk_events(id, 0, Some(operation)).await?;
        (events, None)
    };
    if let Some(through) = through {
        events.retain(|event| event.seq <= through);
    }
    if events.is_empty() {
        return Err(ApiError::new(
            ErrorCode::SessionNotFound,
            "session not found",
        ));
    }
    Ok(canter_core::usage::Ledger::replay(
        &events,
        &app.metadata().2.prices,
    ))
}

async fn session_usage(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<UsageResponse>, ApiError> {
    Ok(Json(ledger(&app, &id).await?.report))
}

async fn stats_list(
    State(app): State<Arc<App>>,
    params: Result<Query<StatsListParams>, QueryRejection>,
) -> Result<Json<StatsListResponse>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    let store = app.store.clone();
    let report = tokio::task::spawn_blocking(move || canter_core::stats::list(&store, params.all))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(CoreError::from)?;
    Ok(Json(report))
}

async fn session_stats(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<StatsResponse>, ApiError> {
    let store = app.store.clone();
    let prices = app.metadata().2.prices;
    let report =
        tokio::task::spawn_blocking(move || canter_core::stats::report(&store, &id, &prices))
            .await
            .map_err(|_| ApiError::internal())?
            .map_err(CoreError::from)?;
    Ok(Json(report))
}

async fn usage(State(app): State<Arc<App>>) -> Result<Json<UsageResponse>, ApiError> {
    let store = app.store.clone();
    let sessions = tokio::task::spawn_blocking(move || store.list_all())
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(CoreError::from)?;
    let mut live = std::collections::HashSet::new();
    let mut rows = BTreeMap::<(String, String), UsageRow>::new();
    let mut report = UsageResponse::default();
    for session in sessions {
        let session_ledger = match ledger(&app, &session.id).await {
            Ok(ledger) => ledger,
            Err(error) if error.0.code == ErrorCode::SessionNotFound => continue,
            Err(error) => return Err(error),
        };
        live.insert(session.id);
        for (timestamp, record) in session_ledger.records {
            let date = chrono::DateTime::from_timestamp_millis(timestamp)
                .ok_or_else(ApiError::internal)?
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string();
            let model = record.model.clone().unwrap_or_default();
            let row = rows
                .entry((date.clone(), model.clone()))
                .or_insert_with(|| UsageRow {
                    date: Some(date),
                    model: Some(model),
                    ..UsageRow::default()
                });
            row.add(&record);
            report.total.add(&record);
        }
    }
    let home = app.home.clone();
    let archived = tokio::task::spawn_blocking(move || cleanup::archived_rows(&home, &live))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| ApiError::internal())?;
    for record in archived {
        let date = record.date.clone().unwrap_or_default();
        let model = record.model.clone().unwrap_or_default();
        let row = rows
            .entry((date.clone(), model.clone()))
            .or_insert_with(|| UsageRow {
                date: Some(date),
                model: Some(model),
                ..UsageRow::default()
            });
        row.add(&record);
        report.total.add(&record);
    }
    report.data = rows.into_values().collect();
    Ok(Json(report))
}

async fn context(
    State(app): State<Arc<App>>,
    ApiPath(SessionPath { id }): ApiPath<SessionPath>,
) -> Result<Json<ContextResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.context().await))
}

async fn sandbox(
    State(app): State<Arc<App>>,
    ApiPath(SessionPath { id }): ApiPath<SessionPath>,
) -> Result<Json<SessionSandboxResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.sandbox().await))
}

async fn skills(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<SkillListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.skills().await))
}

async fn btw(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionBtwParams>, JsonRejection>,
) -> Result<Response, ApiError> {
    let params = body(params)?;
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    let answer = app.session(&id).await?.btw(params.question).await?;
    Ok(private_answer(answer))
}

async fn handoff(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionHandoffParams>, JsonRejection>,
) -> Result<Response, ApiError> {
    let params = body(params)?;
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    let answer = app.session(&id).await?.handoff(params.direction).await?;
    Ok(private_answer(answer))
}

fn private_answer(mut answer: canter_core::BtwStream) -> Response {
    let stream = async_stream::stream! {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                event = answer.recv() => match event {
                    Some(event) => {
                        let terminal = matches!(event, BtwEvent::Completed | BtwEvent::Cancelled | BtwEvent::Error { .. });
                        yield Ok::<_, Infallible>(format!("data: {}\n\n", serde_json::to_string(&event).expect("btw event")));
                        if terminal { break; }
                    }
                    None => break,
                },
                _ = heartbeat.tick() => yield Ok(": heartbeat\n\n".to_owned()),
            }
        }
    };
    (
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn read(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<SessionReadResponse>, ApiError> {
    let operation = app.session_locks.lock(&id).await;
    let response = if let Some(session) = app.opened_session(&id).await {
        drop(operation);
        session.snapshot().await
    } else {
        app.disk_snapshot(&id, operation).await?
    };
    Ok(Json(response))
}

async fn resume(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    headers: HeaderMap,
    params: Result<Json<SessionResumeParams>, JsonRejection>,
) -> Result<Json<SessionResumeResponse>, ApiError> {
    let params = body(params)?;
    let owner = app.managed.owner(&headers)?;
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        // Opening settings and the returned snapshot belong to this resume.
        // Keep deletion from closing/moving its session halfway through.
        let operation = app.session_locks.lock(&id).await;
        if let Some(session) = app.opened_session(&id).await {
            let cwd = session.snapshot().await.session.settings.cwd;
            #[cfg(test)]
            app.pauses.hit(&id, "trust").await;
            let trust = folder_trust_operation(app.home.clone(), cwd, false).await?;
            session
                .set_folder_trust(trust.trusted && !params.restricted_mode)
                .await?;
            session
                .set_sandbox_disabled(params.sandbox_disabled, app.metadata().2.sandbox)
                .await?;
            let response = session.resume_snapshot().await?;
            if let Some(warning) = trust.warning {
                session.opening_warning(warning).await?;
            }
            managed::claim(&app, owner.as_ref(), &id, &session).await?;
            return Ok(Json(SessionResumeResponse {
                session: response.session,
                event_seq: response.event_seq,
            }));
        }
        drop(operation);
        let config = app.configuration().await?;
        let operation = app.session_locks.lock(&id).await;
        let response = if let Some(session) = app.opened_session(&id).await {
            let cwd = session.snapshot().await.session.settings.cwd;
            #[cfg(test)]
            app.pauses.hit(&id, "trust").await;
            let trust = folder_trust_operation(app.home.clone(), cwd, false).await?;
            session
                .set_folder_trust(trust.trusted && !params.restricted_mode)
                .await?;
            session
                .set_sandbox_disabled(params.sandbox_disabled, app.metadata().2.sandbox)
                .await?;
            let response = session.resume_snapshot().await?;
            if let Some(warning) = trust.warning {
                session.opening_warning(warning).await?;
            }
            managed::claim(&app, owner.as_ref(), &id, &session).await?;
            response
        } else {
            let cwd = app
                .disk_snapshot(&id, operation.clone())
                .await?
                .session
                .settings
                .cwd;
            #[cfg(test)]
            app.pauses.hit(&id, "trust").await;
            let trust = folder_trust_operation(app.home.clone(), cwd, false).await?;
            #[cfg(test)]
            app.pauses.hit(&id, "open").await;
            let (session, reasoning_warning) = Session::open_for_resume(
                app.store.clone(),
                id.clone(),
                app.runtime(
                    params.env,
                    &config,
                    !trust.trusted || params.restricted_mode,
                    params.sandbox_disabled,
                ),
                config.model.clone(),
                config.options.clone(),
            )
            .await?;
            #[cfg(test)]
            app.pauses.hit(&id, "configure").await;
            app.configure_session(&session, config.revision).await?;
            session
                .set_sandbox_disabled(params.sandbox_disabled, config.options.sandbox)
                .await?;
            let response = session.snapshot().await;
            if let Some(message) = reasoning_warning {
                session.opening_warning(message).await?;
            }
            if let Some(warning) = trust.warning {
                session.opening_warning(warning).await?;
            }
            app.register_session(id.clone(), session.clone()).await?;
            managed::claim(&app, owner.as_ref(), &id, &session).await?;
            response
        };
        Ok(Json(SessionResumeResponse {
            session: response.session,
            event_seq: response.event_seq,
        }))
    })
    .await
    .map_err(|_| ApiError::internal())?
}

async fn fork(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    headers: HeaderMap,
    params: Result<Json<SessionResumeParams>, JsonRejection>,
) -> Result<Json<SessionCreateResponse>, ApiError> {
    let params = body(params)?;
    let owner = app.managed.owner(&headers)?;
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        let operation = app.session_locks.lock(&id).await;
        let snapshot = if let Some(session) = app.opened_session(&id).await {
            session.fork_snapshot().await?
        } else {
            let store = app.store.clone();
            let operation = operation.clone();
            tokio::task::spawn_blocking(move || {
                let _operation = operation;
                canter_core::ForkSnapshot::read(&store, &id)
            })
            .await
            .map_err(|_| ApiError::internal())??
        };
        drop(operation);
        let config = app.configuration().await?;
        let trust =
            folder_trust_operation(app.home.clone(), snapshot.settings.cwd.clone(), false).await?;
        let restricted = params.restricted_mode
            || !trust.trusted
            || snapshot.settings.restricted_mode == Some(true);
        let id = crate::new_id();
        let _operation = app.session_locks.lock(&id).await;
        let (session, response) = snapshot
            .create(
                app.store.clone(),
                id.clone(),
                app.runtime(params.env, &config, restricted, params.sandbox_disabled),
                config.model.clone(),
                config.options.clone(),
            )
            .await?;
        app.configure_session(&session, config.revision).await?;
        if let Some(warning) = trust.warning {
            session.opening_warning(warning).await?;
        }
        app.register_session(id.clone(), session.clone()).await?;
        managed::claim(&app, owner.as_ref(), &id, &session).await?;
        Ok(Json(response))
    })
    .await
    .map_err(|_| ApiError::internal())?
}

async fn create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    params: Result<Json<SessionCreateParams>, JsonRejection>,
) -> Result<Json<SessionCreateResponse>, ApiError> {
    let params = body(params)?;
    let owner = app.managed.owner(&headers)?;
    // Registry insertion belongs to the accepted command, even if its HTTP caller disconnects.
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        if params.input.as_ref().is_some_and(|input| {
            input.is_empty()
                || input
                    .iter()
                    .any(|value| matches!(value, UserInput::Unknown))
        }) {
            return Err(ApiError::invalid());
        }
        let config = app.configuration().await?;
        if !std::path::Path::new(&params.cwd).is_dir() {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                &format!("Directory does not exist: {}", params.cwd),
            ));
        }
        let trust = folder_trust_operation(app.home.clone(), params.cwd.clone(), false).await?;
        let id = crate::new_id();
        let operation = app.session_locks.lock(&id).await;
        let model = if params.model.is_empty() {
            app.metadata().0
        } else {
            params.model
        };
        let reasoning_effort = params
            .reasoning_effort
            .unwrap_or_else(|| config.options.reasoning(&model).default);
        let settings = SessionSettings {
            tags: params.tags,
            additional_directories: params.additional_directories,
            plan_mode: false,
            pre_plan_approval_policy: None,
            plan_enabled: None,
            sandbox_enabled: None,
            restricted_mode: None,
            cwd: params.cwd,
            name: params.name,
            model,
            approval_policy: params.approval_policy,
            reasoning_effort,
        };
        app.validate_model(&settings.model)?;
        #[cfg(test)]
        app.pauses.hit(&id, "create").await;
        let (session, mut response) = Session::create(
            app.store.clone(),
            id.clone(),
            settings,
            app.runtime(
                params.env,
                &config,
                !trust.trusted || params.restricted_mode,
                params.sandbox_disabled,
            ),
            config.model.clone(),
            config.options.clone(),
        )
        .await?;
        #[cfg(test)]
        app.pauses.hit(&id, "configure").await;
        app.configure_session(&session, config.revision).await?;
        app.register_session(id.clone(), session.clone()).await?;
        managed::claim(&app, owner.as_ref(), &id, &session).await?;
        drop(operation);
        drop(config);
        if let Some(input) = params.input {
            session
                .input(SessionInputParams {
                    from_suggestion: false,
                    non_interactive: false,
                    approval_policy: None,
                    review: None,
                    input,
                    mode: InputMode::Queue,
                    client_name: params.client_name,
                })
                .await?;
            let snapshot = session.snapshot().await;
            response = SessionCreateResponse {
                session: snapshot.session,
                event_seq: snapshot.event_seq,
            };
        }
        // Append after the returned cursor so the opening client sees the warning.
        if let Some(warning) = trust.warning {
            session.opening_warning(warning).await?;
        }
        Ok(Json(response))
    })
    .await
    .map_err(|_| ApiError::internal())?
}

async fn input(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionInputParams>, JsonRejection>,
) -> Result<Json<SessionInputResponse>, ApiError> {
    let params = body(params)?;
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.input(params).await?))
}

async fn shell_command(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionShellCommandParams>, JsonRejection>,
) -> Result<Json<SessionShellCommandResponse>, ApiError> {
    let params = body(params)?;
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.shell_command(params).await?))
}

async fn shell_background(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<ShellBackgroundParams>, JsonRejection>,
) -> Result<Json<ShellBackgroundResponse>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .shell_background(body(params)?)
            .await?,
    ))
}
async fn background_commands(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<BackgroundCommandListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.background_commands().await?))
}
async fn language_servers(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<LanguageServerListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.language_servers().await?))
}
async fn diagnostics(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<DiagnosticListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.diagnostics().await))
}
async fn language_server_stop(
    State(app): State<Arc<App>>,
    ApiPath((id, server_id)): ApiPath<(String, String)>,
) -> Result<Json<LanguageServerListResponse>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .stop_language_server(server_id)
            .await?,
    ))
}
async fn language_server_retry(
    State(app): State<Arc<App>>,
    ApiPath((id, server_id)): ApiPath<(String, String)>,
) -> Result<Json<LanguageServerListResponse>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .retry_language_server(server_id)
            .await?,
    ))
}
async fn background_output(
    State(app): State<Arc<App>>,
    ApiPath((id, command_id)): ApiPath<(String, u64)>,
) -> Result<Json<BackgroundCommandOutput>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .background_output(command_id)
            .await?,
    ))
}
async fn background_stop(
    State(app): State<Arc<App>>,
    ApiPath((id, command_id)): ApiPath<(String, u64)>,
) -> Result<Json<BackgroundCommandListResponse>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .stop_background(Some(command_id))
            .await?,
    ))
}
async fn background_stop_all(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<BackgroundCommandListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.stop_background(None).await?))
}

async fn resolve(
    State(app): State<Arc<App>>,
    ApiPath((id, rid)): ApiPath<(String, String)>,
    params: Result<Json<RequestResolveParams>, JsonRejection>,
) -> Result<Json<RequestResolveResponse>, ApiError> {
    let params = body(params)?;
    Ok(Json(app.session(&id).await?.resolve(&rid, params).await?))
}

async fn interrupt(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionInterruptParams>, JsonRejection>,
) -> Result<Json<SessionInterruptResponse>, ApiError> {
    Ok(Json(
        app.session(&id).await?.interrupt(body(params)?).await?,
    ))
}

async fn compact(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Option<Json<SessionCompactParams>>, JsonRejection>,
) -> Result<Json<SessionCompactResponse>, ApiError> {
    params.map_err(|_| ApiError::invalid())?;
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.compact().await?))
}

async fn update(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionUpdateParams>, JsonRejection>,
) -> Result<Json<SessionUpdateResponse>, ApiError> {
    let params = body(params)?;
    if let Some(model) = &params.model {
        app.validate_model(model)?;
    }
    let draining = app.draining.read().await;
    if *draining {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    Ok(Json(app.session(&id).await?.update(params).await?))
}

async fn close(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Option<Json<SessionCloseParams>>, JsonRejection>,
) -> Result<Json<SessionCloseResponse>, ApiError> {
    params.map_err(|_| ApiError::invalid())?;
    tokio::spawn(async move {
        let session = app.session(&id).await?;
        app.close_session(id, session).await.map(Json)
    })
    .await
    .map_err(|_| ApiError::internal())?
}

impl App {
    async fn close_session(
        &self,
        id: String,
        session: Session,
    ) -> Result<SessionCloseResponse, ApiError> {
        let response = session.close().await;
        if response.is_err() {
            // A final journal append can fail before Core releases its writer; a second close releases it.
            let _ = session.close().await;
        }
        #[cfg(test)]
        self.pauses.hit(&id, "close_remove").await;
        let _operation = self.session_locks.lock(&id).await;
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(&id)
            .is_some_and(|current| current.same_instance(&session))
        {
            sessions.remove(&id);
        }
        self.registry_changed.notify_waiters();
        Ok(response?)
    }
}

async fn trash(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    // Finish the accepted command even if the HTTP caller disconnects.
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        let operation = app.session_locks.lock(&id).await;
        let family = agent_sessions::Family::lock(&app, &id, operation.clone()).await?;
        if family.len() > 1 {
            family.trash(&app).await?;
            return Ok(StatusCode::NO_CONTENT);
        }
        let source = app.store.session_dir(&id).map_err(CoreError::from)?;
        let session = app.opened_session(&id).await;
        if session.is_none() {
            app.disk_snapshot(&id, operation.clone()).await?;
        }
        let trash = app.home.join("trash");
        let target = trash.join(&id);
        #[cfg(test)]
        app.pauses.hit(&id, "trash_prepare").await;
        let target = tokio::task::spawn_blocking(move || -> Result<PathBuf, ApiError> {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            match std::fs::symlink_metadata(&target) {
                Ok(_) => return Err(ApiError::new(ErrorCode::Conflict, "The trash destination already exists; move or remove it manually before deleting this session.")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(_) => return Err(ApiError::internal()),
            }
            std::fs::DirBuilder::new().mode(0o700).recursive(true).create(&trash)
                .map_err(|_| ApiError::internal())?;
            std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| ApiError::internal())?;
            Ok(target)
        }).await.map_err(|_| ApiError::internal())??;
        if let Some(session) = session {
            #[cfg(test)]
            app.pauses.hit(&id, "trash_close").await;
            session.close_for_trash().await?;
        }
        app.sessions.lock().await.remove(&id);
        #[cfg(test)]
        app.pauses.hit(&id, "trash_move").await;
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            std::fs::rename(source, target).map_err(|error| canter_store::StoreError::from(error).into())
        }).await.map_err(|_| ApiError::internal())??;
        app.registry_changed.notify_waiters();
        Ok(StatusCode::NO_CONTENT)
    }).await.map_err(|_| ApiError::internal())?
}

struct EventReader {
    app: Arc<App>,
    id: String,
    cursor: u64,
    pending: VecDeque<Event>,
    subscription: Option<Subscription>,
    _client: Option<canter_core::ClientConnection>,
    observe: bool,
}

impl EventReader {
    async fn new(app: Arc<App>, id: String, cursor: u64, observe: bool) -> Result<Self, ApiError> {
        let operation = app.session_locks.lock(&id).await;
        let (pending, subscription, client) = if let Some(session) = app.opened_session(&id).await {
            // Attach under the per-session lock, so trash cannot move a session
            // between catch-up and registering this external client.
            #[cfg(test)]
            app.pauses.hit(&id, "subscribe").await;
            let subscription = session.subscribe(cursor).await?;
            (
                VecDeque::new(),
                Some(subscription),
                if observe {
                    None
                } else {
                    session.attach_client().await
                },
            )
        } else {
            let events = app.disk_events(&id, 0, Some(operation.clone())).await?;
            let last = events
                .last()
                .ok_or_else(|| ApiError::new(ErrorCode::SessionNotFound, "session not found"))?
                .seq;
            if cursor > last {
                return Err(ApiError::invalid());
            }
            (
                events
                    .into_iter()
                    .filter(|event| event.seq > cursor)
                    .collect(),
                None,
                None,
            )
        };
        drop(operation);
        Ok(Self {
            app,
            id,
            cursor,
            pending,
            subscription,
            _client: client,
            observe,
        })
    }

    async fn recv(&mut self) -> Result<Option<Event>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.cursor = event.seq;
                return Ok(Some(event));
            }
            if let Some(subscription) = self.subscription.as_mut() {
                return subscription.recv().await.map_err(Into::into);
            }
            let changed = self.app.registry_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            // Resume cannot start this session's writer mid-read.
            let operation = self.app.session_locks.lock(&self.id).await;
            if let Some(session) = self.app.opened_session(&self.id).await {
                #[cfg(test)]
                self.app.pauses.hit(&self.id, "subscribe").await;
                self.subscription = Some(session.subscribe(self.cursor).await?);
                if !self.observe {
                    self._client = session.attach_client().await;
                }
                drop(operation);
            } else {
                self.pending = self
                    .app
                    .disk_events(&self.id, self.cursor, Some(operation.clone()))
                    .await?
                    .into();
                drop(operation);
            }
            if self.subscription.is_none() && self.pending.is_empty() {
                changed.await;
            }
        }
    }
}

async fn events(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    params: Result<Query<EventsParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    let last = headers
        .get("last-event-id")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::invalid())?;
    let after = resume_after(params.after, last).map_err(ApiError)?;
    let mut subscription =
        EventReader::new(app.clone(), params.session, after, params.observe).await?;
    let stream = async_stream::stream! {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            // Keep one receive alive across heartbeats, including its disk catch-up or registry wait.
            let next = subscription.recv();
            tokio::pin!(next);
            loop {
                tokio::select! {
                    biased;
                    _ = app.stopped.cancelled() => return,
                    event = &mut next => {
                        match event {
                            Ok(Some(event)) => match sse_frame(&event) {
                                Ok(frame) => yield Ok::<_, Infallible>(frame),
                                Err(_) => return,
                            },
                            _ => return,
                        }
                        break;
                    }
                    _ = heartbeat.tick() => yield Ok(": heartbeat\n\n".to_string()),
                }
            }
        }
    };
    Ok((
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn mcp(State(app): State<Arc<App>>) -> Json<McpListResponse> {
    Json(McpListResponse {
        data: if let Some(reload) = &app.reload {
            reload.mcp_status()
        } else {
            app.mcp.status()
        },
    })
}

async fn rewind_list(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<RewindListResponse>, ApiError> {
    Ok(Json(app.session(&id).await?.rewind_list().await?))
}

async fn rewind_preview(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionRewindParams>, JsonRejection>,
) -> Result<Json<RewindPreviewResponse>, ApiError> {
    Ok(Json(
        app.session(&id)
            .await?
            .rewind_preview(body(params)?)
            .await?,
    ))
}

async fn rewind(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionRewindParams>, JsonRejection>,
) -> Result<Json<SessionRewindResponse>, ApiError> {
    let params = body(params)?;
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        let operation = app.session_locks.lock(&id).await;
        if app.opened_session(&id).await.is_none() {
            app.disk_snapshot(&id, operation.clone()).await?;
        }
        let family = agent_sessions::Family::lock(&app, &id, operation).await?;
        Ok(Json(family.rewind(&app, params).await?))
    })
    .await
    .map_err(|_| ApiError::internal())?
}
