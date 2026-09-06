//! Kubernetes lifecycle and observation, independent of any wire transport.
use crate::Error;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams},
};
use proofstorm_core::{
    InstancePhase, LabInstance, LabInstanceStatus, LabOperation, OperationPhase,
    PrivateAccessGrant, PublishedRevision, TeardownReceipt as CoreTeardownReceipt,
};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, LabAction, LabPhase, ProofstormLab, ProofstormLabAction,
    ProofstormLabActionSpec, ProofstormLabSpec,
};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Runtime {
    pub client: Client,
    pub control_namespace: String,
}
impl Runtime {
    #[must_use]
    pub fn new(client: Client, control_namespace: String) -> Self {
        Self {
            client,
            control_namespace,
        }
    }
    /// Publish private-transfer permissions independently of activity sessions.
    pub async fn private_access(&self, grant: &PrivateAccessGrant) -> Result<(), Error> {
        for attempt in 0..5 {
            match self.private_access_once(grant).await {
                Err(error)
                    if error
                        .details
                        .as_ref()
                        .is_some_and(|d| d["http_status"] == 409)
                        && attempt < 4 =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt + 1))).await;
                }
                result => return result,
            }
        }
        unreachable!()
    }
    async fn private_access_once(&self, grant: &PrivateAccessGrant) -> Result<(), Error> {
        use proofstorm_core::private_io::PRIVATE_ACCESS_ANNOTATION;
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let matches = labs
            .list(&kube::api::ListParams::default())
            .await
            .map_err(kube_error)?
            .items
            .into_iter()
            .filter(|lab| {
                lab.spec.workspace_id == grant.workspace_id
                    && lab.spec.instance_id == grant.instance_id
            })
            .collect::<Vec<_>>();
        let [lab] = matches.as_slice() else {
            return Err(invalid_operation("private access lab unavailable"));
        };
        proofstorm_kube::require_open_lab(lab)
            .map_err(|e| Error::problem(e.code(), e.to_string()))?;
        let mut grants: BTreeMap<String, PrivateAccessGrant> = lab
            .annotations()
            .get(PRIVATE_ACCESS_ANNOTATION)
            .map(|s| serde_json::from_str(s))
            .transpose()
            .map_err(|_| invalid_operation("private access registry invalid"))?
            .unwrap_or_default();
        if let Some(previous) = grants.get(&grant.id) {
            if previous.scope != grant.scope
                || previous.principal_id != grant.principal_id
                || previous.instance_id != grant.instance_id
            {
                return Err(invalid_operation("private access identity conflict"));
            }
            if previous.revoked_at_unix.is_some() && grant.revoked_at_unix.is_none() {
                return Err(invalid_operation("private access was revoked"));
            }
            if previous == grant {
                return Ok(());
            }
        }
        grants.insert(grant.id.clone(), grant.clone());
        let encoded = serde_json::to_string(&grants)
            .map_err(|_| invalid_operation("private access serialization failed"))?;
        if encoded.len() > 128 * 1024 {
            return Err(invalid_operation(
                "private access registry exceeds runtime metadata capacity",
            ));
        }
        labs.patch(&lab.name_any(),&PatchParams::default(),&Patch::Merge(serde_json::json!({"metadata":{"resourceVersion":lab.resource_version(),"annotations":{PRIVATE_ACCESS_ANNOTATION:encoded}}}))).await.map_err(kube_error)?;
        Ok(())
    }
    pub async fn apply_action(
        &self,
        instance: &LabInstance,
        action: &ProofstormLabAction,
    ) -> Result<(), Error> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let lab = labs
            .get(&instance.resource_name)
            .await
            .map_err(kube_error)?;
        proofstorm_kube::require_open_lab(&lab)
            .map_err(|error| coded_invalid_request(error.code(), error.to_string()))?;
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let name = action.metadata.name.as_deref().ok_or_else(|| {
            Error::failure(
                "typed action has no resource name",
                Some(serde_json::json!({"code": "render_failure"})),
            )
        })?;
        if let Some(existing) = actions.get_opt(name).await.map_err(kube_error)? {
            if existing.spec != action.spec {
                return Err(coded_invalid_request(
                    "action_identity_conflict",
                    format!("action resource {name:?} already exists with a different request"),
                ));
            }
            return Ok(());
        }
        actions
            .patch(
                name,
                &PatchParams::apply("proofstorm-mcp"),
                &Patch::Apply(action),
            )
            .await
            .map_err(kube_error)?;
        Ok(())
    }
    pub async fn action_status(
        &self,
        operation: &LabOperation,
    ) -> Result<Option<(OperationPhase, serde_json::Value)>, Error> {
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let Some(action) = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
        else {
            // The runtime resource is gone (lab closed, or garbage collected)
            // before the journal saw a terminal phase. A running operation
            // whose resource vanished is a terminal outcome, never a live
            // one; a pending operation may simply not be applied yet.
            return Ok((operation.phase == OperationPhase::Running)
                .then(|| (OperationPhase::Failed, missing_action_artifact(operation))));
        };
        let Some(status) = action.status else {
            return Ok(None);
        };
        Ok(terminal_action_observation(
            status,
            matches!(action.spec.action, LabAction::ComponentExecLive(_)),
        ))
    }
    pub async fn request_action_cancellation(
        &self,
        operation: &LabOperation,
        token: &str,
    ) -> Result<bool, Error> {
        let actions =
            Api::<ProofstormLabAction>::namespaced(self.client.clone(), &self.control_namespace);
        let Some(action) = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
        else {
            return Ok(false);
        };
        if action.spec.workspace_id != operation.workspace_id
            || action.spec.instance_id != operation.instance_id
            || action.spec.experiment_id != operation.experiment_id
            || action.spec.operation_id != operation.id
            || action.spec.principal_id != operation.principal_id
            || action.spec.request_digest != operation.request_digest
        {
            return Err(coded_invalid_request(
                "action_identity_conflict",
                "action cancellation identity does not match the journal",
            ));
        }
        if action.status.as_ref().is_some_and(|status| {
            matches!(
                status.phase,
                ActionPhase::Succeeded | ActionPhase::Failed | ActionPhase::Cancelled
            )
        }) || action.annotations().contains_key(ACTION_CANCEL_ANNOTATION)
        {
            return Ok(true);
        }
        actions
            .patch(
                &operation.resource_name,
                &PatchParams::default(),
                &Patch::Merge(serde_json::json!({
                    "metadata": {"annotations": {(ACTION_CANCEL_ANNOTATION): token}}
                })),
            )
            .await
            .map_err(kube_error)?;
        Ok(true)
    }
    pub async fn materialize(
        &self,
        instance: LabInstance,
        revision: PublishedRevision,
    ) -> Result<LabInstanceStatus, Error> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let mut resource = ProofstormLab::new(
            &instance.resource_name,
            ProofstormLabSpec {
                workspace_id: instance.workspace_id.clone(),
                instance_id: instance.id.clone(),
                instance_key: instance.instance_key.clone(),
                revision_digest: instance.revision_digest.clone(),
                lock: revision.lock,
                lab: revision.lab,
            },
        );
        resource.metadata.namespace = Some(self.control_namespace.clone());
        let applied = labs
            .patch(
                &instance.resource_name,
                &PatchParams::apply("proofstorm-mcp").force(),
                &Patch::Apply(&resource),
            )
            .await
            .map_err(kube_error)?;
        Ok(status_from_resource(instance, &applied))
    }
    pub async fn status(&self, instance: LabInstance) -> Result<LabInstanceStatus, Error> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        if let Some(resource) = labs
            .get_opt(&instance.resource_name)
            .await
            .map_err(kube_error)?
        {
            return Ok(status_from_resource(instance, &resource));
        }
        let receipts = Api::<ConfigMap>::namespaced(self.client.clone(), &self.control_namespace);
        let name = format!("proofstorm-teardown-{}", instance.instance_key);
        let receipt = receipts.get_opt(&name).await.map_err(kube_error)?;
        let Some(receipt) = receipt else {
            return Err(Error::missing(
                format!(
                    "lab instance {:?} has no runtime resource or teardown receipt",
                    instance.id
                ),
                Some(serde_json::json!({"code": "runtime_not_found"})),
            ));
        };
        let data = receipt.data.unwrap_or_default();
        Ok(LabInstanceStatus {
            instance: instance.clone(),
            phase: InstancePhase::Closed,
            instance_namespace: data.get("instanceNamespace").cloned().unwrap_or_default(),
            components: vec![],
            inventory: vec![],
            teardown_receipt: Some(CoreTeardownReceipt {
                instance_id: instance.id,
                instance_namespace: data.get("instanceNamespace").cloned().unwrap_or_default(),
                inventory_digest: data.get("inventoryDigest").cloned().unwrap_or_default(),
                verified_absent: data
                    .get("verifiedAbsent")
                    .is_some_and(|value| value == "true"),
            }),
            message: None,
        })
    }
    pub async fn close(&self, instance: LabInstance) -> Result<LabInstanceStatus, Error> {
        let mut status = match self.status(instance.clone()).await {
            Ok(status) => status,
            Err(error) if error.kind == crate::ErrorKind::Missing => {
                return self.verify_absent(instance).await;
            }
            Err(error) => return Err(error),
        };
        if status.phase == InstancePhase::Closed {
            return Ok(status);
        }
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        labs.delete(&instance.resource_name, &DeleteParams::default())
            .await
            .map_err(kube_error)?;
        status.phase = InstancePhase::Closing;
        status.message = Some("deleting instance namespace and verifying absence".into());
        Ok(status)
    }

    /// Verify both runtime identity and namespace absence when startup never
    /// reached the controller, so there is no controller teardown receipt.
    pub async fn verify_absent(&self, instance: LabInstance) -> Result<LabInstanceStatus, Error> {
        let labs = Api::<ProofstormLab>::namespaced(self.client.clone(), &self.control_namespace);
        let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
        if labs.get_opt(&instance.resource_name).await?.is_some()
            || Api::<Namespace>::all(self.client.clone())
                .get_opt(&namespace)
                .await?
                .is_some()
        {
            return Err(Error::problem(
                "cleanup_unverified",
                "runtime resource or instance namespace still exists",
            ));
        }
        Ok(LabInstanceStatus {
            instance: instance.clone(),
            phase: InstancePhase::Closed,
            instance_namespace: namespace.clone(),
            components: vec![],
            inventory: vec![],
            teardown_receipt: Some(CoreTeardownReceipt {
                instance_id: instance.id,
                instance_namespace: namespace,
                inventory_digest: proofstorm_core::digest_json(&Vec::<serde_json::Value>::new()),
                verified_absent: true,
            }),
            message: Some(
                "absence verified directly; no controller teardown receipt was available".into(),
            ),
        })
    }
}
#[must_use]
pub fn status_from_resource(instance: LabInstance, resource: &ProofstormLab) -> LabInstanceStatus {
    let status = resource.status.clone().unwrap_or_default();
    LabInstanceStatus {
        instance,
        phase: match status.phase {
            LabPhase::Pending => InstancePhase::Pending,
            LabPhase::Ready => InstancePhase::Ready,
            LabPhase::Closing => InstancePhase::Closing,
            LabPhase::CleanupBlocked => InstancePhase::CleanupBlocked,
        },
        instance_namespace: status.instance_namespace.unwrap_or_default(),
        components: status.components,
        inventory: status.inventory,
        teardown_receipt: status.teardown_receipt.map(|receipt| CoreTeardownReceipt {
            instance_id: receipt.instance_id,
            instance_namespace: receipt.instance_namespace,
            inventory_digest: receipt.inventory_digest,
            verified_absent: receipt.verified_absent,
        }),
        message: status.message,
    }
}
#[must_use]
pub fn terminal_action_observation(
    status: proofstorm_kube::ProofstormLabActionStatus,
    native: bool,
) -> Option<(OperationPhase, serde_json::Value)> {
    let (phase, fallback) = match status.phase {
        ActionPhase::Pending | ActionPhase::Running => return None,
        ActionPhase::Succeeded => (OperationPhase::Succeeded, "terminal_artifact_missing"),
        ActionPhase::Failed => (OperationPhase::Failed, "action_failed"),
        ActionPhase::Cancelled => (OperationPhase::Cancelled, "action_cancelled"),
    };
    // A cancelled or failed native execution can still have a supervisor receipt
    // proving cleanup (or explicitly declining that proof). Preserve that evidence.
    let content = match status.phase {
        ActionPhase::Succeeded => status.artifact,
        ActionPhase::Failed | ActionPhase::Cancelled if native => status.artifact.or(status.error),
        ActionPhase::Failed => status.error,
        _ => None,
    };
    let artifact = content.map_or_else(
        || serde_json::json!({"code": fallback}),
        |artifact| {
            serde_json::to_value(artifact).unwrap_or_else(
                |_| serde_json::json!({"code":"terminal_artifact_serialization_failed"}),
            )
        },
    );
    Some((
        phase,
        if native {
            proofstorm_core::native::cap_public_streams(artifact)
        } else {
            artifact
        },
    ))
}
#[must_use]
pub fn missing_action_artifact(operation: &LabOperation) -> serde_json::Value {
    serde_json::json!({
        "code": "action_runtime_not_found",
        "resource_name": operation.resource_name,
        "message": "the runtime action resource no longer exists; its outcome was not observed",
    })
}
fn coded_invalid_request(code: &str, message: impl Into<String>) -> Error {
    Error::problem(code, message)
}
fn invalid_operation(message: &str) -> Error {
    Error::problem("invalid_operation", message)
}
fn kube_error(error: kube::Error) -> Error {
    error.into()
}
#[must_use]
pub fn runtime_action_resource(
    control_namespace: &str,
    instance: &LabInstance,
    operation: &LabOperation,
    action: LabAction,
) -> ProofstormLabAction {
    let mut resource = ProofstormLabAction::new(
        &operation.resource_name,
        ProofstormLabActionSpec {
            access_scope: None,
            lab_name: instance.resource_name.clone(),
            workspace_id: operation.workspace_id.clone(),
            instance_id: operation.instance_id.clone(),
            instance_key: instance.instance_key.clone(),
            experiment_id: operation.experiment_id.clone(),
            session_id: operation.session_id.clone(),
            principal_id: operation.principal_id.clone(),
            sequence: operation.sequence,
            operation_id: operation.id.clone(),
            request_digest: operation.request_digest.clone(),
            capability: operation.capability,
            accepted_at_unix: operation.accepted_at_unix,
            action,
        },
    );
    resource.metadata.namespace = Some(control_namespace.to_owned());
    resource.metadata.labels = Some(std::collections::BTreeMap::from([
        (
            "proofstorm.dev/instance".to_owned(),
            instance.instance_key.clone(),
        ),
        (
            "proofstorm.dev/lab".to_owned(),
            instance.resource_name.clone(),
        ),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstorm-mcp".to_owned(),
        ),
    ]));
    resource
}
