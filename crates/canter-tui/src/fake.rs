//! A fake server for tests and demos. It speaks the same HTTP + SSE protocol
//! over a unix socket, but runs no model and no tools: it replays a script of
//! events. Replay starts when the first client subscribes. After the script,
//! each sent message gets a short canned reply.

use std::convert::Infallible;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use canter_protocol::{
    ErrorCode, ErrorResponse, Event, EventPayload, EventsParams, Item, ItemDelta,
    SessionInputParams, SessionInputResponse, SessionReadResponse, SessionSnapshot, SessionStatus,
    Turn, TurnStatus, UserInput, apply_event_mut, resume_after, routes, sse_frame,
};
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::now_ms;

/// The bundled script: one session doing a small job, covering every kind of
/// item the conversation screen draws.
pub const DEMO_SCRIPT: &str = include_str!("../fixtures/demo-session.jsonl");

/// How fast the script plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// As fast as possible, at the script's own times (tests).
    Instant,
    /// Roughly like a real session, for people watching.
    Demo,
}

#[derive(Debug, thiserror::Error)]
pub enum FakeError {
    #[error("script line {line}: {source}")]
    Parse {
        line: usize,
        source: serde_json::Error,
    },
    #[error("script event {seq} does not apply: {source}")]
    Replay {
        seq: u64,
        source: canter_protocol::ReduceError,
    },
    #[error("script is empty")]
    Empty,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Reads a script: one protocol event per line (JSON Lines); blank lines are skipped.
pub fn parse_script(text: &str) -> Result<Vec<Event>, FakeError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| FakeError::Parse {
                line: index + 1,
                source,
            })
        })
        .collect()
}

pub struct FakeServer {
    shared: Arc<Shared>,
    server: JoinHandle<()>,
}

struct Shared {
    session_id: String,
    log: Mutex<Log>,
    /// The last sequence written; subscribers wait on changes.
    changed: watch::Sender<u64>,
    stopped: watch::Sender<bool>,
    /// Script events still to play, taken by the first subscriber.
    script: Mutex<Option<Vec<Event>>>,
    pace: Pace,
    replay_done: AtomicBool,
    replay: Mutex<Option<JoinHandle<()>>>,
    echo_turns: AtomicU64,
}

struct Log {
    events: Vec<Event>,
    snapshot: SessionSnapshot,
}

impl FakeServer {
    /// Listens on `socket` and serves the session in `script`. The first
    /// `preload` events are there before anyone connects (they make the
    /// snapshot); the rest play once a client subscribes.
    pub async fn start(
        socket: &Path,
        script: Vec<Event>,
        preload: usize,
        pace: Pace,
    ) -> Result<Self, FakeError> {
        let session_id = script.first().ok_or(FakeError::Empty)?.session_id.clone();
        // Check the whole script up front so replay cannot fail halfway.
        let mut check = SessionSnapshot::empty(&session_id);
        for (index, event) in script.iter().enumerate() {
            let mut event = event.clone();
            event.seq = index as u64 + 1;
            event.session_id = session_id.clone();
            apply_event_mut(&mut check, &event).map_err(|source| FakeError::Replay {
                seq: event.seq,
                source,
            })?;
        }

        let preload = preload.min(script.len());
        let mut rest = script;
        let first: Vec<Event> = rest.drain(..preload).collect();
        let shared = Arc::new(Shared {
            log: Mutex::new(Log {
                events: Vec::new(),
                snapshot: SessionSnapshot::empty(&session_id),
            }),
            session_id,
            changed: watch::channel(0).0,
            stopped: watch::channel(false).0,
            script: Mutex::new(Some(rest)),
            pace,
            replay_done: AtomicBool::new(false),
            replay: Mutex::new(None),
            echo_turns: AtomicU64::new(0),
        });
        let first: Vec<_> = first
            .into_iter()
            .map(|event| {
                let timestamp = shared.timestamp(&event);
                (event.payload, timestamp)
            })
            .collect();
        shared.append(first);

        let app = Router::new()
            .route(routes::SESSION, get(read_session))
            // Nothing to open: resuming returns the snapshot, like reading it.
            .route(routes::RESUME, post(read_session))
            .route(routes::INPUT, post(send_input))
            .route(routes::EVENTS, get(events))
            .with_state(shared.clone());
        let listener = UnixListener::bind(socket)?;
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { shared, server })
    }
}

