//! MCP authorization and private local credentials. SDK errors never cross
//! this boundary: their text may contain provider bodies or credentials.
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, CredentialRefreshGuard, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Serialize, Deserialize)]
struct Saved {
    url: String,
    credentials: StoredCredentials,
    client_secret: Option<String>,
}

#[derive(Clone)]
pub(super) struct Store {
    path: PathBuf,
    url: String,
    client_secret: Option<String>,
    pub(super) secrets: Arc<Mutex<Vec<String>>>,
}
fn store_error() -> AuthError {
    AuthError::CredentialStoreError("cannot read or write MCP credentials".into())
}

impl Store {
    pub(super) fn new(
        home: &Path,
        name: &str,
        url: &str,
        secrets: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        let name: String = name.bytes().map(|b| format!("{b:02x}")).collect();
        Self {
            path: home.join("mcp-auth").join(format!("{name}.json")),
            url: url.into(),
            client_secret: None,
            secrets,
        }
    }
    fn read(&self) -> Result<Option<Saved>, AuthError> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(store_error()),
        };
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| store_error())?;
        let saved: Saved = serde_json::from_reader(file).map_err(|_| store_error())?;
        if saved.url != self.url {
            return Ok(None);
        }
        self.remember(&saved);
        Ok(Some(saved))
    }
    fn remember(&self, saved: &Saved) {
        let mut secrets = self.secrets.lock().unwrap();
        let value = serde_json::to_value(&saved.credentials.token_response).unwrap_or_default();
        for secret in [
            value["access_token"].as_str(),
            value["refresh_token"].as_str(),
            saved.client_secret.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        {
            if !secrets.iter().any(|known| known == secret) {
                secrets.push(secret.into());
            }
        }
        secrets.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));
    }
    pub(super) async fn lock(&self) -> Result<fs::File, AuthError> {
        let dir = self.path.parent().unwrap();
        fs::create_dir_all(dir).map_err(|_| store_error())?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| store_error())?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.path.with_extension("lock"))
            .map_err(|_| store_error())?;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                Err(_) => return Err(store_error()),
            }
        }
    }
}
#[async_trait::async_trait]
impl CredentialStore for Store {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        Ok(self.read()?.map(|s| s.credentials))
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let saved = Saved {
            url: self.url.clone(),
            credentials,
            client_secret: self.client_secret.clone(),
        };
        self.remember(&saved);
        let mut file = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())
            .map_err(|_| store_error())?;
        serde_json::to_writer(&mut file, &saved).map_err(|_| store_error())?;
        file.as_file().sync_all().map_err(|_| store_error())?;
        file.persist(&self.path).map_err(|_| store_error())?;
        Ok(())
    }
    async fn clear(&self) -> Result<(), AuthError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_error()),
        }
    }
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        Ok(Some(CredentialRefreshGuard::new(self.lock().await?)))
    }
}

pub(super) fn http_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| store_error())
}
async fn manager(url: &str) -> Result<AuthorizationManager, AuthError> {
    let mut manager = AuthorizationManager::new(url).await?;
    manager.with_client(http_client()?)?;
    Ok(manager)
}

pub(super) async fn authorized_client(
    mut store: Store,
    client: reqwest::Client,
    use_oauth: bool,
) -> Result<rmcp::transport::auth::AuthClient<reqwest::Client>, AuthError> {
    let mut manager = manager(&store.url).await?;
    if use_oauth {
        if store.read()?.is_some() {
            let _guard = store.lock().await?;
            let saved = store.read()?.ok_or(AuthError::AuthorizationRequired)?;
            store.client_secret = saved.client_secret.clone();
            manager.set_credential_store(store);
            if !manager.initialize_from_store().await? {
                return Err(AuthError::AuthorizationRequired);
            }
            let mut config =
                OAuthClientConfig::new(saved.credentials.client_id, "http://localhost");
            config.client_secret = saved.client_secret;
            manager.configure_client(config)?;
        } else {
            manager.set_credential_store(store);
        }
    }
    Ok(rmcp::transport::auth::AuthClient::new(client, manager))
}

