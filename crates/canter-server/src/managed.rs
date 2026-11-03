//! A terminal's closing responsibility, separate from ordinary session subscribers.
use super::*;
use std::sync::{Mutex as StdMutex, Weak};

const GRACE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(crate) struct Clients(StdMutex<Registry>);

#[derive(Default)]
struct Registry {
    draining: bool,
    owners: HashMap<String, Arc<StdMutex<Owner>>>,
}

#[derive(Default)]
pub(super) struct Owner {
    connections: usize,
    generation: u64,
    closed: bool,
    sessions: HashMap<String, Session>,
}

fn ended() -> ApiError {
    ApiError::new(ErrorCode::Conflict, "managed client has exited")
}

impl Clients {
    pub(super) fn owner(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<Arc<StdMutex<Owner>>>, ApiError> {
        let Some(id) = headers.get(routes::MANAGED_CLIENT_HEADER) else {
            return Ok(None);
        };
        let id = id.to_str().map_err(|_| ApiError::invalid())?;
        let registry = self.0.lock().unwrap();
        let owner = registry.owners.get(id).ok_or_else(ended)?;
        if registry.draining || owner.lock().unwrap().closed {
            return Err(ended());
        }
        Ok(Some(owner.clone()))
    }

    pub(super) fn disarm(&self) {
        self.0.lock().unwrap().draining = true;
    }
}

pub(super) fn claim(
    app: &Arc<App>,
    owner: Option<&Arc<StdMutex<Owner>>>,
    id: &str,
    session: &Session,
) -> Result<(), ApiError> {
    let Some(owner) = owner else {
        return Ok(());
    };
    let mut owner = owner.lock().unwrap();
    if owner.closed {
        // Opening was accepted before the owner exited. Finish its cleanup too.
        close_sessions(app, [(id.to_owned(), session.clone())]);
        return Err(ended());
    }
    owner.sessions.insert(id.to_owned(), session.clone());
    Ok(())
}

fn close_sessions(app: &Arc<App>, sessions: impl IntoIterator<Item = (String, Session)>) {
    for (id, session) in sessions {
        let app = app.clone();
        tokio::spawn(async move {
            if app.close_session(id, session).await.is_err() {
                eprintln!("Warning: could not close a managed session; resume it to recover");
            }
        });
    }
}

// Registry -> owner is the only lock order. Mark closed before spawning any
// work, so a reconnect cannot resurrect an expired owner or cancel cleanup.
fn finish(app: &Arc<App>, owner: &StdMutex<Owner>, generation: Option<u64>) {
    let registry = app.managed.0.lock().unwrap();
    if registry.draining {
        return;
    }
    let mut owner = owner.lock().unwrap();
    if owner.closed
        || generation
            .is_some_and(|generation| owner.connections != 0 || owner.generation != generation)
    {
        return;
    }
    owner.closed = true;
    close_sessions(app, std::mem::take(&mut owner.sessions));
}

struct Connection {
    app: Weak<App>,
    owner: Arc<StdMutex<Owner>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let generation = {
            let mut owner = self.owner.lock().unwrap();
            owner.connections -= 1;
            if owner.connections != 0 || owner.closed {
                return;
            }
            owner.generation += 1;
            owner.generation
        };
        let app = self.app.clone();
        let owner = self.owner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(GRACE).await;
            if let Some(app) = app.upgrade() {
                finish(&app, &owner, Some(generation));
            }
        });
    }
}

pub(super) async fn connect(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, ApiError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError::invalid());
    }
    let connection = {
        let mut registry = app.managed.0.lock().unwrap();
        if registry.draining {
            return Err(ended());
        }
        let owner = registry.owners.entry(id).or_default().clone();
        {
            let mut state = owner.lock().unwrap();
            if state.closed {
                return Err(ended());
            }
            state.connections += 1;
            state.generation += 1;
        }
        Connection {
            app: Arc::downgrade(&app),
            owner,
        }
    };
    let stream = async_stream::stream! {
        let _connection = connection;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = app.stopped.cancelled() => return,
                _ = heartbeat.tick() => yield Ok::<_, Infallible>(": heartbeat\n\n"),
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

pub(super) async fn close(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    let owner = app.managed.0.lock().unwrap().owners.get(&id).cloned();
    if let Some(owner) = owner {
        finish(&app, &owner, None);
    }
    Ok(StatusCode::ACCEPTED)
}
