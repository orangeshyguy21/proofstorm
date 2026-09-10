//! Shared lifecycle reconciliation. Only successful, exact absence checks permit purge.
use crate::{Error, Runtime};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, Preconditions},
};
use proofstorm_core::{Capability, LabInstance, LabInstanceStatus, PublishedRevision};
use proofstorm_kube::ProofstormLab;
use proofstorm_store::{LifecycleGuard, RuntimeBinding, Store, StoreError};
use std::time::Duration;

pub async fn guard(store: &Store) -> Result<LifecycleGuard, Error> {
    for _ in 0..100 {
        if let Some(guard) = store.try_lifecycle_guard()? {
            return Ok(guard);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(Error::problem(
        "lifecycle_busy",
        "Another lab lifecycle transition is in progress; retry this request",
    ))
}
async fn identity(runtime: &Runtime) -> Result<RuntimeBinding, Error> {
    let ns = Api::<Namespace>::all(runtime.client.clone())
        .get("kube-system")
        .await?;
    let uid = ns.uid().ok_or_else(|| {
        Error::problem(
            "cluster_identity_missing",
            "Kubernetes did not return its namespace UID; no cleanup performed",
        )
    })?;
    Ok(RuntimeBinding {
        source: format!("{}:{}", runtime.cluster_source, runtime.control_namespace),
        cluster_uid: uid,
        resource_uid: None,
    })
}
fn mismatch() -> Error {
    Error::problem(
        "stale_incarnation",
        "This runtime does not match the recorded lab incarnation; read the current lab and replan",
    )
}

/// Check the cluster binding before a lifecycle write revokes authority or deletes anything.
pub(crate) async fn validate_runtime(
    runtime: &Runtime,
    store: &Store,
    instance: &LabInstance,
) -> Result<(), Error> {
    let current = identity(runtime).await?;
    if store.runtime_binding(instance)?.is_some_and(|binding| {
        binding.source != current.source || binding.cluster_uid != current.cluster_uid
    }) {
        return Err(Error::problem(
            "lab_cluster_mismatch",
            "This lab belongs to another cluster; select its context before changing it",
        ));
    }
    Ok(())
}

/// Caller holds the lifecycle guard across reconciliation and subsequent creation.
/// Returns true when an old instance was purged. A never-materialized intent is retained.
pub async fn reconcile_name(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    id: &str,
) -> Result<bool, Error> {
    store.authorize(workspace, principal, Capability::LabStatus)?;
    let instance = match store.instance(workspace, principal, id) {
        Ok(i) => i,
        Err(StoreError::NotFound { .. }) => {
            identity(runtime).await?;
            store.purge_unmaterialized_handle(workspace, id)?;
            return Ok(false);
        }
        Err(e) => return Err(e.into()),
    };
    let mut current = identity(runtime).await?;
    let binding = store.runtime_binding(&instance)?;
    if binding.as_ref().is_some_and(|b| b.source != current.source) {
        return Err(Error::problem(
            "lab_cluster_mismatch",
            "This name is tracked in another cluster context; select that context or use a separate workspace/database",
        ));
    }
    let api = Api::<ProofstormLab>::namespaced(runtime.client.clone(), &runtime.control_namespace);
    if let Some(lab) = api.get_opt(&instance.resource_name).await? {
        if lab.spec.instance_key != instance.instance_key
            || lab.spec.instance_id != instance.id
            || lab.spec.workspace_id != workspace
        {
            return Err(mismatch());
        }
        current.resource_uid = lab.uid();
        if current.resource_uid.is_none() {
            return Err(mismatch());
        }
        store.bind_runtime(&instance, &current)?;
        return Ok(false);
    }
    // A Pending pod still has a CR. Only a CR that has never been created is an intent.
    if binding
        .as_ref()
        .is_some_and(|b| b.resource_uid.is_none() && b.cluster_uid == current.cluster_uid)
        && !store.update_state(workspace, principal, id)?.closing
    {
        return Ok(false);
    }
    runtime.verify_absent(instance.clone()).await?;
    remove_receipts(runtime, &instance, binding.as_ref()).await?;
    store.purge_lab(&instance)?;
    Ok(true)
}

/// Existing desired revisions are resumable; deleted labs are not resurrected by retries.
pub async fn materialize_locked(
    runtime: &Runtime,
    store: &Store,
    instance: LabInstance,
    revision: PublishedRevision,
) -> Result<LabInstanceStatus, Error> {
    let mut binding = identity(runtime).await?;
    store.bind_runtime(&instance, &binding)?;
    store.record_plan_use(&instance, None)?;
    let status = runtime.materialize(instance.clone(), revision).await?;
    let resource =
        Api::<ProofstormLab>::namespaced(runtime.client.clone(), &runtime.control_namespace)
            .get(&instance.resource_name)
            .await?;
    binding.resource_uid = resource.uid();
    if binding.resource_uid.is_none() {
        return Err(mismatch());
    }
    store.bind_runtime(&instance, &binding)?;
    Ok(status)
}

#[allow(
    clippy::too_many_arguments,
    reason = "shared creation entry point binds runtime, identity, revision, and retry context"
)]
pub async fn materialize(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    id: &str,
    revision: &str,
    key: &str,
    plan: Option<&str>,
) -> Result<LabInstanceStatus, Error> {
    store.authorize(workspace, principal, Capability::LabMaterialize)?;
    let _guard = guard(store).await?;
    reconcile_name(runtime, store, workspace, principal, id).await?;
    if let Some(plan) = plan {
        store.read_draft(workspace, principal, plan)?;
    }
    let instance = store.materialize(workspace, principal, id, revision, key)?;
    store.record_plan_use(&instance, plan)?;
    let revision =
        store.revision_for_materialize(workspace, principal, &instance.revision_digest)?;
    materialize_locked(runtime, store, instance, revision).await
}

