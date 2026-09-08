//! A single invalidation feed per server, shared by all SSE subscribers.
use crate::lab::Labs;
use kube::{Api, api::ListParams};
use proofstorm_core::Capability;
use proofstorm_kube::ProofstormLab;
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
    pub fn start(labs: Labs, observer: Arc<RwLock<ObserverStatus>>) -> Self {
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
                let token = (signature(&labs).await, status);
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
async fn signature(labs: &Labs) -> (Option<(i64, u64)>, Option<String>) {
    let journal = labs
        .store
        .observation_token(&labs.workspace, &labs.principal)
        .ok();
    if labs
        .store
        .authorize(&labs.workspace, &labs.principal, Capability::LabStatus)
        .is_err()
    {
        return (journal, None);
    }
    let api = Api::<ProofstormLab>::namespaced(
        labs.runtime.client.clone(),
        &labs.runtime.control_namespace,
    );
    let runtime =
        tokio::time::timeout(Duration::from_secs(3), api.list(&ListParams::default())).await;
    let versions = runtime.ok().and_then(Result::ok).map(|list| {
        list.items
            .into_iter()
            .filter(|lab| lab.spec.workspace_id == labs.workspace)
            .map(|lab| {
                (
                    lab.spec.instance_id,
                    (
                        lab.metadata.resource_version,
                        lab.metadata.generation,
                        lab.status,
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
