//! One lazy account manager shared by account commands and model requests.
use crate::chatgpt_auth::{AuthError, ChatgptAuth};
use canter_model::{ModelContext, ModelError, Request, ResponsesAuth, ResponsesCredential};
use futures_util::future::BoxFuture;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub(crate) struct ModelAuth {
    #[cfg(test)]
    pub endpoint: Option<reqwest::Url>,
    home: PathBuf,
    manager: Mutex<Option<ChatgptAuth>>,
}

impl ModelAuth {
    pub fn new(home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            home,
            manager: Mutex::new(None),
            #[cfg(test)]
            endpoint: None,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_manager(home: PathBuf, manager: ChatgptAuth) -> Arc<Self> {
        Arc::new(Self {
            home,
            manager: Mutex::new(Some(manager)),
            #[cfg(test)]
            endpoint: None,
        })
    }

    pub async fn get(&self) -> Result<ChatgptAuth, AuthError> {
        let mut manager = self.manager.lock().await;
        if manager.is_none() {
            *manager = Some(ChatgptAuth::open(&self.home)?);
        }
        Ok(manager.as_ref().unwrap().clone())
    }

    #[cfg(test)]
    pub fn for_test(home: PathBuf, manager: ChatgptAuth, endpoint: reqwest::Url) -> Arc<Self> {
        Arc::new(Self {
            home,
            manager: Mutex::new(Some(manager)),
            endpoint: Some(endpoint),
        })
    }
}

impl ResponsesAuth for ModelAuth {
    fn credential<'a>(
        &'a self,
        _: &'a Request,
        context: Option<&'a ModelContext>,
    ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>> {
        Box::pin(async move {
            let auth = self.get().await.map_err(model_error)?;
            let selected = || async {
                auth.current_record()
                    .await
                    .ok_or_else(|| failure(canter_model::ErrorKind::ChatgptNotLoggedIn))
            };
            let record = if let Some(context) = context {
                context
                    .credential_binding
                    .get_or_try_init(selected)
                    .await?
                    .clone()
            } else {
                selected().await?
            };
            let consent_changes = auth.consent_changes();
            Ok(Box::new(Credential {
                auth,
                record,
                context: context.cloned(),
                consent_changes,
                token: None,
            }) as Box<dyn ResponsesCredential>)
        })
    }
}

struct Credential {
    auth: ChatgptAuth,
    record: String,
    context: Option<ModelContext>,
    consent_changes: tokio::sync::watch::Receiver<()>,
    token: Option<crate::chatgpt_auth::AccessToken>,
}

impl Credential {
    async fn allowed(&self) -> Result<bool, ModelError> {
        Ok(self
            .auth
            .background_consent(&self.record)
            .await
            .map_err(model_error)?
            || self
                .context
                .as_ref()
                .is_some_and(|context| *context.foreground.borrow()))
    }

    async fn wait_for_foreground(&mut self) -> Result<(), ModelError> {
        while !self.allowed().await? {
            let context = self
                .context
                .as_mut()
                .ok_or_else(|| failure(canter_model::ErrorKind::ForegroundRequired))?;
            tokio::select! {
                changed = context.foreground.changed() => changed.map_err(|_| failure(canter_model::ErrorKind::ForegroundRequired))?,
                changed = self.consent_changes.changed() => changed.map_err(|_| failure(canter_model::ErrorKind::ChatgptCredentials))?,
            }
        }
        Ok(())
    }
}

impl ResponsesCredential for Credential {
    fn authorization(&mut self) -> BoxFuture<'_, Result<reqwest::header::HeaderValue, ModelError>> {
        Box::pin(async move {
            loop {
                self.wait_for_foreground().await?;
                let token = self
                    .auth
                    .access_token(&self.record)
                    .await
                    .map_err(model_error)?;
                // A refresh may have taken time while the last interface left.
                if !self.allowed().await? {
                    continue;
                }
                let header = token.authorization_header().map_err(model_error)?;
                self.token = Some(token);
                return Ok(header);
            }
        })
    }

    fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>> {
        Box::pin(async move {
            self.wait_for_foreground().await?;
            let rejected = self
                .token
                .as_mut()
                .ok_or_else(|| failure(canter_model::ErrorKind::ChatgptAuthorization))?;
            self.token = Some(
                self.auth
                    .recover_unauthorized(rejected)
                    .await
                    .map_err(model_error)?,
            );
            Ok(())
        })
    }
}

fn failure(kind: canter_model::ErrorKind) -> ModelError {
    ModelError {
        kind,
        status: None,
        retry_after: None,
        retries: 0,
    }
}

fn model_error(error: AuthError) -> ModelError {
    use crate::chatgpt_auth::ErrorKind as Auth;
    use canter_model::ErrorKind as Model;
    let kind = match error.kind {
        Auth::NotLoggedIn => Model::ChatgptNotLoggedIn,
        Auth::NeedsLogin => Model::ChatgptNeedsLogin,
        Auth::PlanNotAuthorized => Model::ChatgptPlanNotAuthorized,
        Auth::Storage => Model::ChatgptCredentials,
        Auth::ClientConfiguration => Model::ChatgptClientConfiguration,
        Auth::Network => Model::Network,
        Auth::TimedOut => Model::Timeout,
        _ => Model::ChatgptAuthorization,
    };
    ModelError {
        status: error.status,
        ..failure(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt_auth::test_support::Fake;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn account_selection_is_frozen_and_foreground_consent_gates_each_attempt() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new().await;
        let now = fake.state.lock().unwrap().now.clone();
        let auth = ChatgptAuth::for_test(
            &root.path().join("state"),
            fake.origin.clone(),
            Arc::new(move || now.load(Ordering::SeqCst)),
        );
        let login = auth.start_login(None).await.unwrap();
        assert!(fake.callback(&login).await.status().is_success());
        let first = login.wait().await.unwrap();
        auth.complete_login(&first.id, false).await.unwrap();
        let source = ModelAuth::with_manager(root.path().join("state"), auth.clone());
        let (presence, foreground) = tokio::sync::watch::channel(true);
        let context = ModelContext {
            foreground,
            credential_binding: Default::default(),
        };
        let request = Request::default();
        let mut credential = source.credential(&request, Some(&context)).await.unwrap();
        let first_header = credential.authorization().await.unwrap();
        assert!(first_header.is_sensitive());
        presence.send_replace(false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                credential.authorization()
            )
            .await
            .is_err()
        );
        presence.send_replace(true);
        assert!(credential.authorization().await.unwrap() == first_header);
        presence.send_replace(false);
        auth.set_background_consent(&first.id, true).await.unwrap();
        assert!(credential.authorization().await.unwrap() == first_header);
        fake.state.lock().unwrap().client_id = "second-test-client".into();
        let login = auth.start_login(None).await.unwrap();
        assert!(fake.callback(&login).await.status().is_success());
        let second = login.wait().await.unwrap();
        auth.complete_login(&second.id, true).await.unwrap();
        let mut same_turn = source.credential(&request, Some(&context)).await.unwrap();
        assert!(same_turn.authorization().await.unwrap() == first_header);
        let next = ModelContext {
            foreground: presence.subscribe(),
            credential_binding: Default::default(),
        };
        let mut next_turn = source.credential(&request, Some(&next)).await.unwrap();
        assert!(next_turn.authorization().await.unwrap() != first_header);
        fake.stop().await;
    }
}
