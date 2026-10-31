use crate::{ServerError, config::Config, http::App};
use canter_core::{Configuration, ConfigurationSource, CoreError, CoreOptions};
use canter_model::Model;
use canter_tools::{lsp::LspService, mcp::Registry};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

enum Service {
    Mcp(Registry),
    Lsp(Arc<LspService>),
}
impl Service {
    async fn shutdown(&self) {
        match self {
            Self::Mcp(mcp) => mcp.shutdown().await,
            Self::Lsp(lsp) => lsp.shutdown().await,
        }
    }
}
struct ServiceSlot {
    ready: watch::Sender<Option<Arc<Service>>>,
    users: Arc<()>,
    retiring: Mutex<()>,
    closed: AtomicBool,
}
impl ServiceSlot {
    fn new(service: Option<Service>) -> Arc<Self> {
        Arc::new(Self {
            ready: watch::channel(service.map(Arc::new)).0,
            users: Arc::new(()),
            retiring: Mutex::new(()),
            closed: AtomicBool::new(false),
        })
    }
    async fn get(&self, stop: &CancellationToken) -> Result<Arc<Service>, CoreError> {
        let mut ready = self.ready.subscribe();
        loop {
            if let Some(service) = ready.borrow_and_update().clone() {
                return Ok(service);
            }
            tokio::select! {
                _ = stop.cancelled() => return Err(CoreError { code: canter_protocol::ErrorCode::Conflict, message: "server is stopping".into() }),
                _ = ready.changed() => {},
            }
        }
    }
    async fn retire(&self, stop: &CancellationToken) -> Result<(), CoreError> {
        let _retiring = self.retiring.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let service = self.get(stop).await?;
        while Arc::strong_count(&self.users) > 1 {
            tokio::select! { _ = stop.cancelled() => return Err(CoreError {code: canter_protocol::ErrorCode::Conflict, message: "server is stopping".into()}), _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
        }
        service.shutdown().await;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
struct Version {
    revision: u64,
    config: Config,
    options: CoreOptions,
    model: Arc<dyn Model>,
    mcp: BTreeMap<String, Arc<ServiceSlot>>,
    lsp: Arc<ServiceSlot>,
}
// Only active turns hold these counters. Idle sessions retain their tool descriptions,
// but cannot keep a retired service running indefinitely.
struct Lease {
    _users: Vec<Arc<()>>,
}

pub(crate) struct LiveConfig {
    home: PathBuf,
    current: RwLock<Arc<Version>>,
    slots: Mutex<Vec<Arc<ServiceSlot>>>,
    // Retain pending removals too: removing then re-adding the same name must
    // still wait for the old process to finish shutting down.
    mcp_lineage: Mutex<BTreeMap<String, Arc<ServiceSlot>>>,
    workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    stop: CancellationToken,
    warning: Mutex<Option<String>>,
}
impl LiveConfig {
    pub async fn connect(
        home: PathBuf,
        config: Config,
        model: Arc<dyn Model>,
        lsp: Arc<LspService>,
        stop: CancellationToken,
    ) -> Arc<Self> {
        let mcp: BTreeMap<_, _> =
            futures_util::future::join_all(config.mcp_servers.iter().map(|(name, config)| {
                let name = name.clone();
                let config = config.clone();
                let home = home.clone();
                let stop = stop.child_token();
                async move {
                    let registry =
                        Registry::connect(BTreeMap::from([(name.clone(), config)]), home, stop)
                            .await;
                    (name, ServiceSlot::new(Some(Service::Mcp(registry))))
                }
            }))
            .await
            .into_iter()
            .collect();
        let lsp = ServiceSlot::new(Some(Service::Lsp(lsp)));
        let slots = mcp
            .values()
            .cloned()
            .chain(std::iter::once(lsp.clone()))
            .collect();
        let mut options = config.options();
        options.command_rules_path = Some(home.join("rules.json"));
        Arc::new(Self {
            home,
            mcp_lineage: Mutex::new(mcp.clone()),
            current: RwLock::new(Arc::new(Version {
                revision: 0,
                config,
                options,
                model,
                mcp,
                lsp,
            })),
            slots: Mutex::new(slots),
            workers: Mutex::new(Vec::new()),
            stop: CancellationToken::new(),
            warning: Mutex::new(None),
        })
    }

    pub fn metadata(&self) -> (String, Vec<String>, CoreOptions) {
        let version = self.current.read().unwrap();
        (
            version.config.model.clone(),
            version.config.models.clone(),
            version.options.clone(),
        )
    }

    pub fn mcp_status(&self) -> Vec<canter_protocol::McpServerStatus> {
        let slots = self.current.read().unwrap().mcp.clone();
        let mut services = Vec::new();
        let mut pending = Vec::new();
        for (name, slot) in slots {
            if let Some(service) = slot.ready.borrow().clone() {
                services.push(service);
            } else {
                pending.push(canter_protocol::McpServerStatus {
                    name,
                    connected: false,
                    error: Some("Starting; waiting for service handover or initialization".into()),
                    tools: Vec::new(),
                    warnings: Vec::new(),
                });
            }
        }
        let mut status = Registry::status_all(
            &services
                .iter()
                .filter_map(|service| match &**service {
                    Service::Mcp(registry) => Some(registry),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        );
        status.extend(pending);
        status.sort_by(|a, b| a.name.cmp(&b.name));
        status
    }

    pub async fn warning(&self) -> Option<String> {
        self.warning.lock().await.clone()
    }

    pub fn watch(self: &Arc<Self>, app: Weak<App>) -> tokio::task::JoinHandle<()> {
        let live = self.clone();
        tokio::spawn(async move {
            let read = |path| {
                tokio::task::spawn_blocking(move || {
                    std::fs::read(path).map_err(|error| error.kind())
                })
            };
            let path = live.home.join("config.json");
            let mut observed = Ok(live.current.read().unwrap().config.contents.clone());
            let mut due = None;
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { _ = live.stop.cancelled() => break, _ = ticks.tick() => {} }
                let Ok(bytes) = read(path.clone()).await else {
                    continue;
                };
                if bytes != observed {
                    observed = bytes;
                    due = Some(tokio::time::Instant::now() + Duration::from_millis(200));
                }
                if due.is_none_or(|due| due > tokio::time::Instant::now()) {
                    continue;
                }
                due = None;
                let home = live.home.clone();
                let result = tokio::task::spawn_blocking(move || Config::load(&home)).await;
                // Do not install a read that raced another save, including atomic rename.
                if read(path.clone()).await.ok().as_ref() != Some(&observed) {
                    continue;
                }
                let result = match result {
                    Ok(Ok(config)) => live.install(config).await,
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(ServerError("cannot read config.json".into())),
                };
                let message = {
                    let mut warning = live.warning.lock().await;
                    match result {
                        Ok(()) => warning.take().map(|_| {
                            "config.json recovered; new settings apply to the next turn.".into()
                        }),
                        Err(error) => {
                            let message = format!(
                                "Could not reload config.json: {error}. Keeping the previous configuration."
                            );
                            if warning.as_ref() == Some(&message) {
                                None
                            } else {
                                *warning = Some(message.clone());
                                Some(message)
                            }
                        }
                    }
                };
                if let Some(message) = message {
                    let Some(app) = app.upgrade() else {
                        break;
                    };
                    let sessions: Vec<_> = app.sessions.lock().await.values().cloned().collect();
                    for session in sessions {
                        let _ = session.configuration_warning(message.clone()).await;
                    }
                }
            }
        })
    }

    async fn install(&self, config: Config) -> Result<(), ServerError> {
        let model = config.model_client()?;
        let mut options = config.options();
        options.command_rules_path = Some(self.home.join("rules.json"));
        let old = self.current.read().unwrap().clone();
        let mut mcp = BTreeMap::new();
        let mut lineage = self.mcp_lineage.lock().await;
        lineage.retain(|name, slot| {
            config.mcp_servers.contains_key(name) || !slot.closed.load(Ordering::Acquire)
        });
        let mut replacements = Vec::new();
        for (name, server) in &config.mcp_servers {
            if old.config.mcp_servers.get(name) == Some(server) {
                mcp.insert(name.clone(), old.mcp[name].clone());
                continue;
            }
            let slot = ServiceSlot::new(None);
            let previous = lineage.insert(name.clone(), slot.clone());
            replacements.push((slot.clone(), previous, Some((name.clone(), server.clone()))));
            mcp.insert(name.clone(), slot);
        }
        let lsp = if old.config.lsp == config.lsp {
            old.lsp.clone()
        } else {
            let slot = ServiceSlot::new(None);
            // A disabled version may still be waiting for an older active service.
            replacements.push((slot.clone(), Some(old.lsp.clone()), None));
            slot
        };
        let next = Arc::new(Version {
            revision: old.revision + 1,
            options,
            model,
            config,
            mcp,
            lsp,
        });
        *self.current.write().unwrap() = next.clone();
        drop(lineage);
        let mut workers = self.workers.lock().await;
        workers.retain(|worker| !worker.is_finished());
        for (name, slot) in &old.mcp {
            if !next.mcp.contains_key(name) {
                let slot = slot.clone();
                let stop = self.stop.clone();
                workers.push(tokio::spawn(async move {
                    let _ = slot.retire(&stop).await;
                }));
            }
        }
        let mut slots = self.slots.lock().await;
        slots.retain(|slot| !slot.closed.load(Ordering::Acquire));
        for (slot, previous, mcp_config) in replacements {
            slots.push(slot.clone());
            let home = self.home.clone();
            let stop = self.stop.clone();
            let lsp_config = next.config.lsp.clone();
            workers.push(tokio::spawn(async move {
                if let Some(previous) = previous
                    && previous.retire(&stop).await.is_err()
                {
                    return;
                }
                if stop.is_cancelled() {
                    return;
                }
                let service = if let Some((name, config)) = mcp_config {
                    let connect_stop = CancellationToken::new();
                    let connecting = Registry::connect(
                        BTreeMap::from([(name, config)]),
                        home,
                        connect_stop.clone(),
                    );
                    tokio::pin!(connecting);
                    let mcp = tokio::select! {
                        registry = &mut connecting => registry,
                        _ = stop.cancelled() => {
                            connect_stop.cancel();
                            connecting.await.shutdown().await;
                            return;
                        }
                    };
                    Service::Mcp(mcp)
                } else {
                    Service::Lsp(LspService::new(lsp_config).expect("validated LSP configuration"))
                };
                if stop.is_cancelled() {
                    service.shutdown().await;
                } else {
                    slot.ready.send_replace(Some(Arc::new(service)));
                }
            }));
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        for worker in self.workers.lock().await.drain(..) {
            let _ = worker.await;
        }
        for slot in self.slots.lock().await.drain(..) {
            let service = slot.ready.borrow().clone();
            if let Some(service) = service {
                service.shutdown().await;
            }
        }
    }

    pub fn stop_reloading(&self) {
        self.stop.cancel();
    }
}

impl ConfigurationSource for LiveConfig {
    fn snapshot(&self) -> Configuration {
        // Pin the published slots while taking the ready subset. This snapshot
        // is also used to open a session or answer privately during a handover.
        let version = self.current.read().unwrap();
        let lease = Arc::new(Lease {
            _users: version
                .mcp
                .values()
                .chain(std::iter::once(&version.lsp))
                .map(|slot| slot.users.clone())
                .collect(),
        });
        let services: Vec<_> = version
            .mcp
            .values()
            .filter_map(|slot| slot.ready.borrow().clone())
            .collect();
        let lsp = version.lsp.ready.borrow().clone();
        let ready = services.len() == version.mcp.len() && lsp.is_some();
        Configuration {
            revision: ready.then_some(version.revision),
            options: version.options.clone(),
            model: version.model.clone(),
            mcp_tools: Registry::snapshot_all(
                &services
                    .iter()
                    .filter_map(|service| match &**service {
                        Service::Mcp(registry) => Some(registry),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            lsp: lsp.and_then(|service| match &*service {
                Service::Lsp(lsp) => Some(lsp.clone()),
                _ => None,
            }),
            lease,
        }
    }

    fn options(&self) -> CoreOptions {
        self.current.read().unwrap().options.clone()
    }

    fn acquire(&self) -> futures_util::future::BoxFuture<'_, Result<Configuration, CoreError>> {
        Box::pin(async move {
            loop {
                // Pin every service while holding the publication lock.
                let (version, lease) = {
                    let version = self.current.read().unwrap();
                    let users = version
                        .mcp
                        .values()
                        .chain(std::iter::once(&version.lsp))
                        .map(|slot| slot.users.clone())
                        .collect();
                    (version.clone(), Arc::new(Lease { _users: users }))
                };
                let mut services = Vec::new();
                for slot in version.mcp.values() {
                    services.push(slot.get(&self.stop).await?);
                }
                let mcp_tools = Registry::snapshot_all(
                    &services
                        .iter()
                        .filter_map(|service| match &**service {
                            Service::Mcp(registry) => Some(registry),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                );
                let lsp = match &*version.lsp.get(&self.stop).await? {
                    Service::Lsp(lsp) => Some(lsp.clone()),
                    Service::Mcp(_) => unreachable!("LSP slot"),
                };
                if !Arc::ptr_eq(&version, &self.current.read().unwrap()) {
                    continue;
                }
                return Ok(Configuration {
                    revision: Some(version.revision),
                    options: version.options.clone(),
                    model: version.model.clone(),
                    mcp_tools,
                    lsp,
                    lease,
                });
            }
        })
    }
}
