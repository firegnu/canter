//! A local HTTP/SSE daemon. Credentials and client environments are never logged.

mod config;
mod discovery;
mod http;
mod lifecycle;

pub use discovery::{Connection, discover, ensure_server};
pub use lifecycle::{Registration, UPGRADE_GRACE};

use std::{
    future::IntoFuture,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServerError(pub(crate) String);

pub fn state_directory() -> Result<PathBuf, ServerError> {
    let path = if let Some(path) = std::env::var_os("CANTER_HOME") {
        PathBuf::from(path)
    } else {
        PathBuf::from(std::env::var_os("HOME").ok_or(ServerError("HOME 未设置".into()))?)
            .join(".canter")
    };
    if !path.is_absolute() {
        return Err(ServerError("CANTER_HOME 必须是绝对路径".into()));
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

pub async fn serve(replace: bool) -> Result<(), ServerError> {
    let home = state_directory()?;
    let (lease, config, store) = tokio::task::spawn_blocking(move || {
        let lease = lifecycle::Lease::acquire(home, replace)?;
        let config = config::Config::load(&lease.home)?;
        let store = canter_store::Store::new(lease.home.join("sessions"))
            .map_err(|_| ServerError("无法打开会话目录".into()))?;
        lifecycle::cleanup_creating(&lease.home.join("sessions"))?;
        Ok::<_, ServerError>((lease, config, store))
    })
    .await
    .map_err(|_| ServerError("初始化失败".into()))??;
    let app = Arc::new(http::App {
        home: lease.home.clone(),
        store: Arc::new(store),
        model: config.model_client()?,
        options: config.options(),
        default_model: config.model,
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    let listener = tokio::net::UnixListener::bind(&lease.registration.socket_path)
        .map_err(|_| ServerError("无法监听 Unix socket".into()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &lease.registration.socket_path,
            std::fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| ServerError("无法设置 socket 权限".into()))?;
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| ServerError("无法监听退出信号".into()))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| ServerError("无法监听退出信号".into()))?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|_| ServerError("无法监听退出信号".into()))?;
    let lease = tokio::task::spawn_blocking(move || {
        lease.publish()?;
        Ok::<_, ServerError>(lease)
    })
    .await
    .map_err(|_| ServerError("登记失败".into()))??;
    let shutdown = app.clone();
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
        *shutdown.draining.write().await = true;
        let sessions: Vec<_> = shutdown.sessions.lock().await.values().cloned().collect();
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
            if session.close().await.is_err() {
                failed.store(true, Ordering::Relaxed);
                // P1-4 releases a failed writer on a subsequent close attempt.
                let _ = session.close().await;
            }
        })).await;
        shutdown.stopped.cancel();
    }).into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => { result.map_err(|_| ServerError("HTTP 服务失败".into()))?; }
        _ = stopped.cancelled() => {
            // Sessions are already closed. A client stalled in an HTTP body must not hold the daemon lock forever.
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut server).await {
                result.map_err(|_| ServerError("HTTP 服务失败".into()))?;
            }
        }
    }
    drop(lease);
    if close_failed.load(Ordering::Relaxed) {
        Err(ServerError("部分会话关闭失败，请重新接着聊以恢复".into()))
    } else {
        Ok(())
    }
}
