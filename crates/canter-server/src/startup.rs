use std::path::{Path, PathBuf};

use canter_store::Store;

use crate::{
    ServerError,
    config::Config,
    lifecycle::{self, Lease},
};

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;

fn open_and_warm(home: &Path) -> Result<Store, ServerError> {
    let store = Store::new(home.join("sessions"))
        .map_err(|_| ServerError("cannot open the sessions directory".into()))?;
    warm_summaries(&store);
    Ok(store)
}

pub(crate) fn warm_summaries(store: &Store) {
    // A failed optimization must not add a startup failure: list already skips
    // broken sessions, and root read errors can be retried by a later request.
    let _ = store.list();
}

pub(crate) async fn initialize(
    home: PathBuf,
    replace: bool,
) -> Result<(Lease, Config, Summaries), ServerError> {
    initialize_with(home, replace, open_and_warm).await
}

async fn initialize_with(
    home: PathBuf,
    replace: bool,
    warm: impl FnOnce(&Path) -> Result<Store, ServerError> + Send + 'static,
) -> Result<(Lease, Config, Summaries), ServerError> {
    // Resolve once for both parallel tasks, as the old post-lease Store did.
    // Retargeting a home symlink must not move the running server's storage.
    let home = tokio::task::spawn_blocking(move || {
        lifecycle::private_dir(&home)?;
        std::fs::canonicalize(home).map_err(|_| ServerError("invalid state directory".into()))
    })
    .await
    .map_err(|_| ServerError("initialization failed".into()))??;
    tokio::task::spawn_blocking(move || {
        let mut warming = None;
        let lease = if replace {
            let reading_home = home.clone();
            Lease::acquire_with_wait(home.clone(), true, || {
                warming = Some(tokio::task::spawn_blocking(move || warm(&reading_home)));
            })?
        } else {
            Lease::acquire(home.clone(), false)?
        };
        let config = Config::load(&lease.home)?;
        Ok((lease, config, Summaries { home, warming }))
    })
    .await
    .map_err(|_| ServerError("initialization failed".into()))?
}

pub(crate) struct Summaries {
    home: PathBuf,
    warming: Option<tokio::task::JoinHandle<Result<Store, ServerError>>>,
}

impl Summaries {
    async fn finish(self) -> Result<Store, ServerError> {
        let warmed = match self.warming {
            Some(task) => Some(
                task.await
                    .map_err(|_| ServerError("initialization failed".into()))??,
            ),
            None => None,
        };
        tokio::task::spawn_blocking(move || {
            let store = match warmed {
                Some(store) => store,
                None => Store::new(self.home.join("sessions"))
                    .map_err(|_| ServerError("cannot open the sessions directory".into()))?,
            };
            lifecycle::cleanup_creating(&self.home.join("sessions"))?;
            // The replacement owns the daemon lease now. Earlier prewarming
            // may overlap the old process and must remain strictly read-only.
            canter_core::recover_agent_sessions(&store)
                .map_err(|_| ServerError("cannot recover interrupted child sessions".into()))?;
            // First replay for ordinary startup, or refresh changes made by the
            // old daemon during replacement. Both overlap MCP initialization.
            warm_summaries(&store);
            Ok(store)
        })
        .await
        .map_err(|_| ServerError("initialization failed".into()))?
    }

    pub(crate) async fn finish_with<T>(
        self,
        connecting: impl std::future::Future<Output = T>,
    ) -> (T, Result<Store, ServerError>) {
        tokio::join!(connecting, self.finish())
    }
}
