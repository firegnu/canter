//! Account-only HTTP exchanges. This module never writes session events.
use super::*;
use crate::chatgpt_auth::{AuthError, ChatgptAuth, RecordInfo, RecordStatus};

struct Accounts {
    home: PathBuf,
    auth: Mutex<Option<ChatgptAuth>>,
    logins: Mutex<HashMap<String, tokio::sync::watch::Receiver<Option<AccountLoginCompleted>>>>,
    stopped: CancellationToken,
}

impl Accounts {
    async fn auth(&self) -> Result<ChatgptAuth, ApiError> {
        let mut auth = self.auth.lock().await;
        if auth.is_none() {
            *auth = Some(ChatgptAuth::open(&self.home).map_err(auth_error)?);
        }
        Ok(auth.as_ref().unwrap().clone())
    }
}

pub(super) fn router(home: PathBuf, stopped: CancellationToken) -> Router {
    routes(Accounts {
        home,
        auth: Mutex::new(None),
        logins: Default::default(),
        stopped,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn test_router(home: PathBuf, auth: ChatgptAuth) -> Router {
    routes(Accounts {
        home,
        auth: Mutex::new(Some(auth)),
        logins: Default::default(),
        stopped: CancellationToken::new(),
    })
}

fn routes(accounts: Accounts) -> Router {
    Router::new()
        .route(routes::ACCOUNT_READ, post(read))
        .route(routes::ACCOUNT_LOGIN_START, post(start))
        .route(routes::ACCOUNT_LOGIN_WAIT, post(wait))
        .route(routes::ACCOUNT_LOGIN_CANCEL, post(cancel))
        .route(routes::ACCOUNT_SELECT, post(select))
        .route(routes::ACCOUNT_BACKGROUND, post(background))
        .route(routes::ACCOUNT_LOGOUT, post(logout))
        .layer(axum::middleware::map_response(
            |mut response: Response| async {
                response
                    .headers_mut()
                    .insert("cache-control", "no-store".parse().unwrap());
                response
            },
        ))
        .with_state(Arc::new(accounts))
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;

fn provider(value: &str) -> Result<(), ApiError> {
    if value == "chatgpt" {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::InvalidRequest,
            "Unsupported provider. Providers: chatgpt",
        ))
    }
}

fn auth_error(error: AuthError) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest, &error.to_string())
}

fn public_record(info: RecordInfo) -> AccountInfo {
    AccountInfo {
        id: info.id,
        email: info.email,
        status: match info.status {
            RecordStatus::LoggedIn => AccountStatus::LoggedIn,
            RecordStatus::PlanNotAuthorized => AccountStatus::PlanNotAuthorized,
            RecordStatus::NeedsLogin => AccountStatus::NeedsLogin,
            RecordStatus::SignedOut => AccountStatus::SignedOut,
        },
        plan_authorized: info.status == RecordStatus::LoggedIn,
        background_consent: info.background_consent,
        pending_revocation: info.pending_revocation,
    }
}

async fn read(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountReadParams>, JsonRejection>,
) -> Result<Json<AccountReadResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let auth = accounts.auth().await?;
    Ok(Json(snapshot(&auth).await))
}

async fn snapshot(auth: &ChatgptAuth) -> AccountReadResponse {
    AccountReadResponse {
        provider: "chatgpt".into(),
        accounts: auth
            .records()
            .await
            .into_iter()
            .map(public_record)
            .collect(),
        current_account_id: auth.current_record().await,
    }
}

async fn resolve(auth: &ChatgptAuth, selector: Option<&str>) -> Result<RecordInfo, ApiError> {
    let records = auth.records().await;
    let current = auth.current_record().await;
    let selector = selector.or(current.as_deref()).ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "No current account. Run `canter auth login chatgpt`.",
        )
    })?;
    if let Ok(number) = selector.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|i| records.get(i))
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidRequest,
                    "Account number not found. Run `canter auth status chatgpt`.",
                )
            });
    }
    let mut matches = records
        .into_iter()
        .filter(|record| record.id == selector || record.email.as_deref() == Some(selector));
    let record = matches.next().ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "Account not found. Run `canter auth status chatgpt`.",
        )
    })?;
    if matches.next().is_some() {
        return Err(ApiError::new(
            ErrorCode::InvalidRequest,
            "Multiple accounts have this email. Use the number from `canter auth status chatgpt`.",
        ));
    }
    Ok(record)
}

async fn select(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountSelectParams>, JsonRejection>,
) -> Result<Json<AccountReadResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let auth = accounts.auth().await?;
    let record = resolve(&auth, Some(&params.account)).await?;
    auth.select(&record.id).await.map_err(auth_error)?;
    Ok(Json(snapshot(&auth).await))
}

