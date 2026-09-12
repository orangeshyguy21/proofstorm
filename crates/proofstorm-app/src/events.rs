//! A single invalidation feed per server, shared by all SSE subscribers.
use crate::cell::Cells;
use kube::{Api, api::ListParams};
use proofstorm_core::Capability;
use proofstorm_kube::ProofstormCell;
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
    let api = Api::<ProofstormCell>::namespaced(
        cells.runtime.client.clone(),
        &cells.runtime.control_namespace,
    );
    let runtime =
        tokio::time::timeout(Duration::from_secs(3), api.list(&ListParams::default())).await;
    let versions = runtime.ok().and_then(Result::ok).map(|list| {
        list.items
            .into_iter()
            .filter(|cell| cell.spec.workspace_id == cells.workspace)
            .map(|cell| {
                (
                    cell.spec.instance_id,
                    (
                        cell.metadata.resource_version,
                        cell.metadata.generation,
                        cell.status,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    (
        journal,
        versions.and_then(|v| serde_json::to_string(&v).ok()),
    )
}
