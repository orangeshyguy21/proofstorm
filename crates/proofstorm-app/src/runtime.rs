//! Kubernetes lifecycle and observation, independent of any wire transport.
use crate::Error;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, Patch, PatchParams},
};
use proofstorm_core::{
    CellInstance, CellInstanceStatus, CellOperation, InstancePhase, OperationPhase,
    PrivateAccessGrant, PublishedRevision, TeardownReceipt as CoreTeardownReceipt,
};
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionPhase, CellAction, CellPhase,
    FINISHED_ACTION_RETENTION_SECONDS, INSTANCE_LABEL, LoadedCell, ProofstormCell,
    ProofstormCellAction, ProofstormCellActionSpec, ProofstormCellSpec, ProofstormComponent,
};
use std::collections::BTreeMap;

/// Lists that grow with a cell's component count get this long. Metadata and
/// single-object reads keep a 3 s bound.
pub(crate) const GROWING_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn inventory_timeout(message: &str) -> Error {
    Error::failure(message, Some(serde_json::json!({"code":"runtime_failure"})))
}

#[derive(Clone)]
pub struct Runtime {
    pub client: Client,
    pub control_namespace: String,
    pub cluster_source: String,
    /// Immutable cell revisions, read once each per process.
    pub revisions: std::sync::Arc<proofstorm_kube::RevisionCache>,
}
impl Runtime {
    /// Current cluster inventory; a failed list must never look like an empty cluster.
    /// A metadata-only list from the API server's cache: ids are all it needs.
    pub(crate) async fn current_instance_ids(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeSet<String>, Error> {
        let api = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let cells = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            api.list_metadata(&kube::api::ListParams::default().match_any()),
        )
        .await
        .map_err(|_| inventory_timeout("cluster inventory timed out"))??;
        Ok(cells
            .items
            .iter()
            .filter_map(|cell| proofstorm_kube::cell_identity(&cell.metadata))
            .filter(|identity| identity.workspace_id == workspace)
            .map(|identity| identity.instance_id)
            .collect())
    }

