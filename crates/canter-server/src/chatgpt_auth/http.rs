use super::{AuthError, ErrorKind, OAuthCode, Secret, TokenSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::{Client, Url};
use serde::{Deserialize, de::DeserializeOwned};
use std::{sync::Arc, time::Duration};

const ISSUER: &str = "https://auth.openai.com";
pub(super) const RESOURCE: &str = "https://api.openai.com/v1";
pub(super) const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

pub(super) struct Http {
    client: Client,
    origin: String,
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: String,
}
#[derive(Deserialize)]
pub(super) struct Identity {
    pub sub: String,
    pub email: Option<String>,
    exp: u64,
    iat: u64,
    nbf: Option<u64>,
    nonce: Option<String>,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Secret,
    refresh_token: Option<Secret>,
    id_token: Option<Secret>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
    earliest_refresh_at: Option<serde_json::Value>,
}

impl Http {
    pub fn new() -> Result<Self, AuthError> {
        Self::build(
            ISSUER.into(),
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            }),
        )
    }
    fn build(origin: String, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Result<Self, AuthError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("canter/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| AuthError::from(ErrorKind::ClientConfiguration))?;
        Ok(Self {
            client,
            origin,
            clock,
        })
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(origin: String, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        let url = Url::parse(&origin).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.port().is_some());
        Self::build(origin, clock).unwrap()
    }
    pub fn issuer(&self) -> &str {
        &self.origin
    }
    pub fn authorize_url(&self) -> Url {
        Url::parse(&format!("{}/api/accounts/authorize", self.origin))
            .expect("fixed authorization URL")
    }
    fn trusted_url(&self, value: &str) -> Result<Url, AuthError> {
        let url = Url::parse(value).map_err(|_| AuthError::from(ErrorKind::InvalidResponse))?;
        let origin = Url::parse(&self.origin).expect("fixed issuer URL");
        if url.origin() != origin.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(ErrorKind::InvalidResponse.into());
        }
        Ok(url)
    }
    async fn discovery(&self) -> Result<Discovery, AuthError> {
        let discovery: Discovery = self
            .json(
                self.client
                    .get(format!("{}/.well-known/openid-configuration", self.origin)),
            )
            .await?;
        if discovery.issuer != self.origin {
            return Err(ErrorKind::InvalidResponse.into());
        }
        self.trusted_url(&discovery.jwks_uri)?;
        self.trusted_url(&discovery.revocation_endpoint)?;
        Ok(discovery)
    }
    pub async fn tokens(
        &self,
        form: &[(&str, &str)],
        previous: Option<&TokenSet>,
    ) -> Result<TokenSet, AuthError> {
        let response: TokenResponse = self
            .json(
                self.client
                    .post(format!("{}/api/accounts/oauth/token", self.origin))
                    .form(form),
            )
            .await?;
        if !response.token_type.eq_ignore_ascii_case("Bearer")
            || response.access_token.0.is_empty()
            || response.expires_in == 0
        {
            return Err(ErrorKind::InvalidResponse.into());
        }
        let received_at = (self.clock)();
        received_at
            .checked_add(response.expires_in)
            .ok_or(ErrorKind::InvalidResponse)?;
        let scopes = match response.scope {
            Some(scope) => scope.split_ascii_whitespace().map(str::to_owned).collect(),
            None => previous.ok_or(ErrorKind::InvalidResponse)?.scopes.clone(),
        };
        Ok(TokenSet {
            access: response.access_token,
            refresh: response
                .refresh_token
                .filter(|value| !value.0.is_empty())
                .or_else(|| previous.and_then(|tokens| tokens.refresh.clone())),
            id: response.id_token,
            scopes,
            received_at,
            expires_in: response.expires_in,
            earliest_refresh_at: response.earliest_refresh_at,
        })
    }
    pub async fn revoke(&self, token: &Secret, client_id: &str) -> bool {
        for attempt in 0..3 {
            let result = async {
                let discovery = self.discovery().await?;
                let response = self
                    .client
                    .post(self.trusted_url(&discovery.revocation_endpoint)?)
                    .form(&[
                        ("token", token.0.as_str()),
                        ("token_type_hint", "refresh_token"),
                        ("client_id", client_id),
                    ])
                    .send()
                    .await
                    .map_err(|_| AuthError::from(ErrorKind::Network))?;
                let status = response.status().as_u16();
                if status == 200 {
                    return Ok(());
                }
                Err(AuthError {
                    kind: if status >= 500 {
                        ErrorKind::Network
                    } else {
                        ErrorKind::InvalidResponse
                    },
                    status: Some(status),
                    code: None,
                })
            }
            .await;
            match result {
                Ok(()) => return true,
                Err(error) if error.kind == ErrorKind::Network && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
                }
                Err(_) => return false,
            }
        }
        false
    }
    pub async fn identity(
        &self,
        token: &Secret,
        client: &str,
        nonce: Option<&str>,
    ) -> Result<Identity, AuthError> {
        let invalid = || AuthError::from(ErrorKind::InvalidIdToken);
        let header = decode_header(&token.0).map_err(|_| invalid())?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            return Err(invalid());
        }
        let discovery = self.discovery().await?;
        // Fetch for each validation: no stale key cache on a changed kid. Login
        // and token refresh are infrequent; no network fetch on ordinary use.
        let jwks: JwkSet = self
            .json(self.client.get(self.trusted_url(&discovery.jwks_uri)?))
            .await?;
        let kid = header.kid.as_deref().ok_or_else(invalid)?;
        let key = jwks.find(kid).ok_or_else(invalid)?;
        if key
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != jsonwebtoken::jwk::PublicKeyUse::Signature)
            || key
                .common
                .key_operations
                .as_ref()
                .is_some_and(|ops| !ops.contains(&jsonwebtoken::jwk::KeyOperations::Verify))
            || key
                .common
                .key_algorithm
                .is_some_and(|alg| Algorithm::try_from(alg).ok() != Some(header.alg))
        {
            return Err(invalid());
        }
        let decoding = DecodingKey::from_jwk(key).map_err(|_| invalid())?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.origin]);
        validation.set_audience(&[client]);
        validation.set_required_spec_claims(&["iss", "aud", "sub", "exp", "iat"]);
        // Signature, issuer and audience are validated by the library. Time is
        // validated below using the same injectable clock as access expiry.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let identity = decode::<Identity>(&token.0, &decoding, &validation)
            .map_err(|_| invalid())?
            .claims;
        let now = (self.clock)();
        if identity.sub.is_empty()
            || identity.exp <= now
            || identity.iat > now.saturating_add(60)
            || identity.nbf.is_some_and(|nbf| nbf > now.saturating_add(60))
            || nonce.is_some_and(|nonce| identity.nonce.as_deref() != Some(nonce))
        {
            return Err(invalid());
        }
        Ok(identity)
    }
    async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, AuthError> {
        let mut response = request
            .send()
            .await
            .map_err(|_| AuthError::from(ErrorKind::Network))?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AuthError::from(ErrorKind::Network))?
        {
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err(ErrorKind::InvalidResponse.into());
            }
            body.extend_from_slice(&chunk);
        }
        if status != 200 {
            let code = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    let error = &value["error"];
                    OAuthCode::parse(error.as_str().or_else(|| error["code"].as_str())?)
                });
            let kind = match code {
                Some(OAuthCode::InvalidClient) => ErrorKind::ClientConfiguration,
                Some(OAuthCode::AccessDenied) => ErrorKind::AccessDenied,
                Some(_) => ErrorKind::NeedsLogin,
                None if status >= 500 || status == 429 => ErrorKind::Network,
                None => ErrorKind::InvalidResponse,
            };
            return Err(AuthError {
                kind,
                status: Some(status),
                code,
            });
        }
        serde_json::from_slice(&body).map_err(|_| AuthError::from(ErrorKind::InvalidResponse))
    }
}
impl OAuthCode {
    fn parse(code: &str) -> Option<Self> {
        Some(match code {
            "invalid_grant" => Self::InvalidGrant,
            "invalid_refresh_token" => Self::InvalidRefreshToken,
            "token_expired" => Self::TokenExpired,
            "refresh_token_expired" => Self::RefreshTokenExpired,
            "refresh_token_invalidated" => Self::RefreshTokenInvalidated,
            "refresh_token_reused" => Self::RefreshTokenReused,
            "invalid_client" => Self::InvalidClient,
            "access_denied" => Self::AccessDenied,
            _ => return None,
        })
    }
}