/// A bounded page per tick; errors never become an empty inventory or a purge decision.
pub async fn sweep(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    cursor: &str,
) -> Result<String, Error> {
    store.authorize(workspace, principal, Capability::LabStatus)?;
    let Some(_guard) = store.try_lifecycle_guard()? else {
        return Ok(cursor.into());
    };
    // Establish that the selected cluster is reachable before touching any record.
    identity(runtime).await?;
    let (entries, next) = store.environment_entries(workspace, principal, cursor, 20)?;
    let mut failure = None;
    for entry in entries {
        match tokio::time::timeout(
            Duration::from_secs(3),
            reconcile_name(runtime, store, workspace, principal, &entry.id),
        )
        .await
        {
            Ok(Err(e))
                if e.details
                    .as_ref()
                    .is_some_and(|d| d["code"] == "lab_cluster_mismatch") => {}
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                failure.get_or_insert(error);
            }
            Err(_) => {
                failure.get_or_insert(Error::failure(
                    "Lifecycle reconciliation timed out; records retained",
                    Some(serde_json::json!({"code":"runtime_failure"})),
                ));
            }
        }
    }
    let next = next.unwrap_or_default();
    if let Some(mut error) = failure {
        error.details.get_or_insert_with(|| serde_json::json!({}))["next_cursor"] =
            serde_json::json!(next);
        return Err(error);
    }
    Ok(next)
}

async fn remove_receipts(
    runtime: &Runtime,
    instance: &LabInstance,
    binding: Option<&RuntimeBinding>,
) -> Result<(), Error> {
    let api = Api::<ConfigMap>::namespaced(runtime.client.clone(), &runtime.control_namespace);
    // Namespace deletion collects lab workloads. Snapshots normally disappear via owner GC;
    // remove only exact recorded owners, plus the controller's verified teardown receipt.
    let maps = api.list(&ListParams::default()).await?;
    for map in maps {
        let owned = map.owner_references().iter().any(|owner| {
            owner.kind == "ProofstormLab"
                && owner.name == instance.resource_name
                && binding.and_then(|b| b.resource_uid.as_deref()) == Some(owner.uid.as_str())
        });
        let receipt = map.name_any() == format!("proofstorm-teardown-{}", instance.instance_key)
            && map.data.as_ref().is_some_and(|d| {
                d.get("instanceNamespace")
                    == Some(&proofstorm_kube::instance_namespace(&instance.instance_key))
                    && d.get("verifiedAbsent").is_some_and(|v| v == "true")
            });
        if owned || receipt {
            let uid = map.uid().ok_or_else(mismatch)?;
            match api
                .delete(
                    &map.name_any(),
                    &DeleteParams {
                        preconditions: Some(Preconditions {
                            uid: Some(uid),
                            resource_version: map.resource_version(),
                        }),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(e)) if e.code == 404 => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
