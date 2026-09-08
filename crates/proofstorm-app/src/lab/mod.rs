//! Shared lab application service. CLI and MCP adapt inputs and responses here;
//! runtime resources and durable storage remain behind their respective modules.
use crate::{Error, Runtime};
use proofstorm_core::{Capability, Experiment, LabInstance, LabInstanceStatus};
use proofstorm_store::{LabHandle, Store, StoreError};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone)]
pub struct Labs {
    pub store: Store,
    pub runtime: Runtime,
    pub workspace: String,
    pub principal: String,
}

mod apply;
mod close;
mod components;
pub use components::ComponentControlRequest;
mod create;
mod edit;
mod execute;
mod identity;
mod observe;
mod wait;
pub use wait::{WaitRequest, WaitResult, wait_terminal};

pub use apply::{AppliedLab, ReconciliationError, ReviewedApply, review_apply};
pub use proofstorm_view::Activity;

#[derive(Debug, Serialize, JsonSchema)]
pub struct LabView {
    pub lab: LabHandle,
    pub instance_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation_error: Option<ReconciliationError>,
    pub runtime: Option<LabInstanceStatus>,
    pub run: Option<Experiment>,
    pub sessions: proofstorm_store::SessionPage,
    pub activity: Vec<Activity>,
    pub next_sequence: Option<u64>,
    pub observed_at_unix: i64,
}

impl Labs {
    #[must_use]
    pub fn new(store: Store, runtime: Runtime, workspace: String, principal: String) -> Self {
        Self {
            store,
            runtime,
            workspace,
            principal,
        }
    }

    fn authorize(&self, capabilities: &[Capability]) -> Result<(), Error> {
        for capability in capabilities {
            self.store
                .authorize(&self.workspace, &self.principal, *capability)?;
        }
        Ok(())
    }

    fn instance(&self, lab: &LabHandle) -> Result<LabInstance, Error> {
        Ok(self
            .store
            .instance(&self.workspace, &self.principal, &lab.instance_id)?)
    }
}

fn optional<T>(result: Result<T, StoreError>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(StoreError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
