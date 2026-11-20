//! The UI still tests real repository results, now across HTTP. The real server's
//! session lookup, authentication and scan worker have separate integration coverage.
#![allow(dead_code)]
use axum::{
    Json, Router,
    extract::Query,
    response::{IntoResponse, Response},
    routing::get,
};
use canter_protocol::*;
use canter_server::workspace_test_support as queries;
use canter_tui::{App, client::Client};
use std::{collections::BTreeMap, sync::OnceLock};

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    })
}

pub struct Server {
    pub client: Client,
    task: tokio::task::JoinHandle<()>,
    directories: std::sync::Arc<std::sync::RwLock<Vec<String>>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn result<T: serde::Serialize>(value: Result<T, String>) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(message) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"code":"invalidRequest","message":message})),
        )
            .into_response(),
    }
}

impl Server {
    pub fn new(cwd: &str, extras: &[String], home: Option<&str>) -> Self {
        let mut env = BTreeMap::from([("PATH".into(), std::env::var("PATH").unwrap())]);
        if let Some(home) = home {
            env.insert("HOME".into(), home.into());
        }
        Self::with_env(cwd, extras, env)
    }
    pub fn with_env(cwd: &str, extras: &[String], env: BTreeMap<String, String>) -> Self {
        let _enter = runtime().enter();
        let directories = std::sync::Arc::new(std::sync::RwLock::new(
            std::iter::once(cwd.to_owned())
                .chain(extras.iter().cloned())
                .collect::<Vec<_>>(),
        ));
        let dirs = directories.clone();
        let cwd = cwd.to_owned();
        let summary_cwd = cwd.clone();
        let summary_env = env.clone();
        let diff_cwd = cwd.clone();
        let diff_env = env.clone();
        let status_cwd = cwd.clone();
        let status_env = env.clone();
        let router = Router::new()
            .route(routes::INFO, get(|| async { Json(serde_json::json!({"features":["workspaceQueries","review"],"protocolVersion":1,"serverVersion":"test","sessions":[]})) }))
            .route(routes::WORKSPACE_SUMMARY, get(move |Query(params): Query<BTreeMap<String,String>>| {
                let cwd = summary_cwd.clone(); let env = summary_env.clone();
                async move { Json(queries::summary(&cwd, &env, params.get("changedLines").is_none_or(|v| v != "false")).await) }
            }))
            .route(routes::WORKSPACE_DIFF, get(move || { let cwd = diff_cwd.clone(); let env = diff_env.clone(); async move { result(queries::diff(&cwd, &env).await) } }))
            .route(routes::WORKSPACE_STATUS, get(move || { let cwd = status_cwd.clone(); let env = status_env.clone(); async move { Json(queries::status(&cwd, &env).await) } }))
            .route(routes::WORKSPACE_REVIEW, get(move |Query(params): Query<WorkspaceReviewParams>| { let cwd = cwd.clone(); let env = env.clone(); async move { result(queries::review(&cwd, &env, &params.target, &params.client_name).await) } }))
            .route(routes::WORKSPACE_ENTRIES, get(move || {
                let dirs = dirs.read().unwrap().clone();
                async move {
                    let (sender, receiver) = tokio::sync::mpsc::channel(2);
                    tokio::task::spawn_blocking(move || {
                        let _ = queries::entries(&dirs, |batch| sender.blocking_send(batch).map_err(std::io::Error::other));
                    });
                    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
                        receiver.recv().await.map(|batch| (Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().json_data(batch).unwrap()), receiver))
                    });
                    axum::response::Sse::new(stream)
                }
            }));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let task = runtime().spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            client: Client::remote("fixture", &format!("http://{address}"), "test-only").unwrap(),
            task,
            directories,
        }
    }
    pub fn set_directories(&self, settings: &SessionSettings) {
        *self.directories.write().unwrap() = std::iter::once(settings.cwd.clone())
            .chain(settings.additional_directories.clone())
            .collect();
    }
    pub fn attach(&self, mut app: App) -> App {
        let summary = WorkspaceSummary {
            branch: queries::branch(&app.snapshot().settings.cwd),
            changed_lines: None,
            untracked_lines: None,
        };
        app.set_workspace_summary(summary);
        app.with_workspace(self.client.clone())
    }
}

pub fn branch(mut app: App) -> App {
    app.set_workspace_summary(WorkspaceSummary {
        branch: queries::branch(&app.snapshot().settings.cwd),
        changed_lines: None,
        untracked_lines: None,
    });
    app
}

pub async fn files(app: &mut App, home: Option<&str>) {
    let settings = &app.snapshot().settings;
    let server = Server::new(&settings.cwd, &settings.additional_directories, home);
    let status = server
        .client
        .workspace_status(&app.snapshot().id)
        .await
        .unwrap();
    app.show_files(status);
}
