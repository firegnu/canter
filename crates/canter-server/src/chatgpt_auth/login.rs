use super::*;
use axum::{
    Router,
    extract::{Query, rejection::QueryRejection},
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::SecureRandom;
use std::{sync::Mutex as StdMutex, time::Duration};

#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    client_id: Option<String>,
    error: Option<String>,
}

fn random_secret() -> Result<String, AuthError> {
    let mut bytes = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| AuthError::from(ErrorKind::ClientConfiguration))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl ChatgptAuth {
    pub async fn start_login(&self, record_id: Option<&str>) -> Result<LoginAttempt, AuthError> {
        let existing = match record_id {
            Some(id) => {
                let entry = self.entry(id).await?;
                let pending = entry.record.lock().await.pending_revocation.is_some();
                if pending {
                    // Best effort before this registration can be overwritten.
                    self.logout(id).await?;
                }
                let record = entry.record.lock().await;
                Some((record.info.clone(), entry.epoch.load(Ordering::SeqCst)))
            }
            None => None,
        };
        // Listener first, then fresh attempt secrets. No browser is opened here.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| AuthError::from(ErrorKind::Network))?;
        let redirect = format!(
            "http://{}/auth/callback",
            listener
                .local_addr()
                .map_err(|_| AuthError::from(ErrorKind::Network))?
        );
        let state = random_secret()?;
        let nonce = random_secret()?;
        let verifier = random_secret()?;
        let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(
            &ring::digest::SHA256,
            verifier.as_bytes(),
        ));
        let mut url = self.inner.http.authorize_url();
        url.query_pairs_mut().extend_pairs([
            (
                "client_id",
                existing
                    .as_ref()
                    .map_or("dynamic_agent_client", |(info, _)| &info.client_id),
            ),
            ("ext_agent_host_id", self.host_id()),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("scope", http::SCOPES),
            ("resource", http::RESOURCE),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ]);
        if existing.is_none() {
            url.query_pairs_mut()
                .append_pair("agent_name_hint", "canter");
        }
        let (callback_tx, callback_rx) = oneshot::channel();
        let sender = Arc::new(StdMutex::new(Some(callback_tx)));
        let app = Router::new().route("/auth/callback", get(move |query: Result<Query<Callback>, QueryRejection>| {
            let sender = sender.clone();
            async move {
                let value = query.map(|query| query.0).map_err(|_| AuthError::from(ErrorKind::InvalidResponse));
                let accepted = sender.lock().unwrap().take().is_some_and(|sender| sender.send(value).is_ok());
                let status = if accepted { axum::http::StatusCode::OK } else { axum::http::StatusCode::BAD_REQUEST };
                (status, [("content-type", "text/html; charset=utf-8"), ("cache-control", "no-store"),
                    ("referrer-policy", "no-referrer"), ("content-security-policy", "default-src 'none'; frame-ancestors 'none'")],
                    "<!doctype html><title>canter sign-in</title><p>Return to canter to see the sign-in result.</p>").into_response()
            }
        }));
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(server_stop.cancelled_owned())
                .await
        });
        let cancel = CancellationToken::new();
        let id = uuid::Uuid::new_v4().to_string();
        self.inner
            .logins
            .lock()
            .await
            .insert(id.clone(), cancel.clone());
        let (result_tx, result) = oneshot::channel();
        let auth = self.clone();
        let task_id = id.clone();
        let task_cancel = cancel.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        tokio::spawn(async move {
            let work = async {
                let callback = callback_rx
                    .await
                    .map_err(|_| AuthError::from(ErrorKind::Cancelled))??;
                if callback.state.as_deref() != Some(&state) {
                    return Err(ErrorKind::StateMismatch.into());
                }
                if let Some(error) = callback.error {
                    return Err(if error == "access_denied" {
                        ErrorKind::AccessDenied
                    } else {
                        ErrorKind::InvalidResponse
                    }
                    .into());
                }
                let client = match &existing {
                    Some((info, _)) => {
                        if callback
                            .client_id
                            .as_ref()
                            .is_some_and(|client| *client != info.client_id)
                        {
                            return Err(ErrorKind::ClientMismatch.into());
                        }
                        info.client_id.clone()
                    }
                    None => callback
                        .client_id
                        .filter(|id| !id.is_empty() && id != "dynamic_agent_client")
                        .ok_or(ErrorKind::ClientMismatch)?,
                };
                let code = callback
                    .code
                    .filter(|code| !code.is_empty())
                    .ok_or(ErrorKind::InvalidResponse)?;
                let tokens = auth
                    .inner
                    .http
                    .tokens(
                        &[
                            ("grant_type", "authorization_code"),
                            ("client_id", &client),
                            ("code", &code),
                            ("code_verifier", &verifier),
                            ("redirect_uri", &redirect),
                            ("resource", http::RESOURCE),
                        ],
                        None,
                    )
                    .await?;
                let identity = auth
                    .inner
                    .http
                    .identity(
                        tokens.id.as_ref().ok_or(ErrorKind::InvalidIdToken)?,
                        &client,
                        Some(&nonce),
                    )
                    .await?;
                if existing
                    .as_ref()
                    .is_some_and(|(info, _)| info.subject != identity.sub)
                {
                    return Err(ErrorKind::IdentityMismatch.into());
                }
                let info = RecordInfo {
                    id: existing.as_ref().map_or_else(
                        || uuid::Uuid::new_v4().to_string(),
                        |(info, _)| info.id.clone(),
                    ),
                    issuer: auth.inner.http.issuer().into(),
                    subject: identity.sub,
                    client_id: client,
                    email: identity.email,
                    status: tokens.status(),
                    background_consent: existing
                        .as_ref()
                        .is_some_and(|(info, _)| info.background_consent),
                    pending_revocation: false,
                };
                let record = Record {
                    info: info.clone(),
                    tokens: Some(tokens),
                    pending_revocation: None,
                };
                if let Some((_, epoch)) = existing {
                    let entry = auth.entry(&info.id).await?;
                    let mut current = entry.record.lock().await;
                    let _publish = entry.publish.lock().unwrap();
                    if entry.epoch.load(Ordering::SeqCst) != epoch {
                        return Err(ErrorKind::Cancelled.into());
                    }
                    auth.inner.store.save_record(&record, auth.host_id())?;
                    *current = record;
                    entry.epoch.fetch_add(1, Ordering::SeqCst);
                    entry.blocked.store(false, Ordering::SeqCst);
                } else {
                    let mut entries = auth.inner.records.lock().await;
                    for entry in entries.values() {
                        if entry.record.lock().await.info.client_id == info.client_id {
                            return Err(ErrorKind::ClientMismatch.into());
                        }
                    }
                    auth.inner.store.save_record(&record, auth.host_id())?;
                    entries.insert(info.id.clone(), Arc::new(Entry::new(record)));
                }
                Ok(info)
            };
            let outcome = tokio::select! {
                biased;
                _ = task_cancel.cancelled() => Err(ErrorKind::Cancelled.into()),
                _ = tokio::time::sleep_until(deadline) => Err(ErrorKind::TimedOut.into()),
                result = work => result,
            };
            stop.cancel();
            // A stalled callback connection must not keep a cancelled login alive.
            let mut server = server;
            if tokio::time::timeout(Duration::from_secs(1), &mut server)
                .await
                .is_err()
            {
                server.abort();
                let _ = server.await;
            }
            auth.inner.logins.lock().await.remove(&task_id);
            let _ = result_tx.send(outcome);
        });
        Ok(LoginAttempt {
            id,
            url: url.into(),
            cancel,
            result,
        })
    }

    pub async fn cancel_login(&self, id: &str) -> bool {
        if let Some(cancel) = self.inner.logins.lock().await.get(id) {
            cancel.cancel();
            true
        } else {
            false
        }
    }
}