/// Dropping the server stops it and ends open event streams.
impl Drop for FakeServer {
    fn drop(&mut self) {
        self.server.abort();
        if let Some(replay) = self.shared.replay.lock().expect("replay lock").take() {
            replay.abort();
        }
        self.shared.stopped.send_replace(true);
    }
}

impl Shared {
    /// Appends events with fresh sequence numbers; returns the sequence of
    /// the first one.
    fn append(&self, events: impl IntoIterator<Item = (EventPayload, i64)>) -> u64 {
        let mut log = self.log.lock().expect("log lock");
        let first = log.snapshot.last_event_seq + 1;
        for (payload, timestamp) in events {
            let event = Event {
                seq: log.snapshot.last_event_seq + 1,
                session_id: self.session_id.clone(),
                timestamp,
                payload,
            };
            apply_event_mut(&mut log.snapshot, &event).expect("checked when the server started");
            log.events.push(event);
        }
        self.changed.send_replace(log.snapshot.last_event_seq);
        first
    }

    /// When a script event happens: as the script says when replaying for
    /// tests, so times on screen are the same on every run; now when people
    /// watch, so the status line counts from now.
    fn timestamp(&self, event: &Event) -> i64 {
        match self.pace {
            Pace::Instant => event.timestamp,
            Pace::Demo => now_ms(),
        }
    }

    fn start_replay(self: &Arc<Self>) {
        let Some(script) = self.script.lock().expect("script lock").take() else {
            return;
        };
        let shared = self.clone();
        let task = tokio::spawn(async move {
            for event in script {
                let pause = delay(shared.pace, &event.payload);
                if pause.is_zero() {
                    tokio::task::yield_now().await;
                } else {
                    tokio::time::sleep(pause).await;
                }
                let timestamp = shared.timestamp(&event);
                shared.append([(event.payload, timestamp)]);
            }
            shared.replay_done.store(true, Ordering::SeqCst);
        });
        *self.replay.lock().expect("replay lock") = Some(task);
    }

    /// The 404 response when `id` is not the scripted session.
    fn unknown_session(&self, id: &str) -> Option<Response> {
        (id != self.session_id).then(|| {
            error(
                StatusCode::NOT_FOUND,
                ErrorCode::SessionNotFound,
                format!("no session {id}"),
            )
        })
    }
}

async fn read_session(State(shared): State<Arc<Shared>>, UrlPath(id): UrlPath<String>) -> Response {
    if let Some(response) = shared.unknown_session(&id) {
        return response;
    }
    let log = shared.log.lock().expect("log lock");
    axum::Json(SessionReadResponse {
        session: log.snapshot.clone(),
        event_seq: log.snapshot.last_event_seq,
    })
    .into_response()
}

