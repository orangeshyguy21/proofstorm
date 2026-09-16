//! GUI-owned loopback tunnels. Browser sessions never own a tunnel's lifetime.
use crate::cell::Cells;
use futures::FutureExt;
use proofstorm_view::{
    LocalConnectionState as State, LocalConnectionView as View, OpenLocalConnection,
};
use std::fmt::Write;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, oneshot, watch},
    task::JoinHandle,
};

#[derive(Default)]
struct Registry {
    closing: bool,
    entries: BTreeMap<String, Entry>,
}
struct Entry {
    view: View,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub(crate) struct Connections {
    registry: Mutex<Registry>,
    updates: watch::Sender<Vec<View>>,
    pub streams: Arc<Semaphore>,
}
#[derive(Clone)]
struct Reporter {
    manager: Weak<Connections>,
    id: String,
}
impl Reporter {
    fn update(&self, state: State, url: Option<String>, message: Option<String>) {
        let Some(manager) = self.manager.upgrade() else {
            return;
        };
        let mut registry = manager.registry.lock().unwrap();
        if let Some(entry) = registry.entries.get_mut(&self.id) {
            // Setup completion cannot revive a cancelled connection.
            if entry.view.state == State::Disconnecting && state == State::Connected {
                return;
            }
            entry.view.state = state;
            entry.view.url = url;
            entry.view.message = message;
            manager.publish(&registry);
        }
    }
}
impl Connections {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(Registry::default()),
            updates: watch::channel(Vec::new()).0,
            // Leave HTTP capacity for controls alongside the eight environment streams.
            streams: Arc::new(Semaphore::new(4)),
        })
    }
    fn publish(&self, registry: &Registry) {
        self.updates
            .send_replace(registry.entries.values().map(|e| e.view.clone()).collect());
    }
    pub fn subscribe(&self) -> watch::Receiver<Vec<View>> {
        self.updates.subscribe()
    }
    pub fn snapshot(&self) -> Vec<View> {
        self.updates.borrow().clone()
    }

    pub fn open(
        self: &Arc<Self>,
        cells: Cells,
        request: OpenLocalConnection,
    ) -> anyhow::Result<View> {
        cells.store.authorize(
            &cells.workspace,
            &cells.principal,
            proofstorm_core::Capability::CellConnect,
        )?;
        let instance = cells.resolve_instance(&request.cell)?;
        anyhow::ensure!(
            request.incarnation == format!("{}:{}", instance.workspace_id, instance.instance_key),
            "This cell has changed. Refresh before connecting."
        );
        let handle = cells.resolve(&request.cell)?;
        anyhow::ensure!(
            handle.phase == proofstorm_store::CellHandlePhase::Open,
            "This cell is closed."
        );
        let (_, revision) = cells.store.operation_context(
            &cells.workspace,
            &cells.principal,
            &handle.instance_id,
            proofstorm_core::Capability::CellConnect,
        )?;
        // The GUI only exports supported mint HTTP endpoints, never arbitrary ports.
        crate::connections::endpoint(&revision, &request.component, "http")?;
        let cell = handle.instance_id;
        let view = View {
            id: String::new(),
            cell: cell.clone(),
            incarnation: request.incarnation,
            component: request.component.clone(),
            state: State::Connecting,
            url: None,
            message: None,
        };
        self.start(view, move |mut cancel, reporter| async move {
            let connection = tokio::select! {
                biased;
                _ = &mut cancel => return Ok(()),
                result = tokio::time::timeout(Duration::from_secs(15), cells.connect(&cell, &request.component, "http", 0)) => result.map_err(|_| "Connection setup timed out".to_owned())?.map_err(|e|e.to_string())?,
            };
            reporter.update(State::Connected, Some(connection.descriptor.url.clone()), None);
            connection.serve_until(async { let _ = cancel.await; }).await.map_err(|e|e.to_string())
        })
    }

    fn start<F, Fut>(self: &Arc<Self>, mut view: View, run: F) -> anyhow::Result<View>
    where
        F: FnOnce(oneshot::Receiver<()>, Reporter) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let mut registry = self.registry.lock().unwrap();
        anyhow::ensure!(!registry.closing, "GUI is stopping. Reopen it to connect.");
        if let Some(entry) = registry.entries.values().find(|e| {
            e.view.cell == view.cell
                && e.view.incarnation == view.incarnation
                && e.view.component == view.component
                && matches!(
                    e.view.state,
                    State::Connecting | State::Connected | State::Disconnecting
                )
        }) {
            return Ok(entry.view.clone());
        }
        // Bound both active listeners and retained terminal statuses.
        registry
            .entries
            .retain(|_, entry| !matches!(entry.view.state, State::Disconnected | State::Failed));
        anyhow::ensure!(
            registry.entries.len() < 32,
            "Too many local connections. Disconnect one first."
        );
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce)
            .map_err(|e| anyhow::anyhow!("Connection ID unavailable: {e}"))?;
        view.id = nonce
            .iter()
            .fold(String::with_capacity(32), |mut id, byte| {
                let _ = write!(id, "{byte:02x}");
                id
            });
        let (cancel, receiver) = oneshot::channel();
        let reporter = Reporter {
            manager: Arc::downgrade(self),
            id: view.id.clone(),
        };
        let task = tokio::spawn(async move {
            match std::panic::AssertUnwindSafe(run(receiver, reporter.clone()))
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => {
                    reporter.update(State::Disconnected, None, Some("Connection closed".into()));
                }
                Ok(Err(message)) => reporter.update(State::Failed, None, Some(message)),
                Err(_) => reporter.update(
                    State::Failed,
                    None,
                    Some("Connection stopped unexpectedly".into()),
                ),
            }
        });
        registry.entries.insert(
            view.id.clone(),
            Entry {
                view: view.clone(),
                cancel: Some(cancel),
                task: Some(task),
            },
        );
        self.publish(&registry);
        Ok(view)
    }

    pub async fn close(&self, id: &str) {
        let mut updates = self.subscribe();
        {
            let mut registry = self.registry.lock().unwrap();
            let Some(entry) = registry.entries.get_mut(id) else {
                return;
            };
            if matches!(entry.view.state, State::Connecting | State::Connected) {
                entry.view.state = State::Disconnecting;
                entry.view.url = None;
                if let Some(cancel) = entry.cancel.take() {
                    let _ = cancel.send(());
                }
                self.publish(&registry);
            }
        }
        loop {
            let finished = updates
                .borrow_and_update()
                .iter()
                .find(|v| v.id == id)
                .is_none_or(|v| matches!(v.state, State::Disconnected | State::Failed));
            if finished || updates.changed().await.is_err() {
                return;
            }
        }
    }

    pub async fn shutdown(&self) {
        let tasks = {
            let mut registry = self.registry.lock().unwrap();
            registry.closing = true;
            let mut tasks = Vec::new();
            for entry in registry.entries.values_mut() {
                if let Some(cancel) = entry.cancel.take() {
                    let _ = cancel.send(());
                }
                if let Some(task) = entry.task.take() {
                    tasks.push(task);
                }
            }
            tasks
        };
        for task in tasks {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };

    fn view(component: &str) -> View {
        View {
            id: String::new(),
            cell: "cell".into(),
            incarnation: "workspace:instance".into(),
            component: component.into(),
            state: State::Connecting,
            url: None,
            message: None,
        }
    }
    async fn state(manager: &Connections, id: &str, expected: State) -> View {
        let mut updates = manager.subscribe();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(view) = updates
                    .borrow_and_update()
                    .iter()
                    .find(|v| v.id == id && v.state == expected)
                    .cloned()
                {
                    return view;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("connection state transition")
    }
    async fn listening(
        manager: &Arc<Connections>,
        component: &str,
    ) -> (View, std::net::SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let view = manager
            .start(view(component), move |mut cancel, reporter| async move {
                reporter.update(State::Connected, Some(format!("http://{address}")), None);
                let mut sockets = Vec::new();
                loop {
                    tokio::select! {
                        _ = &mut cancel => break,
                        socket = listener.accept() => { sockets.push(socket.unwrap().0); },
                    }
                }
                drop(sockets);
                drop(listener);
                Ok(())
            })
            .unwrap();
        state(manager, &view.id, State::Connected).await;
        (view, address)
    }

    #[tokio::test]
    async fn duplicate_open_and_cancellation_during_setup_create_no_listener() {
        let manager = Connections::new();
        let first = manager
            .start(view("mint"), |cancel, _| async {
                let _ = cancel.await;
                Ok(())
            })
            .unwrap();
        let duplicate = manager
            .start(view("mint"), |_, _| async {
                panic!("duplicate worker started")
            })
            .unwrap();
        assert_eq!(first.id, duplicate.id);
        let ((), ()) = tokio::join!(manager.close(&first.id), manager.close(&first.id));
        assert_eq!(
            state(&manager, &first.id, State::Disconnected).await.url,
            None
        );
    }

    #[tokio::test]
    async fn disconnect_releases_port_and_accepted_socket_before_returning() {
        let manager = Connections::new();
        let (view, address) = listening(&manager, "mint").await;
        let mut socket = TcpStream::connect(address).await.unwrap();
        tokio::task::yield_now().await;
        manager.close(&view.id).await;
        let rebound = TcpListener::bind(address)
            .await
            .expect("listener must be released");
        let mut buf = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf))
            .await
            .unwrap();
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "accepted socket remained open"
        );
        drop(rebound);
    }

    #[tokio::test]
    async fn shutdown_drains_all_connections_and_refuses_new_work() {
        let manager = Connections::new();
        let (a, address_a) = listening(&manager, "a").await;
        let (_, address_b) = listening(&manager, "b").await;
        let ((), ()) = tokio::join!(manager.close(&a.id), manager.shutdown());
        let _a = TcpListener::bind(address_a).await.unwrap();
        let _b = TcpListener::bind(address_b).await.unwrap();
        assert!(manager.start(view("c"), |_, _| async { Ok(()) }).is_err());
    }

    #[tokio::test]
    async fn dropping_manager_aborts_workers_but_dropping_browser_subscription_does_not() {
        let manager = Connections::new();
        let (_, address) = listening(&manager, "mint").await;
        let subscriber = manager.subscribe();
        drop(subscriber);
        assert!(TcpListener::bind(address).await.is_err());
        drop(manager);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if TcpListener::bind(address).await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn old_disconnect_cannot_close_replacement_and_reconnect_receives_snapshot() {
        let manager = Connections::new();
        let (old, _) = listening(&manager, "mint").await;
        manager.close(&old.id).await;
        let (new, address) = listening(&manager, "mint").await;
        assert_ne!(old.id, new.id);
        manager.close(&old.id).await;
        assert!(TcpListener::bind(address).await.is_err());
        assert_eq!(manager.subscribe().borrow()[0].id, new.id);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn failed_setup_reports_error_and_can_be_retried() {
        let manager = Connections::new();
        let first = manager
            .start(view("mint"), |_, _| async {
                Err("no ready workload".into())
            })
            .unwrap();
        assert_eq!(
            state(&manager, &first.id, State::Failed)
                .await
                .message
                .as_deref(),
            Some("no ready workload")
        );
        let (next, _) = listening(&manager, "mint").await;
        assert_ne!(first.id, next.id);
        manager.shutdown().await;
    }
}
