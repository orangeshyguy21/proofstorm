//! A single invalidation feed per server, shared by all SSE subscribers.
use crate::cell::Cells;
use kube::{Api, api::ListParams};
use proofstorm_core::Capability;
use proofstorm_kube::{INSTANCE_LABEL, ProofstormCell, ProofstormComponent};
use proofstorm_view::ObserverStatus;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

pub struct Events {
    pub receiver: watch::Receiver<u64>,
    task: JoinHandle<()>,
}
impl Events {
    pub fn start(cells: Cells, observer: Arc<RwLock<ObserverStatus>>) -> Self {
        let (sender, receiver) = watch::channel(0_u64);
        let task = tokio::spawn(async move {
            let mut previous = None;
            let mut timer = tokio::time::interval(Duration::from_secs(2));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let status = observer
                    .read()
                    .ok()
                    .map(|s| (s.state.clone(), s.error.clone()));
                let token = (signature(&cells).await, status);
                if previous.as_ref() != Some(&token) {
                    previous = Some(token);
                    sender.send_modify(|version| *version = version.wrapping_add(1));
                }
            }
        });
        Self { receiver, task }
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn signature(cells: &Cells) -> (Option<(i64, u64)>, Option<String>) {
    let journal = cells
        .store
        .observation_token(&cells.workspace, &cells.principal)
        .ok();
    if cells
        .store
        .authorize(&cells.workspace, &cells.principal, Capability::CellStatus)
        .is_err()
    {
        return (journal, None);
    }
    // Metadata from the API server's cache: a resourceVersion moves whenever
    // its object changes, so versions alone say when to refresh.
    let api = Api::<ProofstormCell>::namespaced(
        cells.runtime.client.clone(),
        &cells.runtime.control_namespace,
    );
    let runtime = tokio::time::timeout(
        Duration::from_secs(3),
        api.list_metadata(&ListParams::default().match_any()),
    )
    .await;
    let workspace_cells = runtime.ok().and_then(Result::ok).map(|list| {
        list.items
            .into_iter()
            .filter_map(|cell| {
                let identity = proofstorm_kube::cell_identity(&cell.metadata)?;
                (identity.workspace_id == cells.workspace).then_some((identity, cell.metadata))
            })
            .collect::<Vec<_>>()
    });
    let keys = workspace_cells.as_ref().map(|list| {
        list.iter()
            .map(|(identity, _)| identity.instance_key.clone())
            .collect::<std::collections::BTreeSet<_>>()
    });
    let versions = workspace_cells.map(|list| {
        list.into_iter()
            .map(|(identity, metadata)| {
                (
                    identity.instance_id,
                    (metadata.resource_version, metadata.generation),
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    // Component statuses live in their own objects.
    let components = Api::<ProofstormComponent>::namespaced(
        cells.runtime.client.clone(),
        &cells.runtime.control_namespace,
    );
    let components = tokio::time::timeout(
        Duration::from_secs(3),
        components.list_metadata(&ListParams::default().match_any()),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .map(|list| {
        list.items
            .into_iter()
            .filter(|object| {
                object
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(INSTANCE_LABEL))
                    .is_some_and(|key| keys.as_ref().is_some_and(|keys| keys.contains(key)))
            })
            .map(|object| (object.metadata.name, object.metadata.resource_version))
            .collect::<BTreeMap<_, _>>()
    });
    (
        journal,
        versions
            .zip(components)
            .and_then(|v| serde_json::to_string(&v).ok()),
    )
}
