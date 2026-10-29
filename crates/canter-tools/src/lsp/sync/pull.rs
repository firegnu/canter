use super::*;
impl Workspace {
    pub(super) async fn refresh(self: &Arc<Self>) {
        let paths: Vec<_> = self.documents.lock().await.keys().cloned().collect();
        for path in paths {
            self.pull(&path).await;
        }
    }
    pub(super) async fn pull(self: &Arc<Self>, path: &Path) {
        let provider = &self.instance.capabilities["diagnosticProvider"];
        if !provider.is_object() || !self.allowed(path) {
            return;
        }
        let mut documents = self.documents.lock().await;
        let Some(doc) = documents.get_mut(path) else {
            return;
        };
        doc.cancel.cancel();
        doc.cancel = self.stop.child_token();
        let cancel = doc.cancel.clone();
        let (version, hash, previous) = (doc.version, doc.digest, doc.result_id.clone());
        let identifier = provider["identifier"].as_str().map(str::to_owned);
        let source = format!("pull:{}", identifier.as_deref().unwrap_or(""));
        let Ok(uri) = uri(path) else {
            return;
        };
        let mut params = json!({"textDocument":{"uri":uri}});
        if let Some(id) = identifier {
            params["identifier"] = json!(id);
        }
        if let Some(previous) = previous {
            params["previousResultId"] = json!(previous);
        }
        let instance = self.instance.clone();
        let weak = Arc::downgrade(self);
        let path = path.to_owned();
        tokio::spawn(async move {
            let result = instance
                .request(
                    "textDocument/diagnostic",
                    params,
                    Duration::from_secs(30),
                    &cancel,
                )
                .await;
            if cancel.is_cancelled() {
                return;
            }
            let Some(workspace) = weak.upgrade() else {
                return;
            };
            let mut documents = workspace.documents.lock().await;
            let Some(doc) = documents
                .get_mut(&path)
                .filter(|d| d.version == version && d.digest == hash)
            else {
                return;
            };
            if !instance.is_running()
                || !read_text(&path)
                    .await
                    .is_ok_and(|text| digest(&text) == hash)
            {
                return;
            }
            match result {
                Ok(result) => {
                    let mut incomplete = false;
                    let diagnostics = match result["kind"].as_str() {
                        Some("full") => result["items"].as_array().cloned(),
                        Some("unchanged") => workspace
                            .sessions
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(Weak::upgrade)
                            .find_map(|session| {
                                session
                                    .state
                                    .lock()
                                    .unwrap()
                                    .delivery
                                    .cached()
                                    .into_iter()
                                    .find(|b| {
                                        b.path == path
                                            && b.source == source
                                            && b.generation == instance.generation()
                                    })
                                    .map(|b| {
                                        incomplete = b.incomplete;
                                        b.diagnostics
                                    })
                            }),
                        _ => None,
                    };
                    let Some(diagnostics) = diagnostics else {
                        drop(documents);
                        workspace
                            .mark_stale("LSP returned an invalid or unavailable diagnostic report")
                            .await;
                        return;
                    };
                    doc.result_id = result["resultId"]
                        .as_str()
                        .filter(|id| id.len() <= 4096)
                        .map(str::to_owned);
                    let batch = DiagnosticBatch {
                        id: 0,
                        path,
                        server_id: instance.server_id.clone(),
                        generation: instance.generation(),
                        version: Some(version),
                        digest: hash,
                        source,
                        diagnostics,
                        freshness: Freshness::Current,
                        incomplete,
                        reason: incomplete
                            .then(|| "Previous LSP diagnostic report was incomplete".into()),
                    };
                    for session in workspace
                        .sessions
                        .lock()
                        .unwrap()
                        .iter()
                        .filter_map(Weak::upgrade)
                    {
                        session.deliver(batch.clone());
                    }
                }
                Err(error) => {
                    drop(documents);
                    workspace.mark_stale(&error.message).await;
                }
            }
        });
    }
}
