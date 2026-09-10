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
    installation: Option<crate::installation::Installation>,
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
            installation: None,
        }
    }

    /// Installed clients opt into private, on-demand image preparation.
    #[must_use]
    pub fn with_installation(
        mut self,
        installation: Option<crate::installation::Installation>,
    ) -> Self {
        self.installation = installation;
        self
    }

    async fn prepare_images(
        &self,
        revision: &proofstorm_core::PublishedRevision,
    ) -> Result<(), Error> {
        if let Some(installation) = &self.installation {
            self.authorize(&[Capability::LabMaterialize])?;
            if self.runtime.cluster_source != installation.context()
                || self.runtime.control_namespace != crate::config::DEFAULT_NAMESPACE
            {
                return Err(Error::problem(
                    "installation_runtime_mismatch",
                    "Image preparation requires this installation's private runtime",
                ));
            }
            crate::bootstrap::prepare_images(installation.clone(), revision.lock.clone())
                .await
                .map_err(|error| {
                    Error::problem(
                        "image_preparation_failed",
                        format!(
                            "{error:#}. Retry the same lab request; no resources were deleted."
                        ),
                    )
                })?;
        }
        Ok(())
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

#[cfg(test)]
mod image_tests {
    use super::*;

    #[tokio::test]
    async fn denied_materialization_and_foreign_runtime_never_prepare_images() {
        let home = tempfile::tempdir().unwrap();
        let installation = crate::installation::Installation {
            format_version: 1,
            id: "e".repeat(32),
            home: home.path().join("absent"),
            api_port: 42101,
            registry_port: 42102,
        };
        let store = Store::memory().unwrap();
        crate::developer::configure(&store, "local", "agent").unwrap();
        let runtime = Runtime {
            client: kube::Client::new(
                tower::service_fn(|_: http::Request<kube::client::Body>| async {
                    Err::<http::Response<kube::client::Body>, _>(std::io::Error::other(
                        "unexpected runtime request",
                    ))
                }),
                "default",
            ),
            control_namespace: crate::config::DEFAULT_NAMESPACE.into(),
            cluster_source: "foreign".into(),
        };
        let mut labs = Labs::new(store.clone(), runtime, "local".into(), "agent".into())
            .with_installation(Some(installation.clone()));
        let lab =
            serde_json::from_str(include_str!("../../../../examples/developer-lab.json")).unwrap();
        let lock = proofstorm_core::resolve_lock(&lab, proofstorm_core::default_catalog()).unwrap();
        let revision = proofstorm_core::PublishedRevision {
            workspace_id: "local".into(),
            digest: "fixture".into(),
            lab,
            lock,
        };
        assert_eq!(
            labs.prepare_images(&revision)
                .await
                .unwrap_err()
                .details
                .unwrap()["code"],
            "installation_runtime_mismatch"
        );
        store
            .revoke("local", "agent", Capability::LabMaterialize)
            .unwrap();
        labs.runtime.cluster_source = installation.context();
        assert!(labs.prepare_images(&revision).await.is_err());
        assert!(!installation.home.exists());
    }
}
