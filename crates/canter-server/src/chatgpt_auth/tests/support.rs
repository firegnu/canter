use super::*;
use axum::{
    Json, Router,
    extract::{Form, Query, State},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::KeyPair;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub const NOW: u64 = 1_800_000_000;
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
pub const CODE: &str = "fake-sensitive-authorization-code";
pub type Params = HashMap<String, String>;

pub struct Fake {
    pub origin: String,
    pub state: Arc<Mutex<Settings>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Settings {
    pub now: Arc<AtomicU64>,
    pub origin: String,
    pub key: Vec<u8>,
    pub jwks: Value,
    pub client_id: String,
    pub subject: String,
    pub scopes: String,
    pub omit_refresh_scope: bool,
    pub omit_refresh_token: bool,
    pub earliest: Option<Value>,
    pub authorize: Vec<Params>,
    pub grants: Vec<Params>,
    pub revocations: Vec<Params>,
    pub callback_override: Params,
    pub omit_client: bool,
    pub bad_claim: Option<&'static str>,
    pub future_iat: u64,
    pub future_nbf: Option<u64>,
    pub error: Option<(u16, String)>,
    pub revoke_status: u16,
    pub discovery_override: Option<Value>,
    pub discovery_status: u16,
    pub jwks_status: u16,
    pub refresh_started: Arc<tokio::sync::Notify>,
    pub refresh_release: Option<Arc<tokio::sync::Notify>>,
    pub revoke_started: Arc<tokio::sync::Notify>,
    pub revoke_release: Option<Arc<tokio::sync::Notify>>,
}
impl Fake {
    pub async fn stop(&self) {
        self.task.abort();
        while !self.task.is_finished() {
            tokio::task::yield_now().await;
        }
    }
    pub async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let rng = ring::rand::SystemRandom::new();
        let key = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .unwrap();
        let pair = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            key.as_ref(),
            &rng,
        )
        .unwrap();
        let public = pair.public_key().as_ref();
        let jwks = json!({"keys":[{"kty":"EC","crv":"P-256","alg":"ES256","use":"sig","kid":"test-key","x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..])}]});
        let state = Arc::new(Mutex::new(Settings {
            now: Arc::new(AtomicU64::new(NOW)),
            origin: origin.clone(),
            key: key.as_ref().to_vec(),
            jwks,
            client_id: "oaiapp_test-one".into(),
            subject: "test-subject".into(),
            scopes: SCOPES.into(),
            omit_refresh_scope: false,
            omit_refresh_token: false,
            earliest: None,
            authorize: Vec::new(),
            grants: Vec::new(),
            revocations: Vec::new(),
            callback_override: Params::new(),
            omit_client: false,
            bad_claim: None,
            future_iat: 0,
            future_nbf: None,
            error: None,
            revoke_status: 200,
            discovery_override: None,
            discovery_status: 200,
            jwks_status: 200,
            refresh_started: Arc::new(tokio::sync::Notify::new()),
            refresh_release: None,
            revoke_started: Arc::new(tokio::sync::Notify::new()),
            revoke_release: None,
        }));
        let app = Router::new()
            .route("/api/accounts/authorize", get(authorize))
            .route("/api/accounts/oauth/token", post(token))
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/.well-known/jwks.json", get(serve_jwks))
            .route("/revoke", post(revoke))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            state,
            task,
        }
    }

    pub async fn callback(&self, attempt: &LoginAttempt) -> reqwest::Response {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = client
            .get(attempt.authorization_url())
            .send()
            .await
            .unwrap();
        let location = response.headers()["location"].to_str().unwrap();
        client.get(location).send().await.unwrap()
    }
}
async fn authorize(
    State(state): State<Arc<Mutex<Settings>>>,
    Query(query): Query<Params>,
) -> Redirect {
    let mut state = state.lock().unwrap();
    let mut url = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    let mut params = Params::from([
        ("code".into(), CODE.into()),
        ("state".into(), query["state"].clone()),
    ]);
    if !state.omit_client {
        params.insert("client_id".into(), state.client_id.clone());
    }
    params.extend(state.callback_override.clone());
    url.query_pairs_mut().extend_pairs(params);
    state.authorize.push(query);
    Redirect::temporary(url.as_str())
}
async fn discovery(State(state): State<Arc<Mutex<Settings>>>) -> axum::response::Response {
    let state = state.lock().unwrap();
    (axum::http::StatusCode::from_u16(state.discovery_status).unwrap(),
        Json(state.discovery_override.clone().unwrap_or_else(|| json!({"issuer":state.origin,"jwks_uri":format!("{}/.well-known/jwks.json",state.origin),"revocation_endpoint":format!("{}/revoke",state.origin)})))).into_response()
}
async fn serve_jwks(State(state): State<Arc<Mutex<Settings>>>) -> axum::response::Response {
    let state = state.lock().unwrap();
    (
        axum::http::StatusCode::from_u16(state.jwks_status).unwrap(),
        Json(state.jwks.clone()),
    )
        .into_response()
}
async fn token(
    State(state): State<Arc<Mutex<Settings>>>,
    Form(form): Form<Params>,
) -> axum::response::Response {
    let pause = {
        let mut state = state.lock().unwrap();
        state.grants.push(form.clone());
        if form["grant_type"] == "refresh_token" {
            state.refresh_started.notify_one();
            state.refresh_release.clone()
        } else {
            None
        }
    };
    if let Some(pause) = pause {
        pause.notified().await;
    }
    let state = state.lock().unwrap();
    if let Some((status, code)) = &state.error {
        let login_state = &state.authorize.last().unwrap()["state"];
        let mut response = (axum::http::StatusCode::from_u16(*status).unwrap(), Json(json!({"error":code,"error_description":format!("fake-access-1 fake-refresh-1 {CODE} {login_state}")}))).into_response();
        if *status == 302 {
            response.headers_mut().insert(
                "location",
                format!("{}/api/accounts/oauth/token", state.origin)
                    .parse()
                    .unwrap(),
            );
        }
        return response;
    }
    let attempt = state.authorize.last().unwrap();
    let now = state.now.load(Ordering::SeqCst);
    let mut claims = json!({"iss":state.origin,"aud":form["client_id"],"sub":state.subject,"exp":now+3600,"iat":now,"nonce":attempt["nonce"],"email":"same@example.test"});
    claims["iat"] = json!(now + state.future_iat);
    if let Some(offset) = state.future_nbf {
        claims["nbf"] = json!(now + offset);
    }
    match state.bad_claim {
        Some("issuer") => claims["iss"] = json!("https://wrong.example.test"),
        Some("audience") => claims["aud"] = json!("other-client"),
        Some("expired") => claims["exp"] = json!(NOW - 1),
        Some("nonce") => claims["nonce"] = json!("wrong-nonce"),
        _ => {}
    }
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some("test-key".into());
    let mut id = jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_ec_der(&state.key),
    )
    .unwrap();
    if state.bad_claim == Some("signature") {
        id.push('x');
    }
    let number = state.grants.len();
    let mut result = json!({"access_token":format!("fake-access-{number}"),"refresh_token":format!("fake-refresh-{number}"),"id_token":id,"token_type":"Bearer","expires_in":3600,"scope":state.scopes});
    if form["grant_type"] == "refresh_token" {
        if state.omit_refresh_scope {
            result.as_object_mut().unwrap().remove("scope");
        }
        if state.omit_refresh_token {
            result.as_object_mut().unwrap().remove("refresh_token");
        }
    }
    if let Some(earliest) = &state.earliest {
        result["earliest_refresh_at"] = earliest.clone();
    }
    Json(result).into_response()
}
async fn revoke(
    State(state): State<Arc<Mutex<Settings>>>,
    Form(form): Form<Params>,
) -> axum::response::Response {
    let pause = {
        let mut state = state.lock().unwrap();
        state.revocations.push(form);
        state.revoke_started.notify_one();
        state.revoke_release.clone()
    };
    if let Some(pause) = pause {
        pause.notified().await;
    }
    let state = state.lock().unwrap();
    (
        axum::http::StatusCode::from_u16(state.revoke_status).unwrap(),
        "",
    )
        .into_response()
}

pub struct Fixture {
    pub auth: ChatgptAuth,
    pub fake: Fake,
    pub root: tempfile::TempDir,
    pub now: Arc<AtomicU64>,
}
impl Fixture {
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new().await;
        let now = fake.state.lock().unwrap().now.clone();
        let clock = now.clone();
        let auth = ChatgptAuth::for_test(
            &root.path().join("state"),
            fake.origin.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        Self {
            auth,
            fake,
            root,
            now,
        }
    }
    pub async fn login(&self, record: Option<&str>) -> Result<RecordInfo, AuthError> {
        let attempt = self.auth.start_login(record).await?;
        self.fake.callback(&attempt).await;
        attempt.wait().await
    }
}
