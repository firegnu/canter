//! One independent lease per terminal run, shared by every session switch.
use crate::{
    Connected, Connector,
    client::{Client, ClientError},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

pub(crate) struct Managed<C> {
    connector: C,
    id: String,
    lease: Mutex<Option<Lease>>,
    disconnected: Arc<Notify>,
    stopping: AtomicBool,
}

struct Lease {
    client: Client,
    server: Option<String>,
    reader: Task,
}

pub(crate) struct Task(pub(crate) tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<C: Connector> Managed<C> {
    pub(crate) fn new(connector: C, id: String) -> Arc<Self> {
        Arc::new(Self {
            connector,
            id,
            lease: Mutex::new(None),
            disconnected: Arc::new(Notify::new()),
            stopping: AtomicBool::new(false),
        })
    }

    pub(crate) async fn attach(&self, mut connected: Connected) -> Result<Connected, ClientError> {
        let mut lease = self.lease.lock().await;
        if self.stopping.load(Ordering::Relaxed) {
            return Err(ClientError::Protocol("Managed terminal is exiting".into()));
        }
        connected.client.managed_id = Some(self.id.clone());
        if lease.as_ref().is_some_and(|lease| {
            connected.server.is_some()
                && lease.server == connected.server
                && !lease.reader.0.is_finished()
        }) {
            return Ok(connected);
        }
        if !connected
            .client
            .info()
            .await?
            .features
            .unwrap_or_default()
            .iter()
            .any(|feature| feature == "managedClients")
        {
            return Err(ClientError::Protocol("This server does not support --close-on-exit; run canter serve --replace and retry".into()));
        }
        let mut response = connected.client.managed_connect(&self.id).await?;
        let disconnected = self.disconnected.clone();
        let reader = Task(tokio::spawn(async move {
            while matches!(response.chunk().await, Ok(Some(_))) {}
            disconnected.notify_one();
        }));
        *lease = Some(Lease {
            client: connected.client.clone(),
            server: connected.server.clone(),
            reader,
        });
        Ok(connected)
    }

    pub(crate) fn watch(self: &Arc<Self>) -> Task {
        let managed = self.clone();
        Task(tokio::spawn(async move {
            loop {
                managed.disconnected.notified().await;
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if managed.stopping.load(Ordering::Relaxed) {
                        return;
                    }
                    if managed.connect().await.is_ok() {
                        break;
                    }
                }
            }
        }))
    }

    pub(crate) async fn close(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        // Covers the HTTP request and a concurrent reconnect holding this lock.
        let _ = tokio::time::timeout(Duration::from_secs(4), async {
            if let Some(lease) = self.lease.lock().await.take() {
                let _ = lease.client.managed_close(&self.id).await;
            }
        })
        .await;
    }
}

impl<C: Connector> Connector for Arc<Managed<C>> {
    async fn connect(&self) -> Result<Connected, String> {
        let connected = self.connector.connect().await?;
        self.attach(connected)
            .await
            .map_err(|error| error.to_string())
    }
}
