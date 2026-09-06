//! Background receipt collection; independent from the passive HTTP GET handlers.
use crate::{journal, lab::Labs};
use futures::{StreamExt, stream};
use proofstorm_core::Capability;
use proofstorm_view::ObserverStatus;
use std::{
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;

pub struct Observer {
    pub status: Arc<RwLock<ObserverStatus>>,
    task: JoinHandle<()>,
}
impl Observer {
    #[must_use]
    pub fn start(labs: Labs) -> Self {
        let status = Arc::new(RwLock::new(ObserverStatus {
            state: "starting".into(),
            ..Default::default()
        }));
        let shared = status.clone();
        let task = tokio::spawn(async move {
            let mut cursor = String::new();
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                let result = collect(&labs, &cursor).await;
                let Ok(mut status) = shared.write() else {
                    break;
                };
                status.last_attempt_at_unix = Some(now);
                if let Ok((next, recorded, failed)) = result {
                    cursor = next;
                    status.recorded_operations += recorded;
                    if failed {
                        status.state = "degraded".into();
                        status.error = Some(
                            "Some runtime receipts could not be collected; retrying automatically."
                                .into(),
                        );
                    } else {
                        status.state = "watching".into();
                        status.error = None;
                        status.last_success_at_unix = Some(now);
                    }
                } else {
                    status.state = "unavailable".into();
                    status.error=Some("Receipt collection requires lab.status, experiment.read and artifact.read in this workspace, and a readable journal.".into());
                }
            }
        });
        Self { status, task }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn collect(labs: &Labs, cursor: &str) -> Result<(String, u64, bool), ()> {
    for cap in [
        Capability::LabStatus,
        Capability::ExperimentRead,
        Capability::ArtifactRead,
    ] {
        labs.store
            .authorize(&labs.workspace, &labs.principal, cap)
            .map_err(|_| ())?;
    }
    let operations = labs
        .store
        .pending_observations(&labs.workspace, &labs.principal, cursor, 50)
        .map_err(|_| ())?;
    let next = if operations.len() == 50 {
        operations.last().map(|o| o.id.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    let results = stream::iter(operations)
        .map(|op| async move {
            let observed =
                tokio::time::timeout(Duration::from_secs(3), labs.runtime.action_status(&op))
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
            if let Some((phase, artifact)) = observed {
                for cap in [
                    Capability::LabStatus,
                    Capability::ExperimentRead,
                    Capability::ArtifactRead,
                ] {
                    labs.store
                        .authorize(&labs.workspace, &labs.principal, cap)
                        .map_err(|_| ())?;
                }
                journal::record(&labs.store, &labs.workspace, &op, phase, artifact)
                    .map_err(|_| ())?;
                Ok::<bool, ()>(true)
            } else {
                Ok(false)
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    Ok((
        next,
        u64::try_from(results.iter().filter(|r| matches!(r, Ok(true))).count()).unwrap_or(0),
        results.iter().any(Result::is_err),
    ))
}
