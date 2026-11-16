use super::*;
mod git;
mod review;
#[cfg(test)]
mod review_tests;

// Rendering tests exercise the moved collectors through a disposable HTTP
// fixture. Process isolation and session lookup are tested against the real server.
#[cfg(feature = "test-support")]
pub mod test_support {
    use super::*;
    pub fn branch(cwd: &str) -> Option<String> {
        git::branch(cwd)
    }
    pub async fn summary(
        cwd: &str,
        env: &BTreeMap<String, String>,
        count: bool,
    ) -> WorkspaceSummary {
        WorkspaceSummary {
            branch: git::branch(cwd),
            changed_lines: if count {
                git::changed_lines(cwd, env).await
            } else {
                None
            },
        }
    }
    pub async fn diff(cwd: &str, env: &BTreeMap<String, String>) -> Result<WorkspaceDiff, String> {
        let local = git::local(cwd, env).await?;
        Ok(WorkspaceDiff {
            root: local.root,
            staged: local.staged,
            unstaged: local.unstaged,
            untracked: local.untracked,
        })
    }
    pub async fn status(cwd: &str, env: &BTreeMap<String, String>) -> WorkspaceStatus {
        match git::status(cwd, env.get("HOME").map(String::as_str), env).await {
            git::Status::Repository { branch, entries } => {
                WorkspaceStatus::Repository { branch, entries }
            }
            git::Status::Unavailable(reason) => WorkspaceStatus::Unavailable {
                reason: reason.into(),
            },
        }
    }
    pub async fn review(
        cwd: &str,
        env: &BTreeMap<String, String>,
        target: &str,
        client: &str,
    ) -> Result<SessionInputParams, String> {
        super::review::prepare(cwd, env, target, client).await
    }
    pub fn entries(
        dirs: &[String],
        send: impl FnMut(WorkspaceEntriesBatch) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        crate::workspace_walk::scan(dirs, send)
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SummaryParams {
    changed_lines: Option<bool>,
}

pub(super) async fn summary(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Query<SummaryParams>, QueryRejection>,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    let (settings, env) = app.session(&id).await?.workspace_context().await;
    let cwd = settings.cwd;
    let branch_cwd = cwd.clone();
    let branch = tokio::task::spawn_blocking(move || git::branch(&branch_cwd));
    let changed_lines = if params.changed_lines.unwrap_or(true) {
        git::changed_lines(&cwd, &env).await
    } else {
        None
    };
    Ok(Json(WorkspaceSummary {
        branch: branch.await.map_err(|_| ApiError::internal())?,
        changed_lines,
    }))
}

fn display_path(path: &std::path::Path, cwd: &str, home: Option<&str>) -> String {
    if let Ok(relative) = path.strip_prefix(cwd) {
        return relative.to_string_lossy().into_owned();
    }
    let text = path.to_string_lossy();
    if let Some(home) = home
        && let Some(rest) = text.strip_prefix(home.trim_end_matches('/'))
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("~{rest}");
    }
    text.into_owned()
}

pub(super) async fn diff(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<WorkspaceDiff>, ApiError> {
    let (settings, env) = app.session(&id).await?.workspace_context().await;
    let local = tokio::time::timeout(Duration::from_secs(15), git::local(&settings.cwd, &env))
        .await
        .unwrap_or_else(|_| Err("collecting changes timed out".into()))
        .map_err(|error| ApiError::new(ErrorCode::InvalidRequest, &error))?;
    Ok(Json(WorkspaceDiff {
        root: local.root,
        staged: local.staged,
        unstaged: local.unstaged,
        untracked: local.untracked,
    }))
}

pub(super) async fn status(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<WorkspaceStatus>, ApiError> {
    let (settings, env) = app.session(&id).await?.workspace_context().await;
    let status = match git::status(&settings.cwd, env.get("HOME").map(String::as_str), &env).await {
        git::Status::Repository { branch, entries } => {
            WorkspaceStatus::Repository { branch, entries }
        }
        git::Status::Unavailable(reason) => WorkspaceStatus::Unavailable {
            reason: reason.into(),
        },
    };
    Ok(Json(status))
}

pub(super) async fn review(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
    params: Result<Query<WorkspaceReviewParams>, QueryRejection>,
) -> Result<Json<SessionInputParams>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    let (settings, env) = app.session(&id).await?.workspace_context().await;
    review::prepare(&settings.cwd, &env, &params.target, &params.client_name)
        .await
        .map(Json)
        .map_err(|error| ApiError::new(ErrorCode::InvalidRequest, &error))
}

pub(super) async fn entries(
    State(app): State<Arc<App>>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, ApiError> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (settings, env) = app.session(&id).await?.workspace_context().await;
    let directories: Vec<_> = std::iter::once(settings.cwd.clone())
        .chain(settings.additional_directories)
        .collect();
    let mut child =
        tokio::process::Command::new(std::env::current_exe().map_err(|_| ApiError::internal())?)
            .arg(crate::WORKSPACE_WORKER_ARGUMENT)
            .env_clear()
            .envs(env)
            .current_dir(&settings.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                ApiError::new(ErrorCode::Internal, "could not start workspace file scan")
            })?;
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&directories).map_err(|_| ApiError::internal())?)
        .await
        .map_err(|_| ApiError::internal())?;
    drop(input);
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let stream = async_stream::stream! {
        let mut done = false;
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(batch) = serde_json::from_str::<WorkspaceEntriesBatch>(&line) else { break; };
            done = batch.done;
            yield Ok::<_, Infallible>(axum::response::sse::Event::default().json_data(batch).unwrap());
            if done { break; }
        }
        if !done {
            let batch = WorkspaceEntriesBatch { entries: Vec::new(), truncated: false, done: true, error: Some("Could not scan workspace files".into()) };
            yield Ok(axum::response::sse::Event::default().json_data(batch).unwrap());
        }
        // kill_on_drop also stops and reaps the worker if the client leaves mid-walk.
        if done { let _ = child.wait().await; }
    };
    Ok(axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
}
