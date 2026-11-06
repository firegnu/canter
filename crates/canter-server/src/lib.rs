//! A local HTTP/SSE daemon. Credentials and client environments are never logged.

pub mod chatgpt_auth;
mod config;
mod discovery;
mod http;
mod lifecycle;
#[cfg(test)]
mod lsp_tests;
mod model_auth;
mod model_catalog;
#[cfg(test)]
#[allow(dead_code)] // Shared fake server also records request timing for retry tests.
#[path = "../../canter-model/tests/support/mod.rs"]
mod model_test_server;
mod reload;
#[cfg(test)]
mod reload_tests;
#[cfg(test)]
mod responses_tests;
mod service_env;
mod session_locks;
mod startup;
mod trust;

pub use discovery::{Connection, discover, ensure_server, replace_server};
pub use http::doctor::offline as doctor_offline;
pub use lifecycle::{Registration, UPGRADE_GRACE};
pub use service_env::service_environment;

use std::{
    future::IntoFuture,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = env!("CANTER_VERSION");

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServerError(pub(crate) String);

pub fn state_directory() -> Result<PathBuf, ServerError> {
    let path = if let Some(path) = std::env::var_os("CANTER_HOME") {
        PathBuf::from(path)
    } else {
        PathBuf::from(std::env::var_os("HOME").ok_or(ServerError("HOME is not set".into()))?)
            .join(".canter")
    };
    if !path.is_absolute() {
        return Err(ServerError("CANTER_HOME must be an absolute path".into()));
    }
    Ok(path)
}

fn new_id() -> String {
    static IDS: AtomicU64 = AtomicU64::new(0);
    format!(
        "s-{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        IDS.fetch_add(1, Ordering::Relaxed)
    )
}

/// Runs the server until it is asked to exit. `version` is the line
/// `canter --version` prints; it is shown once the server accepts clients.
pub async fn serve(replace: bool, version: &str) -> Result<(), ServerError> {
    serve_with_lsp(replace, version, canter_tools::lsp::LspService::new).await
}

async fn serve_with_lsp(
    replace: bool,
    version: &str,
    make_lsp: impl FnOnce(
        canter_tools::lsp::LspConfig,
    )
        -> Result<Arc<canter_tools::lsp::LspService>, canter_tools::lsp::LspError>,
) -> Result<(), ServerError> {
    let home = state_directory()?;
    let (lease, config, summaries) = startup::initialize(home, replace).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| ServerError("cannot listen for shutdown signals".into()))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| ServerError("cannot listen for shutdown signals".into()))?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|_| ServerError("cannot listen for shutdown signals".into()))?;
    let auth = model_auth::ModelAuth::new(lease.home.clone());
    let model = config.model_client(Some(auth.clone()))?;
    let options = config.options();
    let lsp = make_lsp(config.lsp.clone()).map_err(|error| ServerError(error.to_string()))?;
    if let Ok(manager) = auth.get().await {
        auth.catalog().start(manager);
    }
    let startup_stop = CancellationToken::new();
    let connecting = summaries.finish_with(reload::LiveConfig::connect(
        lease.home.clone(),
        config.clone(),
        model.clone(),
        lsp.clone(),
        startup_stop.clone(),
        auth,
    ));
    tokio::pin!(connecting);
    let (reload, store) = tokio::select! {
        registry = &mut connecting => registry,
        _ = async { tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {}, _ = hangup.recv() => {} } } => {
            startup_stop.cancel();
            let (reload, _) = connecting.await;
            reload.shutdown().await;
            lsp.shutdown().await;
            return Ok(());
        }
    };
    let store = match store {
        Ok(store) => store,
        Err(error) => {
            reload.shutdown().await;
            lsp.shutdown().await;
            return Err(error);
        }
    };
    let app = Arc::new(http::App {
        reload: Some(reload.clone()),
        mcp: Default::default(),
        lsp,
        home: lease.home.clone(),
        store: Arc::new(store),
        model,
        options,
        default_model: config.model,
        models: config.models,
        sessions: Default::default(),
        session_locks: Default::default(),
        registry_changed: Default::default(),
        managed: Default::default(),
        #[cfg(test)]
        pauses: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    let listener = tokio::net::UnixListener::bind(&lease.registration.socket_path)
        .map_err(|_| ServerError("cannot listen on the Unix socket".into()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &lease.registration.socket_path,
            std::fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| ServerError("cannot set the socket permissions".into()))?;
    }
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish()?;
        Ok::<_, ServerError>(lease)
    })
    .await
    .map_err(|_| ServerError("registration failed".into()))??;
    // The socket is listening and its registration is published: clients can connect.
    // A closed log must not take the daemon down after it became reachable.
    let _ = writeln!(
        std::io::stdout(),
        "canter server ready · {version} · pid {}",
        std::process::id()
    );
    http::cleanup::start(Arc::downgrade(&app));
    let shutdown = app.clone();
    let watcher = reload.watch(Arc::downgrade(&app));
    let home = lease.home.clone();
    let registration = lease.registration.clone();
    let stopped = app.stopped.clone();
    let close_failed = Arc::new(AtomicBool::new(false));
    let failed = close_failed.clone();
    let server = axum::serve(listener, http::router(app)).with_graceful_shutdown(async move {
        let upgrade = async move {
            loop {
                let home = home.clone();
                let registration = registration.clone();
                if tokio::task::spawn_blocking(move || lifecycle::replacement_requested(&home, &registration)).await.unwrap_or(false) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        };
        let exit = async {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {}, _ = hangup.recv() => {} }
        };
        tokio::pin!(exit);
        let replacing = tokio::select! { _ = &mut exit => false, _ = upgrade => true };
        if let Some(reload) = &shutdown.reload { reload.stop_reloading(); }
        let sessions = shutdown.begin_drain().await;
        if replacing {
            let wait = async {
                loop {
                    let mut busy = false;
                    for session in &sessions {
                        busy |= session.snapshot().await.session.turns.iter().any(|turn| turn.status == canter_protocol::TurnStatus::InProgress);
                    }
                    if !busy { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            };
            tokio::select! {
                _ = &mut exit => {},
                _ = tokio::time::timeout(lifecycle::UPGRADE_GRACE, wait) => {},
            }
        }
        futures_util::future::join_all(sessions.iter().map(|session| async {
            if session.close_for_shutdown().await.is_err() {
                failed.store(true, Ordering::Relaxed);
                // P1-4 releases a failed writer on a subsequent close attempt.
                let _ = session.close_for_shutdown().await;
            }
        })).await;
        if let Some(reload) = &shutdown.reload { reload.shutdown().await; }
        shutdown.stopped.cancel();
    }).into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => { result.map_err(|_| ServerError("the HTTP server failed".into()))?; }
        _ = stopped.cancelled() => {
            // Sessions are already closed. A client stalled in an HTTP body must not hold the daemon lock forever.
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut server).await {
                result.map_err(|_| ServerError("the HTTP server failed".into()))?;
            }
        }
    }
    reload.shutdown().await;
    let _ = watcher.await;
    drop(lease);
    if close_failed.load(Ordering::Relaxed) {
        Err(ServerError(
            "some sessions failed to close; resume them to recover".into(),
        ))
    } else {
        Ok(())
    }
}
