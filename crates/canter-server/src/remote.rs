use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use canter_protocol::{DoctorCategory, DoctorCheck, DoctorStatus, ErrorCode, ErrorResponse};
use futures_util::StreamExt;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::remote_tokens;

pub(crate) struct Remote {
    home: PathBuf,
    listening: Result<Vec<SocketAddr>, String>,
    tokens: Mutex<Tokens>,
}

#[derive(Default)]
struct Tokens {
    entries: Vec<(remote_tokens::Record, CancellationToken)>,
    error: Option<String>,
}

fn addresses(value: &serde_json::Value) -> Result<Vec<SocketAddr>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        listen: Vec<SocketAddr>,
    }
    let settings: Settings = serde_json::from_value(value.clone())
        .map_err(|_| "remote.listen must be a list of IP:port addresses".to_owned())?;
    if settings.listen.iter().any(|addr| !addr.ip().is_loopback()) {
        return Err("remote.listen accepts loopback addresses only; put an HTTPS proxy such as Caddy in front of it".into());
    }
    Ok(settings.listen)
}

impl Remote {
    pub async fn bind(
        home: PathBuf,
        settings: &serde_json::Value,
    ) -> (Arc<Self>, Vec<TcpListener>) {
        let mut listeners = Vec::new();
        let listening = match addresses(settings) {
            Ok(addresses) => {
                let mut bound = Vec::new();
                let mut error = None;
                for address in addresses {
                    match TcpListener::bind(address).await {
                        Ok(listener) => match listener.local_addr() {
                            Ok(address) => {
                                bound.push(address);
                                listeners.push(listener);
                            }
                            Err(_) => {
                                error = Some("cannot determine TCP listener address".into());
                                break;
                            }
                        },
                        Err(_) => {
                            error = Some(format!(
                                "cannot listen on {address}; the port may already be in use"
                            ));
                            break;
                        }
                    }
                }
                if let Some(error) = error {
                    Err(error)
                } else {
                    Ok(bound)
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &listening {
            listeners.clear();
            eprintln!("TCP disabled: {error}");
        }
        (
            Arc::new(Self {
                home,
                listening,
                tokens: Mutex::default(),
            }),
            listeners,
        )
    }

    // Always reread under the same lock, including after atomic replacement with
    // an unchanged timestamp. A failed read revokes all previously accepted tokens.
    fn refresh(&self) -> std::sync::MutexGuard<'_, Tokens> {
        let mut state = self.tokens.lock().unwrap();
        let (records, error) = match remote_tokens::read(&self.home) {
            Ok(records) => (records, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        if error != state.error
            && let Some(error) = &error
        {
            eprintln!("TCP authentication denies all requests: {error}");
        }
        state.error = error;
        state.entries.retain(|(old, stopped)| {
            let keep = records
                .iter()
                .any(|new| new.name == old.name && new.sha256 == old.sha256);
            if !keep {
                stopped.cancel();
            }
            keep
        });
        for record in records {
            if !state
                .entries
                .iter()
                .any(|(old, _)| old.sha256 == record.sha256)
            {
                state.entries.push((record, CancellationToken::new()));
            }
        }
        state
    }

    pub async fn watch(self: Arc<Self>, stopped: CancellationToken) {
        if !matches!(&self.listening, Ok(addresses) if !addresses.is_empty()) {
            return;
        }
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                biased;
                _ = stopped.cancelled() => return,
                _ = interval.tick() => {
                    let remote = self.clone();
                    let _ = tokio::task::spawn_blocking(move || { drop(remote.refresh()); }).await;
                }
            }
        }
    }

    pub fn router(self: &Arc<Self>, app: Router) -> Router {
        // Wrap the entire router, including unknown paths and unsupported methods.
        Router::new()
            .fallback_service(app)
            .layer(middleware::from_fn_with_state(self.clone(), authenticate))
    }

    pub async fn checks(&self) -> Vec<DoctorCheck> {
        let (status, reason) = match &self.listening {
            Ok(addresses) if addresses.is_empty() => (
                DoctorStatus::Skip,
                "TCP disabled: remote.listen is not configured.".into(),
            ),
            Ok(addresses) => (
                DoctorStatus::Ok,
                format!(
                    "Listening on {}",
                    addresses
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            Err(error) => (DoctorStatus::Fail, format!("TCP disabled: {error}")),
        };
        let mut checks = vec![DoctorCheck {
            id: "remote.listen".into(),
            name: "TCP listener".into(),
            category: DoctorCategory::Configuration,
            status,
            reason: Some(reason),
            fix: (status == DoctorStatus::Fail)
                .then(|| "Fix remote.listen and run canter serve --replace.".into()),
        }];
        checks.push(token_check(self.home.clone()).await);
        checks
    }
}

pub(crate) async fn offline_checks(home: PathBuf) -> Vec<DoctorCheck> {
    vec![
        DoctorCheck {
            id: "remote.listen".into(),
            name: "TCP listener".into(),
            category: DoctorCategory::Configuration,
            status: DoctorStatus::Skip,
            reason: Some("TCP listener status is unavailable without a running server.".into()),
            fix: None,
        },
        token_check(home).await,
    ]
}

async fn token_check(home: PathBuf) -> DoctorCheck {
    let result = tokio::task::spawn_blocking(move || remote_tokens::read(&home)).await;
    let (status, reason) = match result {
        Ok(Ok(records)) => (DoctorStatus::Ok, format!("{} tokens", records.len())),
        Ok(Err(error)) => (
            DoctorStatus::Fail,
            format!("TCP authentication denies all requests: {error}"),
        ),
        Err(_) => (
            DoctorStatus::Fail,
            "TCP authentication could not read remote-tokens.json".into(),
        ),
    };
    DoctorCheck {
        id: "remote.tokens".into(),
        name: "Remote tokens".into(),
        category: DoctorCategory::Authentication,
        status,
        reason: Some(reason),
        fix: (status == DoctorStatus::Fail)
            .then(|| "Fix remote-tokens.json and its permissions on the server machine.".into()),
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(ErrorResponse {
            code: ErrorCode::Unauthorized,
            message: "unauthorized".into(),
        }),
    )
        .into_response()
}

async fn authenticate(State(remote): State<Arc<Remote>>, request: Request, next: Next) -> Response {
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let token = headers
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
        .map(|(_, token)| token);
    let Some(token) = token.filter(|_| headers.next().is_none()) else {
        return unauthorized();
    };
    let hash = remote_tokens::hash(token);
    let authorized = tokio::task::spawn_blocking(move || {
        let state = remote.refresh();
        let mut client = None;
        for (record, stopped) in &state.entries {
            if bool::from(record.sha256.as_bytes().ct_eq(hash.as_bytes())) {
                client = Some((record.name.clone(), stopped.clone()));
            }
        }
        client
    })
    .await;
    let Ok(Some((client, revoked))) = authorized else {
        return unauthorized();
    };
    eprintln!("TCP client: {client}");
    let response = tokio::select! {
        biased;
        _ = revoked.cancelled() => return unauthorized(),
        response = next.run(request) => response,
    };
    let (parts, body) = response.into_parts();
    let mut stream = body.into_data_stream();
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                biased;
                _ = revoked.cancelled() => break,
                frame = stream.next() => match frame {
                    Some(frame) => yield frame,
                    None => break,
                }
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revocation_ends_streaming_responses_regardless_of_content_type() {
        for content_type in [
            "application/octet-stream",
            "text/event-stream; charset=utf-8",
        ] {
            let home = tempfile::tempdir().unwrap();
            let token = remote_tokens::add(home.path(), "phone").unwrap();
            let (remote, mut listeners) = Remote::bind(
                home.path().to_path_buf(),
                &serde_json::json!({"listen":["127.0.0.1:0"]}),
            )
            .await;
            let listener = listeners.pop().unwrap();
            let address = listener.local_addr().unwrap();
            let router = remote.router(Router::new().route(
                "/stream",
                axum::routing::get(move || async move {
                    let stream =
                        futures_util::stream::iter([Ok::<_, std::convert::Infallible>("first")])
                            .chain(futures_util::stream::pending());
                    (
                        [(header::CONTENT_TYPE, content_type)],
                        Body::from_stream(stream),
                    )
                }),
            ));
            let stopped = CancellationToken::new();
            let watcher = tokio::spawn(remote.watch(stopped.clone()));
            let shutdown = stopped.clone();
            let server = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await
                    .unwrap();
            });
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            let response = client
                .get(format!("http://{address}/stream"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            let mut stream = response.bytes_stream();
            assert_eq!(stream.next().await.unwrap().unwrap(), "first");
            remote_tokens::remove(home.path(), "phone").unwrap();
            let ended = tokio::time::timeout(Duration::from_secs(3), stream.next()).await;
            drop(stream);
            stopped.cancel();
            server.await.unwrap();
            watcher.await.unwrap();
            assert!(
                matches!(ended, Ok(None)),
                "revoked {content_type} response must end cleanly: {ended:?}"
            );
        }
    }
}