async fn background(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountBackgroundParams>, JsonRejection>,
) -> Result<Json<AccountReadResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let auth = accounts.auth().await?;
    let record = resolve(&auth, None).await?;
    auth.set_background_consent(&record.id, params.enabled)
        .await
        .map_err(auth_error)?;
    Ok(Json(snapshot(&auth).await))
}

async fn logout(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountLogoutParams>, JsonRejection>,
) -> Result<Json<AccountLogoutResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let auth = accounts.auth().await?;
    let record = resolve(&auth, params.account.as_deref()).await?;
    let result = match auth.logout(&record.id).await.map_err(auth_error)? {
        crate::chatgpt_auth::LogoutResult::Revoked => AccountLogoutResult::Revoked,
        crate::chatgpt_auth::LogoutResult::Pending => AccountLogoutResult::Pending,
        crate::chatgpt_auth::LogoutResult::NoToken => AccountLogoutResult::NoToken,
    };
    Ok(Json(AccountLogoutResponse {
        provider: params.provider,
        account: public_record(resolve(&auth, Some(&record.id)).await?),
        result,
    }))
}

async fn start(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountLoginStartParams>, JsonRejection>,
) -> Result<Json<AccountLoginStartResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    if accounts.stopped.is_cancelled() {
        return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
    }
    let auth = accounts.auth().await?;
    let mut record_id = params.account_id;
    if record_id.is_none() {
        let current = auth.current_record().await;
        record_id = auth
            .records()
            .await
            .into_iter()
            .find(|record| {
                Some(&record.id) == current.as_ref() && record.status != RecordStatus::LoggedIn
            })
            .map(|record| record.id);
    }
    let attempt = auth
        .start_login(record_id.as_deref())
        .await
        .map_err(auth_error)?;
    let response = AccountLoginStartResponse {
        provider: params.provider.clone(),
        login_id: attempt.id.clone(),
        auth_url: attempt.authorization_url().into(),
    };
    let (sender, receiver) = tokio::sync::watch::channel(None);
    let login_id = attempt.id.clone();
    accounts
        .logins
        .lock()
        .await
        .insert(login_id.clone(), receiver);
    let weak = Arc::downgrade(&accounts);
    let stopped = accounts.stopped.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = stopped.cancelled() => Err(crate::chatgpt_auth::ErrorKind::Cancelled.into()),
            result = attempt.wait() => result,
        };
        let result = match result {
            Ok(record) => {
                auth.complete_login(&record.id, params.background_consent)
                    .await
            }
            Err(error) => Err(error),
        };
        drop(auth);
        let completed = match result {
            Ok((record, show_plan_notice)) => AccountLoginCompleted {
                provider: params.provider,
                login_id: login_id.clone(),
                success: true,
                error: None,
                account: Some(public_record(record)),
                show_plan_notice,
            },
            Err(error) => AccountLoginCompleted {
                provider: params.provider,
                login_id: login_id.clone(),
                success: false,
                error: Some(error.to_string()),
                account: None,
                show_plan_notice: false,
            },
        };
        sender.send_replace(Some(completed));
        // A short retry window accommodates a dropped HTTP response. No URL is retained.
        tokio::select! {
            _ = stopped.cancelled() => {},
            _ = tokio::time::sleep(Duration::from_secs(60)) => {},
        }
        if let Some(accounts) = weak.upgrade() {
            accounts.logins.lock().await.remove(&login_id);
        }
    });
    Ok(Json(response))
}

async fn wait(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountLoginParams>, JsonRejection>,
) -> Result<Json<AccountLoginCompleted>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let mut result = accounts
        .logins
        .lock()
        .await
        .get(&params.login_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidRequest,
                "Sign-in attempt not found. Start sign-in again.",
            )
        })?;
    let value = result
        .wait_for(Option::is_some)
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(value.as_ref().unwrap().clone()))
}

async fn cancel(
    State(accounts): State<Arc<Accounts>>,
    params: Result<Json<AccountLoginParams>, JsonRejection>,
) -> Result<Json<AccountLoginCancelResponse>, ApiError> {
    let params = body(params)?;
    provider(&params.provider)?;
    let status = if accounts.auth().await?.cancel_login(&params.login_id).await {
        AccountLoginCancelStatus::Canceled
    } else {
        AccountLoginCancelStatus::NotFound
    };
    Ok(Json(AccountLoginCancelResponse {
        provider: params.provider,
        status,
    }))
}
