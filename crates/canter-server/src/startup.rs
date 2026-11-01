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
) -> Result<(Lease, Config, Store), ServerError> {
    initialize_with(home, replace, open_and_warm).await
}

async fn initialize_with(
    home: PathBuf,
    replace: bool,
    warm: impl FnOnce(&Path) -> Result<Store, ServerError> + Send + 'static,
) -> Result<(Lease, Config, Store), ServerError> {
    // Resolve once for both parallel tasks, as the old post-lease Store did.
    // Retargeting a home symlink must not move the running server's storage.
    let home = tokio::task::spawn_blocking(move || {
        lifecycle::private_dir(&home)?;
        std::fs::canonicalize(home).map_err(|_| ServerError("invalid state directory".into()))
    })
    .await
    .map_err(|_| ServerError("initialization failed".into()))??;
    let reading_home = home.clone();
    // Readers need no daemon lock. Overlap the full replay with replacement's
    // bounded lock wait, rather than spending its remaining startup allowance.
    // Join both tasks even on errors so no initialization work is left detached.
    let (prepared, warmed) = tokio::join!(
        tokio::task::spawn_blocking(move || {
            let lease = Lease::acquire(home, replace)?;
            let config = Config::load(&lease.home)?;
            Ok::<_, ServerError>((lease, config))
        }),
        tokio::task::spawn_blocking(move || warm(&reading_home))
    );
    let (lease, config) = prepared.map_err(|_| ServerError("initialization failed".into()))??;
    let store = warmed.map_err(|_| ServerError("initialization failed".into()))??;
    tokio::task::spawn_blocking(move || {
        lifecycle::cleanup_creating(&lease.home.join("sessions"))?;
        // The old daemon may have appended while we waited. Only changed files
        // replay; this finishes before binding/publishing the new HTTP server.
        warm_summaries(&store);
        Ok((lease, config, store))
    })
    .await
    .map_err(|_| ServerError("initialization failed".into()))?
}
