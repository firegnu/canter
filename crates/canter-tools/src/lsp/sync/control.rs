use super::*;
use canter_protocol::{LanguageServer, LanguageServerState};

pub(super) struct Subscription {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub file: PathBuf,
    pub stopped: bool,
    pub cancel: CancellationToken,
    pub error: Option<String>,
}
impl SessionInner {
    pub(super) fn subscription(&self, path: &Path) -> Result<CancellationToken, LspError> {
        let context = self.context.lock().unwrap().clone();
        let (name, root) = self
            .service
            .selection_info(path, &context)
            .or_else(|error| {
                if !matches!(path.try_exists(), Ok(false)) {
                    return Err(error);
                }
                let state = self.state.lock().unwrap();
                state
                    .workspaces
                    .values()
                    .find(|workspace| {
                        path.starts_with(&workspace.instance.root) && workspace.matches(path)
                    })
                    .map(|workspace| {
                        (
                            workspace.instance.server_id.clone(),
                            workspace.instance.root.clone(),
                        )
                    })
                    .or_else(|| {
                        state
                            .subscriptions
                            .iter()
                            .find(|s| s.file == path)
                            .map(|s| (s.name.clone(), s.root.clone()))
                    })
                    .ok_or(error)
            })?;
        let mut state = self.state.lock().unwrap();
        if let Some(subscription) = state
            .subscriptions
            .iter_mut()
            .find(|s| s.name == name && s.root == root)
        {
            if subscription.stopped {
                return Err(LspError::new(
                    ErrorKind::Stopped,
                    "LSP subscription is stopped; use retry to resume",
                ));
            }
            subscription.file = path.into();
            return Ok(subscription.cancel.clone());
        }
        if state.subscriptions.len() >= 1024 {
            return Err(LspError::new(
                ErrorKind::ResourceLimited,
                "LSP subscription limit exceeded",
            ));
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let cancel = self.stop.child_token();
        let id = state
            .restored_ids
            .remove(&(name.clone(), root.clone()))
            .unwrap_or_else(|| {
                format!(
                    "lsp-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )
            });
        state.subscriptions.push(Subscription {
            id,
            name,
            root,
            file: path.into(),
            stopped: false,
            cancel: cancel.clone(),
            error: None,
        });
        Ok(cancel)
    }
}
impl LspSession {
    pub fn restore_subscriptions(&self, servers: &[LanguageServer], files: &[PathBuf]) {
        let context = self.0.context.lock().unwrap().clone();
        for path in files {
            let Ok((name, root)) = self.0.service.selection_info(path, &context) else {
                continue;
            };
            let Some(old) = servers
                .iter()
                .find(|s| s.name == name && Path::new(&s.root) == root)
            else {
                continue;
            };
            if self.0.subscription(path).is_ok() && old.state == LanguageServerState::Stopped {
                let mut state = self.0.state.lock().unwrap();
                if let Some(subscription) = state.subscriptions.iter_mut().find(|s| s.id == old.id)
                {
                    subscription.stopped = true;
                    subscription.cancel.cancel();
                }
            }
        }
    }
    pub fn restore_server_ids(&self, servers: &[LanguageServer]) {
        self.0.state.lock().unwrap().restored_ids = servers
            .iter()
            .map(|s| ((s.name.clone(), PathBuf::from(&s.root)), s.id.clone()))
            .collect();
    }
    pub fn enabled(&self) -> bool {
        self.0.service.enabled()
    }
    pub fn server_id(&self, name: &str, root: &Path) -> String {
        self.0
            .state
            .lock()
            .unwrap()
            .subscriptions
            .iter()
            .find(|s| s.name == name && s.root == root)
            .map(|s| s.id.clone())
            .unwrap_or_default()
    }
    pub fn language_servers(&self) -> Vec<LanguageServer> {
        let context = self.0.context.lock().unwrap().clone();
        // Query the pool outside the session lock (pool queries count shared sessions).
        let subscriptions: Vec<_> = self
            .0
            .state
            .lock()
            .unwrap()
            .subscriptions
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    s.name.clone(),
                    s.root.clone(),
                    s.stopped,
                    s.error.clone(),
                )
            })
            .collect();
        subscriptions
            .into_iter()
            .map(|(id, name, root, stopped, error)| {
                let (mut state, mut reason, rss_bytes, shared_references) =
                    self.0.service.describe(&name, &root, &context);
                if stopped {
                    state = LanguageServerState::Stopped;
                    reason = Some("LSP subscription is stopped; use retry to resume".into());
                } else if state == LanguageServerState::Reconnecting && error.is_some() {
                    state = LanguageServerState::Unavailable;
                    reason = error;
                }
                LanguageServer {
                    id,
                    name,
                    root: root.display().to_string(),
                    state,
                    reason,
                    rss_bytes,
                    shared_references,
                    updated_at: 0,
                }
            })
            .collect()
    }
    pub async fn stop_server(&self, id: &str) -> Result<(), LspError> {
        let context = self.0.context.lock().unwrap().clone();
        let mut state = self.0.state.lock().unwrap();
        let Some(subscription) = state.subscriptions.iter_mut().find(|s| s.id == id) else {
            return Err(LspError::new(
                ErrorKind::Unsupported,
                "Unknown LSP subscription",
            ));
        };
        subscription.stopped = true;
        subscription.cancel.cancel();
        let (name, root) = (subscription.name.clone(), subscription.root.clone());
        let paths: Vec<_> = state
            .interests
            .iter()
            .filter(|p| {
                self.0.service.selection_info(p, &context).is_ok_and(
                    |(selected_name, selected_root)| selected_name == name && selected_root == root,
                )
            })
            .cloned()
            .collect();
        for path in paths {
            state.delivery.clear_file(&path);
            state.pending.remove(&path);
            if let Some(reply) = state.tickets.remove(&path) {
                reply.send_replace(Some(Err(LspError::new(
                    ErrorKind::Stopped,
                    "LSP subscription is stopped; use retry to resume",
                ))));
            }
        }
        state
            .workspaces
            .retain(|_, w| w.instance.server_id != name || w.instance.root != root);
        Ok(())
    }
    pub async fn retry_server(&self, id: &str) -> Result<(), LspError> {
        let file = {
            let mut state = self.0.state.lock().unwrap();
            let Some(subscription) = state.subscriptions.iter_mut().find(|s| s.id == id) else {
                return Err(LspError::new(
                    ErrorKind::Unsupported,
                    "Unknown LSP subscription",
                ));
            };
            subscription.stopped = false;
            subscription.cancel = self.0.stop.child_token();
            subscription.error = None;
            subscription.file.clone()
        };
        let context = self.0.context.lock().unwrap().clone();
        self.0.service.retry(&file, &context).await?;
        if context.round == Round::Modify {
            self.changed(&file);
        }
        Ok(())
    }
}