    /// The workspace's cells in full, from the API server's cache.
    pub(crate) async fn current_instances(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeMap<String, ProofstormCell>, Error> {
        let api = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let resources = tokio::time::timeout(
            GROWING_LIST_TIMEOUT,
            api.list(&kube::api::ListParams::default().match_any()),
        )
        .await
        .map_err(|_| inventory_timeout("cluster inventory timed out"))??;
        Ok(resources
            .items
            .into_iter()
            .filter(|cell| cell.spec.workspace_id == workspace)
            .map(|cell| (cell.spec.instance_id.clone(), cell))
            .collect())
    }

    /// The component status objects of one cell instance, or of every cell.
    /// Read them after the cells they belong to; see
    /// [`proofstorm_kube::assemble_component_statuses`].
    pub(crate) async fn component_objects(
        &self,
        instance_key: Option<&str>,
    ) -> Result<Vec<ProofstormComponent>, Error> {
        let api =
            Api::<ProofstormComponent>::namespaced(self.client.clone(), &self.control_namespace);
        let params = instance_key.map_or_else(kube::api::ListParams::default, |key| {
            kube::api::ListParams::default().labels(&format!("{INSTANCE_LABEL}={key}"))
        });
        Ok(api.list(&params).await.map_err(kube_error)?.items)
    }

    /// Fill each cell's component statuses from one list of every cell's
    /// component objects, in component id order.
    pub(crate) async fn assemble_components(
        &self,
        cells: &mut std::collections::BTreeMap<String, ProofstormCell>,
    ) -> Result<(), Error> {
        let api =
            Api::<ProofstormComponent>::namespaced(self.client.clone(), &self.control_namespace);
        let objects = tokio::time::timeout(
            GROWING_LIST_TIMEOUT,
            api.list(&kube::api::ListParams::default().match_any()),
        )
        .await
        .map_err(|_| inventory_timeout("component status inventory timed out"))?
        .map_err(kube_error)?
        .items;
        for cell in cells.values_mut() {
            proofstorm_kube::assemble_component_statuses(cell, None, &objects);
        }
        Ok(())
    }

    #[must_use]
    pub fn new(client: Client, control_namespace: String) -> Self {
        Self {
            revisions: std::sync::Arc::default(),
            client,
            cluster_source: kube::config::Kubeconfig::read()
                .ok()
                .and_then(|c| c.current_context)
                .unwrap_or_else(|| "default".into()),
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
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let matches = cells
            .list(&kube::api::ListParams::default())
            .await
            .map_err(kube_error)?
            .items
            .into_iter()
            .filter(|cell| {
                cell.spec.workspace_id == grant.workspace_id
                    && cell.spec.instance_id == grant.instance_id
            })
            .collect::<Vec<_>>();
        let [cell] = matches.as_slice() else {
            return Err(invalid_operation("private access cell unavailable"));
        };
        proofstorm_kube::require_open_cell(cell)
            .map_err(|e| Error::problem(e.code(), e.to_string()))?;
        let mut grants: BTreeMap<String, PrivateAccessGrant> = cell
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
        cells.patch(&cell.name_any(),&PatchParams::default(),&Patch::Merge(serde_json::json!({"metadata":{"resourceVersion":cell.resource_version(),"annotations":{PRIVATE_ACCESS_ANNOTATION:encoded}}}))).await.map_err(kube_error)?;
        Ok(())
    }
    pub async fn apply_action(
        &self,
        instance: &CellInstance,
        action: &ProofstormCellAction,
    ) -> Result<(), Error> {
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let cell = cells
            .get(&instance.resource_name)
            .await
            .map_err(kube_error)?;
        proofstorm_kube::require_open_cell(&cell)
            .map_err(|error| coded_invalid_request(error.code(), error.to_string()))?;
        let actions =
            Api::<ProofstormCellAction>::namespaced(self.client.clone(), &self.control_namespace);
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
        operation: &CellOperation,
    ) -> Result<Option<(OperationPhase, serde_json::Value)>, Error> {
        let actions =
            Api::<ProofstormCellAction>::namespaced(self.client.clone(), &self.control_namespace);
        let Some(action) = actions
            .get_opt(&operation.resource_name)
            .await
            .map_err(kube_error)?
        else {
            // The runtime resource is gone (cell closed, or expired by proofstormd)
            // before the journal saw a terminal phase.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| {
                    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
                });
            return Ok(vanished_action_outcome(operation, now));
        };
        let Some(status) = action.status else {
            return Ok(None);
        };
        Ok(terminal_action_observation(
            status,
            matches!(action.spec.action, CellAction::ComponentExecLive(_)),
        ))
    }
    pub async fn request_action_cancellation(
        &self,
        operation: &CellOperation,
        token: &str,
    ) -> Result<bool, Error> {
        let actions =
            Api::<ProofstormCellAction>::namespaced(self.client.clone(), &self.control_namespace);
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
        instance: CellInstance,
        revision: PublishedRevision,
    ) -> Result<CellInstanceStatus, Error> {
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let revision = proofstorm_kube::CellRevision {
            workspace_id: instance.workspace_id.clone(),
            instance_key: instance.instance_key.clone(),
            revision_digest: instance.revision_digest.clone(),
            cell: revision.cell,
            lock: revision.lock,
        };
        let mut resource = ProofstormCell::new(
            &instance.resource_name,
            ProofstormCellSpec {
                workspace_id: instance.workspace_id.clone(),
                instance_id: instance.id.clone(),
                instance_key: instance.instance_key.clone(),
                revision_digest: instance.revision_digest.clone(),
            },
        );
        resource.metadata.namespace = Some(self.control_namespace.clone());
        proofstorm_kube::identify_cell(&mut resource);
        if let Some(existing) = cells
            .get_opt(&instance.resource_name)
            .await
            .map_err(kube_error)?
        {
            proofstorm_kube::require_open_cell(&existing)
                .map_err(|e| coded_invalid_request(e.code(), e.to_string()))?;
            if existing.spec != resource.spec {
                return Err(coded_invalid_request(
                    "cell_update_conflict",
                    "Existing desired configuration differs; resume the accepted update instead of materializing an older revision",
                ));
            }
            let revision = self.store_revision(&existing, revision).await?;
            let observed = self.with_components(paired(existing, revision)?).await?;
            return Ok(status_from_resource(instance, &observed));
        }
        // The cell first, so its revision can be owned by it. Until the revision
        // is stored the controller leaves the cell pending.
        let applied = cells
            .create(&kube::api::PostParams::default(), &resource)
            .await
            .map_err(kube_error)?;
        let revision = self.store_revision(&applied, revision).await?;
        Ok(status_from_resource(instance, &paired(applied, revision)?))
    }