pub async fn login(
    home: &Path,
    name: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
) -> Result<(), String> {
    if headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("authorization"))
    {
        return Err("remove the configured Authorization header before OAuth login".into());
    }
    let mut store = Store::new(home, name, url, Default::default());
    let _guard = store
        .lock()
        .await
        .map_err(|_| "cannot lock MCP credentials")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "cannot bind the local OAuth callback port")?;
    let redirect = format!(
        "http://{}/callback",
        listener
            .local_addr()
            .map_err(|_| "cannot read OAuth callback port")?
    );
    let mut manager = manager(url)
        .await
        .map_err(|_| "cannot initialize MCP OAuth")?;
    let metadata = manager
        .resolve_metadata()
        .await
        .map_err(|_| "cannot discover MCP authorization server")?
        .metadata;
    if metadata.registration_endpoint.is_none() {
        return Err("authorization server does not support dynamic client registration".into());
    }
    manager.set_metadata(metadata);
    let scopes = manager.select_initial_scopes(&[]).await;
    let scopes: Vec<_> = scopes.iter().map(String::as_str).collect();
    let config = manager
        .register_client("canter", &redirect, &scopes)
        .await
        .map_err(|_| "OAuth dynamic client registration failed")?;
    store.client_secret = config.client_secret;
    manager.set_credential_store(store);
    let auth_url = manager
        .get_authorization_url(&scopes)
        .await
        .map_err(|_| "cannot create OAuth authorization URL")?;
    println!("{auth_url}");
    std::io::stdout()
        .flush()
        .map_err(|_| "cannot print OAuth authorization URL")?;
    let browser = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let opened = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(browser)
            .arg(&auth_url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await;
    if !matches!(opened,Ok(Ok(status)) if status.success()) {
        eprintln!("Open the authorization URL above in your browser.");
    }
    let expected_state = reqwest::Url::parse(&auth_url)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned())
        })
        .ok_or("OAuth state missing")?;
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let (socket, _) = listener.accept().await.map_err(|_| "OAuth callback listener failed")?;
            let mut socket = BufReader::new(socket);
            let mut request = String::new();
            // Only the request line is needed; bound both its size and read time.
            let read = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::io::AsyncReadExt::take(&mut socket, 8192).read_line(&mut request),
            ).await;
            if !matches!(read, Ok(Ok(n)) if n > 0 && n < 8192) {
                continue;
            }
            let mut parts = request.split_whitespace();
            let method = parts.next();
            let target = parts.next().unwrap_or_default();
            let callback = reqwest::Url::parse(&format!("http://127.0.0.1{target}"));
            let valid = callback.as_ref().is_ok_and(|u| {
                u.path() == "/callback"
                    && u.query_pairs().filter(|(k, _)| k == "state").count() == 1
                    && u.query_pairs().any(|(k, v)| k == "state" && v == expected_state)
            });
            if method != Some("GET") || !valid {
                let _ = socket.get_mut().write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            }
            let callback = callback.unwrap();
            let denied = callback.query_pairs().any(|(k, _)| k == "error");
            let result = if denied {
                Err("OAuth authorization was denied".into())
            } else {
                let code = callback.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned());
                let issuer = callback.query_pairs().find(|(k, _)| k == "iss").map(|(_, v)| v.into_owned());
                match code {
                    Some(code) => manager
                        .exchange_code_for_token_with_issuer(&code, &expected_state, issuer.as_deref())
                        .await
                        .map(|_| ())
                        .map_err(|_| "OAuth callback or token exchange failed".into()),
                    None => Err("OAuth callback has no authorization code".into()),
                }
            };
            let response: &[u8] = if result.is_ok() {
                b"HTTP/1.1 200 OK\r\nContent-Length: 22\r\nConnection: close\r\n\r\nYou may close this tab."
            } else {
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = socket.get_mut().write_all(response).await;
            return result;
        }
    }).await.map_err(|_| "OAuth login timed out")?
}

pub async fn logout(home: &Path, name: &str) -> Result<(), String> {
    let store = Store::new(home, name, "", Default::default());
    let _guard = store
        .lock()
        .await
        .map_err(|_| "cannot lock MCP credentials")?;
    store
        .clear()
        .await
        .map_err(|_| "cannot remove MCP credentials".into())
}
