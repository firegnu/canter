use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

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

pub(crate) struct App {
    pub home: PathBuf,
    pub store: Arc<Store>,
    pub model: Arc<dyn Model>,
    pub default_model: String,
    pub models: Vec<String>,
    pub options: CoreOptions,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub registry_changed: Notify,
    pub draining: RwLock<bool>,
    pub stopped: CancellationToken,
}

impl App {
    fn validate_model(&self, model: &str) -> Result<(), ApiError> {
        if self.models.iter().any(|available| available == model) {
            Ok(())
        } else {
            Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "model is not in the configured model list",
            ))
        }
    }

    fn runtime(&self, environment: std::collections::BTreeMap<String, String>) -> RuntimeContext {
        RuntimeContext {
            environment,
            protected_paths: vec![self.home.clone(), self.home.join("config.json")],
        }
    }

    async fn session(&self, id: &str) -> Result<Session, ApiError> {
        let sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(id) {
            return Ok(session.clone());
        }
        self.disk_snapshot(id).await?;
        Err(ApiError::new(
            ErrorCode::SessionNotResumed,
            "resume the session first: POST /v1/sessions/{id}/resume with env again",
        ))
    }

    async fn disk_events(&self, id: &str, after: u64) -> Result<Vec<Event>, ApiError> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || store.events_after(&id, after))
            .await
            .map_err(|_| ApiError::internal())?
            .map_err(|error| CoreError::from(error).into())
    }

    async fn disk_snapshot(&self, id: &str) -> Result<SessionReadResponse, ApiError> {
        let store = self.store.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let mut snapshot = SessionSnapshot::empty(&id);
            for event in store.events_after(&id, 0).map_err(CoreError::from)? {
                apply_event_mut(&mut snapshot, &event).map_err(|_| ApiError::internal())?;
            }
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
    Router::new()
        .route(routes::INFO, get(info))
        .route(routes::MODELS, get(models))
        .route(routes::SESSIONS, post(create).get(list))
        .route(routes::SESSION, get(read).patch(update).delete(close))
        .route(routes::RESUME, post(resume))
        .route(routes::INPUT, post(input))
        .route(routes::INTERRUPT, post(interrupt))
        .route(routes::COMPACT, post(compact))
        .route(routes::REQUEST, post(resolve))
        .route(routes::EVENTS, get(events))
        .fallback(|| async { ApiError::invalid() })
        .method_not_allowed_fallback(|| async { ApiError::invalid() })
        .with_state(app)
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
        server_version: crate::VERSION.into(),
        protocol_version: PROTOCOL_VERSION,
        sessions,
    }))
}

async fn models(State(app): State<Arc<App>>) -> Json<ModelListResponse> {
    Json(ModelListResponse {
        data: app
            .models
            .iter()
            .map(|model| canter_protocol::Model {
                id: model.clone(),
                model: model.clone(),
                display_name: model.clone(),
                is_default: model == &app.default_model,
            })
            .collect(),
        next_cursor: None,
    })
}

async fn list(state: State<Arc<App>>) -> Result<Json<SessionListResponse>, ApiError> {
    Ok(Json(SessionListResponse {
        data: info(state).await?.0.sessions,
    }))
}

async fn read(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<SessionReadResponse>, ApiError> {
    let sessions = app.sessions.lock().await;
    let response = if let Some(session) = sessions.get(&id) {
        session.snapshot().await
    } else {
        app.disk_snapshot(&id).await?
    };
    Ok(Json(response))
}

async fn resume(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Json<SessionResumeParams>, JsonRejection>,
) -> Result<Json<SessionResumeResponse>, ApiError> {
    let params = body(params)?;
    tokio::spawn(async move {
        let draining = app.draining.read().await;
        if *draining {
            return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
        }
        let mut sessions = app.sessions.lock().await;
        let response = if let Some(session) = sessions.get(&id) {
            session.snapshot().await
        } else {
            let (session, response) = Session::open(
                app.store.clone(),
                id.clone(),
                app.runtime(params.env),
                app.model.clone(),
                app.options.clone(),
            )
            .await?;
            sessions.insert(id, session);
            app.registry_changed.notify_waiters();
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

async fn create(
    State(app): State<Arc<App>>,
    params: Result<Json<SessionCreateParams>, JsonRejection>,
) -> Result<Json<SessionCreateResponse>, ApiError> {
    let params = body(params)?;
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
        let mut sessions = app.sessions.lock().await;
        let id = crate::new_id();
        let settings = SessionSettings {
            cwd: params.cwd,
            name: params.name,
            model: if params.model.is_empty() {
                app.default_model.clone()
            } else {
                params.model
            },
            approval_policy: params.approval_policy,
            reasoning_effort: params.reasoning_effort.unwrap_or_default(),
        };
        app.validate_model(&settings.model)?;
        let (session, mut response) = Session::create(
            app.store.clone(),
            id.clone(),
            settings,
            app.runtime(params.env),
            app.model.clone(),
            app.options.clone(),
        )
        .await?;
        sessions.insert(id, session.clone());
        app.registry_changed.notify_waiters();
        if let Some(input) = params.input {
            session
                .input(SessionInputParams {
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
        let mut sessions = app.sessions.lock().await;
        let Some(session) = sessions.get(&id).cloned() else {
            app.disk_snapshot(&id).await?;
            return Err(ApiError::new(
                ErrorCode::SessionNotResumed,
                "resume the session first: POST /v1/sessions/{id}/resume with env again",
            ));
        };
        let response = session.close().await;
        if response.is_err() {
            // A final journal append can fail before Core releases its writer; a second close releases it.
            let _ = session.close().await;
        }
        sessions.remove(&id);
        app.registry_changed.notify_waiters();
        Ok(Json(response?))
    })
    .await
    .map_err(|_| ApiError::internal())?
}

struct EventReader {
    app: Arc<App>,
    id: String,
    cursor: u64,
    pending: VecDeque<Event>,
    subscription: Option<Subscription>,
}

impl EventReader {
    async fn new(app: Arc<App>, id: String, cursor: u64) -> Result<Self, ApiError> {
        let sessions = app.sessions.lock().await;
        let (pending, subscription) = if let Some(session) = sessions.get(&id) {
            (VecDeque::new(), Some(session.subscribe(cursor).await?))
        } else {
            let events = app.disk_events(&id, 0).await?;
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
            )
        };
        drop(sessions);
        Ok(Self {
            app,
            id,
            cursor,
            pending,
            subscription,
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
            // Keep the registry locked while reading an unopened session: resume cannot start a writer mid-read.
            let sessions = self.app.sessions.lock().await;
            if let Some(session) = sessions.get(&self.id) {
                self.subscription = Some(session.subscribe(self.cursor).await?);
            } else {
                self.pending = self.app.disk_events(&self.id, self.cursor).await?.into();
            }
            drop(sessions);
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
    let mut subscription = EventReader::new(app.clone(), params.session, after).await?;
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