    /// Store `revision` for `cell`, owned by it, unless an identical one is
    /// already stored. Revisions are immutable: a different one under the same
    /// name is a conflict.
    pub(crate) async fn store_revision(
        &self,
        cell: &ProofstormCell,
        revision: proofstorm_kube::CellRevision,
    ) -> Result<std::sync::Arc<proofstorm_kube::CellRevision>, Error> {
        use kube::Resource;
        revision
            .verify()
            .map_err(|e| Error::problem("cell_revision_invalid", e.to_string()))?;
        let maps = Api::<ConfigMap>::namespaced(self.client.clone(), &self.control_namespace);
        let object = proofstorm_kube::revision_config_map(
            &revision,
            &self.control_namespace,
            cell.controller_owner_ref(&()),
        )
        .map_err(|e| Error::problem("cell_revision_too_large", e.to_string()))?;
        let name = object.metadata.name.clone().unwrap_or_default();
        let stored = match maps.get_opt(&name).await.map_err(kube_error)? {
            Some(existing) => existing,
            None => match maps
                .create(&kube::api::PostParams::default(), &object)
                .await
            {
                Ok(created) => created,
                Err(kube::Error::Api(error)) if error.code == 409 => {
                    maps.get(&name).await.map_err(kube_error)?
                }
                Err(error) => return Err(kube_error(error)),
            },
        };
        if proofstorm_kube::decode_revision(&stored).as_ref() != Ok(&revision) {
            return Err(Error::problem(
                "cell_revision_conflict",
                "Immutable revision differs from the expected cell configuration",
            ));
        }
        self.revisions.insert(revision.clone());
        Ok(std::sync::Arc::new(revision))
    }

    /// `cell` with the revision its digest names.
    pub(crate) async fn loaded(
        &self,
        cell: ProofstormCell,
    ) -> Result<proofstorm_kube::LoadedCell, Error> {
        self.revisions
            .loaded(&self.client, cell)
            .await
            .map_err(|e| Error::problem("cell_revision_unavailable", e.to_string()))
    }
    /// `cell` with its revision and its component statuses.
    pub(crate) async fn observed(&self, cell: ProofstormCell) -> Result<LoadedCell, Error> {
        let loaded = self.loaded(cell).await?;
        self.with_components(loaded).await
    }

    async fn with_components(&self, mut loaded: LoadedCell) -> Result<LoadedCell, Error> {
        let objects = self
            .component_objects(Some(&loaded.spec.instance_key))
            .await?;
        let LoadedCell { resource, revision } = &mut loaded;
        proofstorm_kube::assemble_component_statuses(resource, Some(&revision.cell), &objects);
        Ok(loaded)
    }