async fn send_input(
    State(shared): State<Arc<Shared>>,
    UrlPath(id): UrlPath<String>,
    axum::Json(params): axum::Json<SessionInputParams>,
) -> Response {
    if let Some(response) = shared.unknown_session(&id) {
        return response;
    }
    let idle = shared.log.lock().expect("log lock").snapshot.status == SessionStatus::Idle;
    if !shared.replay_done.load(Ordering::SeqCst) || !idle {
        return error(
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            "the fake server is still replaying; send after it finishes".into(),
        );
    }
    let number = shared.echo_turns.fetch_add(1, Ordering::SeqCst) + 1;
    let turn_id = format!("echo-{number}");
    let item_id = format!("{turn_id}-reply");
    let said: Vec<&str> = params
        .input
        .iter()
        .filter_map(|part| match part {
            UserInput::Text { text } => Some(text.as_str()),
            UserInput::LocalImage { .. } | UserInput::Unknown => None,
        })
        .collect();
    let reply = format!(
        "This is a fake server; it only replays scripted events. Received: {}",
        said.join(" ")
    );
    let turn = |status| Turn {
        review: params.review.clone(),
        id: turn_id.clone(),
        items: Vec::new(),
        status,
        error: None,
        input: params.input.clone(),
        client_name: params.client_name.clone(),
        usage: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    let message = |text: &str| Item::AgentMessage {
        id: item_id.clone(),
        text: text.to_owned(),
    };
    let reply_events = [
        EventPayload::TurnStarted {
            turn: turn(TurnStatus::InProgress),
            input_id: None,
        },
        EventPayload::SessionStatus {
            status: SessionStatus::Working,
            close_reason: None,
        },
        EventPayload::ItemStarted {
            turn_id: turn_id.clone(),
            item: message(""),
        },
        EventPayload::ItemDelta {
            turn_id: turn_id.clone(),
            item_id: item_id.clone(),
            delta: ItemDelta::AgentMessage {
                delta: reply.clone(),
            },
        },
        EventPayload::ItemCompleted {
            turn_id: turn_id.clone(),
            item: message(&reply),
        },
        EventPayload::TurnCompleted {
            turn: turn(TurnStatus::Completed),
        },
        EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        },
    ];
    // The whole answer at one moment: the turn takes no time.
    let now = now_ms();
    let seq = shared.append(reply_events.map(|payload| (payload, now)));
    axum::Json(SessionInputResponse {
        event_seq: seq,
        turn_id: Some(turn_id),
    })
    .into_response()
}

async fn events(
    State(shared): State<Arc<Shared>>,
    Query(params): Query<EventsParams>,
    headers: HeaderMap,
) -> Response {
    let last_event_id = headers
        .get("last-event-id")
        .map(|value| value.to_str().unwrap_or_default());
    let after = match resume_after(params.after, last_event_id) {
        Ok(after) => after,
        Err(body) => return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response(),
    };
    if let Some(response) = shared.unknown_session(&params.session) {
        return response;
    }
    shared.start_replay();
    let changed = shared.changed.subscribe();
    let stopped = shared.stopped.subscribe();
    let stream = futures_util::stream::unfold(
        (shared, changed, stopped, after),
        |(shared, mut changed, mut stopped, cursor)| async move {
            loop {
                changed.borrow_and_update();
                let frames: Vec<String> = {
                    let log = shared.log.lock().expect("log lock");
                    log.events
                        .iter()
                        .filter(|event| event.seq > cursor)
                        .map(|event| sse_frame(event).expect("events serialize"))
                        .collect()
                };
                if !frames.is_empty() {
                    let cursor = cursor + frames.len() as u64;
                    let chunk = Ok::<_, Infallible>(Bytes::from(frames.concat()));
                    return Some((chunk, (shared, changed, stopped, cursor)));
                }
                tokio::select! {
                    result = changed.changed() => if result.is_err() { return None },
                    _ = stopped.wait_for(|stopped| *stopped) => return None,
                }
            }
        },
    );
    (
        [
            (CONTENT_TYPE, "text/event-stream"),
            (CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

fn error(status: StatusCode, code: ErrorCode, message: String) -> Response {
    (status, axum::Json(ErrorResponse { code, message })).into_response()
}

fn delay(pace: Pace, payload: &EventPayload) -> Duration {
    if pace == Pace::Instant {
        return Duration::ZERO;
    }
    let ms = match payload {
        EventPayload::ItemDelta {
            delta: ItemDelta::CommandExecution { .. },
            ..
        } => 300,
        EventPayload::ItemDelta { .. } => 50,
        EventPayload::ItemStarted { .. } | EventPayload::ItemCompleted { .. } => 400,
        _ => 150,
    };
    Duration::from_millis(ms)
}
