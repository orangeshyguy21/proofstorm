//! Resume accepted lab edits without allowing an older request to overwrite newer desired state.
use crate::{Error, Runtime};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Resource, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use proofstorm_core::{InstancePhase, LabInstanceStatus};
use proofstorm_kube::{ProofstormLab, ProofstormLabSpec};
use proofstorm_store::Store;

pub const GENERATION: &str = "proofstorm.dev/desired-generation";
pub const DELETE_DATA: &str = "proofstorm.dev/delete-component-data";

pub async fn snapshot(runtime: &Runtime, lab: &ProofstormLab) -> Result<(), Error> {
    let name = proofstorm_core::digest_json(&(&lab.spec.instance_key, &lab.spec.revision_digest));
    let maps = Api::<ConfigMap>::namespaced(runtime.client.clone(), &runtime.control_namespace);
    let name = format!("revision-{}", &name[7..39]);
    let value = serde_json::json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"ownerReferences":lab.controller_owner_ref(&()).into_iter().collect::<Vec<_>>()},"immutable":true,"data":{"spec.json":serde_json::to_string(&lab.spec).map_err(|e|Error::problem("serialization_failed",e.to_string()))?}});
    if let Some(existing) = maps.get_opt(&name).await.map_err(runtime_error)? {
        let expected = serde_json::to_string(&lab.spec)
            .map_err(|e| Error::problem("serialization_failed", e.to_string()))?;
        if existing.data.as_ref().and_then(|d| d.get("spec.json")) != Some(&expected) {
            return Err(Error::problem(
                "lab_revision_conflict",
                "Immutable revision snapshot differs from the expected lab configuration",
            ));
        }
    } else {
        maps.patch(
            &name,
            &PatchParams::apply("proofstorm-mcp"),
            &Patch::Apply(value),
        )
        .await
        .map_err(runtime_error)?;
    }
    Ok(())
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the runtime error"
)]
fn runtime_error(e: kube::Error) -> Error {
    let status = match &e {
        kube::Error::Api(response) => Some(response.code),
        _ => None,
    };
    Error::failure(
        e.to_string(),
        Some(serde_json::json!({"code":"lab_update_runtime", "http_status":status})),
    )
}

pub async fn reconcile(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    id: &str,
) -> Result<LabInstanceStatus, Error> {
    for attempt in 0..4 {
        let result = reconcile_once(runtime, store, workspace, principal, id).await;
        if result.as_ref().is_err_and(|error| {
            error
                .details
                .as_ref()
                .is_some_and(|details| details["http_status"] == 409)
        }) && attempt < 3
        {
            // Controller status writes also change resourceVersion. Reread both the
            // journal and the resource; never retry a stale replacement body.
            tokio::time::sleep(std::time::Duration::from_millis(10 << attempt)).await;
            continue;
        }
        return result;
    }
    unreachable!("the final reconciliation attempt always returns")
}

async fn reconcile_once(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    principal: &str,
    id: &str,
) -> Result<LabInstanceStatus, Error> {
    // Always reread durable desired state, including when replaying an old accepted request.
    let instance = store.instance(workspace, principal, id)?;
    let state = store.update_state(workspace, principal, id)?;
    if state.closing {
        return runtime.status(instance).await;
    }
    let revision =
        store.revision_for_materialize(workspace, principal, &instance.revision_digest)?;
    let labs = Api::<ProofstormLab>::namespaced(runtime.client.clone(), &runtime.control_namespace);
    let Some(mut lab) = labs
        .get_opt(&instance.resource_name)
        .await
        .map_err(runtime_error)?
    else {
        // Never resurrect an externally deleted or closing lab as a side effect of recovery.
        return Err(Error::problem(
            "lab_update_runtime_missing",
            "existing lab is absent; edit reconciliation will not recreate it",
        ));
    };
    proofstorm_kube::require_open_lab(&lab).map_err(|e| Error::problem(e.code(), e.to_string()))?;
    let observed_desired = lab
        .annotations()
        .get(GENERATION)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);
    if observed_desired > state.generation {
        return Err(Error::problem(
            "lab_update_superseded",
            "cluster already has a newer desired generation",
        ));
    }
    if lab.spec.revision_digest != revision.digest || observed_desired != state.generation {
        snapshot(runtime, &lab).await?;
        let actions = Api::<proofstorm_kube::ProofstormLabAction>::namespaced(
            runtime.client.clone(),
            &runtime.control_namespace,
        );
        for action in actions
            .list(&kube::api::ListParams::default().labels(&format!(
                "proofstorm.dev/instance={}",
                instance.instance_key
            )))
            .await
            .map_err(runtime_error)?
            .items
        {
            if !action
                .annotations()
                .contains_key("proofstorm.dev/action-revision")
            {
                actions.patch(&action.name_any(),&PatchParams::default(),&Patch::Merge(serde_json::json!({"metadata":{"resourceVersion":action.metadata.resource_version,"annotations":{"proofstorm.dev/action-revision":lab.spec.revision_digest}}}))).await.map_err(runtime_error)?;
            }
        }
        lab.spec = ProofstormLabSpec {
            workspace_id: workspace.into(),
            instance_id: id.into(),
            instance_key: instance.instance_key.clone(),
            revision_digest: revision.digest,
            lock: revision.lock,
            lab: revision.lab,
        };
        lab.annotations_mut()
            .insert(GENERATION.into(), state.generation.to_string());
        let deleted = store.pending_deleted_data(workspace, principal, id)?;
        lab.annotations_mut().insert(
            DELETE_DATA.into(),
            serde_json::to_string(&deleted)
                .map_err(|e| Error::problem("serialization_failed", e.to_string()))?,
        );
        // replace uses resourceVersion: simultaneous edit/close or a newer update makes this fail.
        lab = labs
            .replace(&instance.resource_name, &PostParams::default(), &lab)
            .await
            .map_err(runtime_error)?;
        snapshot(runtime, &lab).await?;
    }
    let status = crate::runtime::status_from_resource(instance, &lab);
    if status.phase == InstancePhase::Ready {
        store.mark_update_applied(
            workspace,
            id,
            state.generation,
            Some(&status.instance.revision_digest),
        )?;
    }
    Ok(status)
}

/// Resume durable accepted writes while a control client is running, independently of reads.
#[must_use]
pub fn start_recovery(
    runtime: Runtime,
    store: Store,
    workspace: String,
    principal: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cleanup_cursor = String::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match crate::lifecycle::sweep(&runtime, &store, &workspace, &principal, &cleanup_cursor)
                .await
            {
                Ok(next) => cleanup_cursor = next,
                Err(error) => {
                    if let Some(next) = error
                        .details
                        .as_ref()
                        .and_then(|d| d["next_cursor"].as_str())
                    {
                        cleanup_cursor = next.into();
                    }
                    eprintln!("lab lifecycle reconciliation: {error}");
                }
            }
            let Ok(ids) = store.pending_updates(&workspace, &principal) else {
                continue;
            };
            for id in ids {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    reconcile(&runtime, &store, &workspace, &principal, &id),
                )
                .await;
            }
        }
    })
}