    pub async fn status(&self, instance: CellInstance) -> Result<CellInstanceStatus, Error> {
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        if let Some(resource) = cells
            .get_opt(&instance.resource_name)
            .await
            .map_err(kube_error)?
        {
            if resource.spec.instance_key != instance.instance_key
                || resource.spec.workspace_id != instance.workspace_id
                || resource.spec.instance_id != instance.id
            {
                return Err(Error::problem(
                    "stale_incarnation",
                    "Runtime resource belongs to a different cell incarnation",
                ));
            }
            return Ok(status_from_resource(
                instance,
                &self.observed(resource).await?,
            ));
        }
        let receipts = Api::<ConfigMap>::namespaced(self.client.clone(), &self.control_namespace);
        let name = format!("proofstorm-teardown-{}", instance.instance_key);
        let receipt = receipts.get_opt(&name).await.map_err(kube_error)?;
        let Some(receipt) = receipt else {
            return Err(Error::missing(
                format!(
                    "cell instance {:?} has no runtime resource or teardown receipt",
                    instance.id
                ),
                Some(serde_json::json!({"code": "runtime_not_found"})),
            ));
        };
        let data = receipt.data.unwrap_or_default();
        Ok(CellInstanceStatus {
            observed_generation: 0,
            observed_revision_digest: String::new(),
            last_converged_revision: None,
            retained_storage: BTreeMap::new(),
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
    pub async fn close(&self, instance: CellInstance) -> Result<CellInstanceStatus, Error> {
        let mut status = match self.status(instance.clone()).await {
            Ok(status) => status,
            Err(error) if error.kind == crate::ErrorKind::Missing => {
                return self.removal_status(instance).await;
            }
            Err(error) => return Err(error),
        };
        if status.phase == InstancePhase::Closed {
            return Ok(status);
        }
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        if let Some(resource) = cells.get_opt(&instance.resource_name).await? {
            if resource.spec.instance_key != instance.instance_key {
                return Err(Error::problem(
                    "stale_incarnation",
                    "Cell incarnation changed",
                ));
            }
            let uid = resource
                .uid()
                .ok_or_else(|| Error::problem("runtime_identity_missing", "Cell UID missing"))?;
            match cells
                .delete(
                    &instance.resource_name,
                    &DeleteParams {
                        preconditions: Some(kube::api::Preconditions {
                            uid: Some(uid),
                            resource_version: resource.resource_version(),
                        }),
                        ..Default::default()
                    },
                )
                .await
            {
                // Finalization may win the race between GET and DELETE. A 404
                // requires verification; it is not itself a cleanup receipt.
                Err(kube::Error::Api(error)) if error.code == 404 => {
                    return self.removal_status(instance).await;
                }
                // The controller's status writes advance resourceVersion, so the
                // precondition can lose to a reconcile. Nothing was deleted; the
                // caller's poll re-reads the same incarnation and retries.
                Err(kube::Error::Api(error)) if error.code == 409 => {
                    status.phase = InstancePhase::Closing;
                    status.message = Some("cell changed before deletion; retrying".into());
                    return Ok(status);
                }
                result => {
                    result.map_err(kube_error)?;
                }
            }
        }
        status.phase = InstancePhase::Closing;
        status.message = Some("deleting instance namespace and verifying absence".into());
        Ok(status)
    }

    /// Verify both runtime identity and namespace absence when startup never
    /// reached the controller, so there is no controller teardown receipt.
    pub async fn verify_absent(&self, instance: CellInstance) -> Result<CellInstanceStatus, Error> {
        let status = self.removal_status(instance).await?;
        if status.phase != InstancePhase::Closed {
            return Err(Error::problem(
                "cleanup_unverified",
                "runtime resource or instance namespace still exists",
            ));
        }
        Ok(status)
    }

    /// Observe teardown by exact incarnation, including after its local record
    /// or controller receipt has been collected. Failed reads remain errors.
    pub(crate) async fn removal_status(
        &self,
        instance: CellInstance,
    ) -> Result<CellInstanceStatus, Error> {
        let cells = Api::<ProofstormCell>::namespaced(self.client.clone(), &self.control_namespace);
        let namespace = proofstorm_kube::instance_namespace(&instance.instance_key);
        let pending = cells.get_opt(&instance.resource_name).await?.is_some()
            || Api::<Namespace>::all(self.client.clone())
                .get_opt(&namespace)
                .await?
                .is_some();
        Ok(CellInstanceStatus {
            observed_generation: 0,
            observed_revision_digest: String::new(),
            last_converged_revision: None,
            retained_storage: BTreeMap::new(),
            instance: instance.clone(),
            phase: if pending {
                InstancePhase::Closing
            } else {
                InstancePhase::Closed
            },
            instance_namespace: namespace.clone(),
            components: vec![],
            inventory: vec![],
            teardown_receipt: (!pending).then(|| CoreTeardownReceipt {
                instance_id: instance.id,
                instance_namespace: namespace,
                inventory_digest: proofstorm_core::digest_json(&Vec::<serde_json::Value>::new()),
                verified_absent: true,
            }),
            message: Some(
                if pending {
                    "runtime resource or instance namespace still exists; waiting for cleanup"
                } else {
                    "absence verified directly; no controller teardown receipt was available"
                }
                .into(),
            ),
        })
    }
}
fn paired(
    cell: ProofstormCell,
    revision: std::sync::Arc<proofstorm_kube::CellRevision>,
) -> Result<proofstorm_kube::LoadedCell, Error> {
    proofstorm_kube::LoadedCell::new(cell, revision)
        .map_err(|e| Error::problem("cell_revision_mismatch", e.to_string()))
}

#[must_use]
pub fn status_from_resource(
    instance: CellInstance,
    loaded: &proofstorm_kube::LoadedCell,
) -> CellInstanceStatus {
    let mut resource = loaded.resource.clone();
    proofstorm_kube::probes::expire_cell_status(
        &mut resource,
        &loaded.revision.cell,
        &loaded.revision.lock,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
            }),
    );
    status_from_current_resource(instance, &resource)
}

// The caller has already expired protocol observations on this resource.
pub(crate) fn status_from_current_resource(
    instance: CellInstance,
    resource: &ProofstormCell,
) -> CellInstanceStatus {
    let mut status = resource.status.clone().unwrap_or_default();
    if status.observed_desired_generation != instance.generation
        || status.observed_revision_digest != instance.revision_digest
        || status.observed_generation != resource.metadata.generation
    {
        if status.phase == CellPhase::Ready {
            status.phase = CellPhase::Pending;
        }
        if status.phase != CellPhase::Blocked {
            status.message = Some("Reconciling the desired cell revision; readiness from an older generation is not completion.".into());
        }
    }
    CellInstanceStatus {
        observed_generation: status.observed_desired_generation,
        observed_revision_digest: status.observed_revision_digest,
        last_converged_revision: status.last_converged_revision,
        retained_storage: status.retained_storage,
        instance,
        phase: match status.phase {
            CellPhase::Pending => InstancePhase::Pending,
            CellPhase::Blocked => InstancePhase::Blocked,
            CellPhase::Ready => InstancePhase::Ready,
            CellPhase::Closing => InstancePhase::Closing,
            CellPhase::CleanupBlocked => InstancePhase::CleanupBlocked,
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
    status: proofstorm_kube::ProofstormCellActionStatus,
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
/// A running operation whose resource vanished is a terminal outcome, never a
/// live one. A pending operation may simply not be applied yet, unless it is
/// older than proofstormd keeps finished actions.
fn vanished_action_outcome(
    operation: &CellOperation,
    now_unix: i64,
) -> Option<(OperationPhase, serde_json::Value)> {
    let expired = now_unix
        > operation
            .accepted_at_unix
            .saturating_add(FINISHED_ACTION_RETENTION_SECONDS);
    (operation.phase == OperationPhase::Running || expired)
        .then(|| (OperationPhase::Failed, missing_action_artifact(operation)))
}
#[must_use]
pub fn missing_action_artifact(operation: &CellOperation) -> serde_json::Value {
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
    instance: &CellInstance,
    operation: &CellOperation,
    action: CellAction,
) -> ProofstormCellAction {
    let mut resource = ProofstormCellAction::new(
        &operation.resource_name,
        ProofstormCellActionSpec {
            access_scope: None,
            cell_name: instance.resource_name.clone(),
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
    resource.annotations_mut().insert(
        "proofstorm.dev/action-revision".into(),
        operation.revision_digest.clone(),
    );
    resource.metadata.labels = Some(std::collections::BTreeMap::from([
        (
            "proofstorm.dev/instance".to_owned(),
            instance.instance_key.clone(),
        ),
        (
            "proofstorm.dev/cell".to_owned(),
            instance.resource_name.clone(),
        ),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstorm-mcp".to_owned(),
        ),
    ]));
    resource
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vanished_pending_action_fails_once_it_could_have_expired() {
        let mut operation = CellOperation {
            revision_digest: String::new(),
            id: "op".into(),
            workspace_id: "workspace".into(),
            instance_id: "instance".into(),
            experiment_id: "run".into(),
            session_id: "session".into(),
            principal_id: "principal".into(),
            sequence: 1,
            kind: proofstorm_core::OperationKind::ComponentExecLive,
            capability: proofstorm_core::Capability::ComponentExecLive,
            resource_name: "op-resource".into(),
            request_digest: "sha256:request".into(),
            request: serde_json::json!({}),
            phase: OperationPhase::Pending,
            accepted_at_unix: 1_000,
            started_at_unix: None,
            completed_at_unix: None,
            artifact: None,
        };
        let deadline = 1_000 + FINISHED_ACTION_RETENTION_SECONDS;
        assert_eq!(vanished_action_outcome(&operation, deadline), None);
        let (phase, artifact) = vanished_action_outcome(&operation, deadline + 1).unwrap();
        assert_eq!(phase, OperationPhase::Failed);
        assert_eq!(artifact["code"], "action_runtime_not_found");
        operation.phase = OperationPhase::Running;
        assert!(vanished_action_outcome(&operation, 1_000).is_some());
    }
}
