//! Adopt old inventories and reconcile resources removed from a live cell.
use super::{
    Api, BTreeMap, BTreeSet, Context, DeleteParams, Error, ListParams, ProofstormCell,
    ProofstormCellAction, ResourceExt, instance_namespace,
};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, Preconditions};

/// The cell as an action sees it: at the revision the action was submitted
/// against when it names one, otherwise at the cell's current revision.
pub async fn action_cell(
    action: &ProofstormCellAction,
    cell: ProofstormCell,
    context: &Context,
) -> Result<proofstorm_kube::LoadedCell, Error> {
    let Some(revision) = action
        .annotations()
        .get("proofstorm.dev/action-revision")
        .filter(|revision| !revision.is_empty() && **revision != cell.spec.revision_digest)
        .cloned()
    else {
        let loaded = context.revisions.loaded(&context.client, cell).await?;
        return super::with_component_statuses(context, loaded).await;
    };
    let bound = context
        .revisions
        .load(&context.client, &cell, &revision)
        .await?;
    let loaded = proofstorm_kube::LoadedCell::at_revision(cell, bound)?;
    let mut loaded = super::with_component_statuses(context, loaded).await?;
    if let Some(status) = loaded.resource.status.as_mut() {
        for component in &mut status.components {
            if loaded.revision.lock.entries.iter().any(|e| {
                e.component_id == component.id
                    && e.rollout_digest == component.observed_rollout_digest
            }) {
                component.observed_revision_digest.clone_from(&revision);
            }
        }
        status.observed_revision_digest.clone_from(&revision);
    }
    Ok(loaded)
}

pub async fn prune(
    cell: &proofstorm_kube::LoadedCell,
    context: &Context,
) -> Result<
    (
        bool,
        Vec<proofstorm_core::InventoryEntry>,
        BTreeMap<String, String>,
    ),
    Error,
> {
    let namespace = instance_namespace(&cell.spec.instance_key);
    let desired = cell
        .revision
        .cell
        .components
        .iter()
        .map(|c| c.id.as_str())
        .collect::<BTreeSet<_>>();
    let delete_data: Vec<String> = cell
        .annotations()
        .get("proofstorm.dev/delete-component-data")
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default();
    let mut complete = true;
    let mut retained = vec![];
    let mut storage = BTreeMap::new();
    for (group, version, kind) in [
        ("apps", "v1", "Deployment"),
        ("apps", "v1", "StatefulSet"),
        ("", "v1", "Service"),
        ("", "v1", "ConfigMap"),
        ("", "v1", "Secret"),
        ("", "v1", "PersistentVolumeClaim"),
        ("networking.k8s.io", "v1", "NetworkPolicy"),
    ] {
        let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
        let api = Api::<DynamicObject>::namespaced_with(context.client.clone(), &namespace, &ar);
        let selector = format!(
            "proofstorm.dev/instance={},app.kubernetes.io/managed-by=proofstormd",
            cell.spec.instance_key
        );
        for resource in api
            .list(&ListParams::default().labels(&selector))
            .await?
            .items
        {
            let Some(component) = resource.labels().get("proofstorm.dev/component") else {
                continue;
            };
            if desired.contains(component.as_str()) {
                continue;
            }
            if !owned_resource(cell, &namespace, kind, component, &resource.name_any()) {
                continue;
            }
            // Keep pending removals in inventory until actual deletion is observed.
            retained.push(proofstorm_core::InventoryEntry {
                api_version: ar.api_version.clone(),
                kind: kind.into(),
                namespace: namespace.clone(),
                name: resource.name_any(),
            });
            if matches!(kind, "PersistentVolumeClaim" | "Secret")
                && !delete_data.contains(component)
            {
                if kind == "PersistentVolumeClaim" {
                    storage.insert(
                        resource.name_any(),
                        resource
                            .data
                            .pointer("/spec/resources/requests/storage")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_owned(),
                    );
                }
                continue;
            }
            complete = false;
            if resource.metadata.deletion_timestamp.is_none() {
                api.delete(
                    &resource.name_any(),
                    &DeleteParams {
                        preconditions: Some(Preconditions {
                            uid: resource.metadata.uid.clone(),
                            resource_version: resource.metadata.resource_version.clone(),
                        }),
                        ..Default::default()
                    },
                )
                .await?;
            }
        }
    }
    Ok((complete, retained, storage))
}

fn owned_resource(
    cell: &ProofstormCell,
    namespace: &str,
    kind: &str,
    component: &str,
    name: &str,
) -> bool {
    cell.status.as_ref().is_some_and(|status| {
        status.inventory.iter().any(|entry| {
            (entry.kind == kind && entry.name == name && entry.namespace == namespace)
                || (kind == "PersistentVolumeClaim"
                    && entry.kind == "StatefulSet"
                    && entry.name == component
                    && name == format!("data-{component}-0"))
        })
    })
}
