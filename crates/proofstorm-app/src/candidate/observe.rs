use crate::{Error, Runtime};
use futures::{StreamExt, stream};
use kube::Api;
use proofstorm_core::CandidateBuild;
use proofstorm_kube::ProofstormCandidateBuild;
use proofstorm_store::Store;

pub async fn observe(
    runtime: &Runtime,
    store: &Store,
    workspace: &str,
    candidate: CandidateBuild,
) -> Result<CandidateBuild, Error> {
    if candidate.phase.terminal() {
        return Ok(candidate);
    }
    let builds = Api::<ProofstormCandidateBuild>::namespaced(
        runtime.client.clone(),
        &runtime.control_namespace,
    );
    let Some(resource) = builds.get_opt(&candidate.resource_name).await? else {
        return Ok(candidate);
    };
    if resource.spec.workspace_id != candidate.workspace_id
        || resource.spec.candidate_id != candidate.id
        || resource.spec.principal_id != candidate.principal_id
        || resource.spec.implementation != candidate.implementation
        || resource.spec.base_version != candidate.base_version
        || resource.spec.pull_request_url != candidate.pull_request_url
        || resource.spec.provenance != candidate.provenance
        || resource.spec.request_digest != candidate.request_digest
        || Some(&resource.spec.repository) != candidate.repository.as_ref()
        || Some(&resource.spec.commit_sha) != candidate.commit_sha.as_ref()
        || Some(&resource.spec.version) != candidate.version.as_ref()
        || resource.spec.accepted_at_unix != candidate.accepted_at_unix
    {
        return Err(Error::problem(
            "candidate_identity_conflict",
            "Runtime candidate identity does not match the durable record",
        ));
    }
    let Some(status) = resource.status else {
        return Ok(candidate);
    };
    let mut updated = candidate.clone();
    updated.phase = status.phase;
    updated.started_at_unix = status.started_at_unix;
    updated.completed_at_unix = status.completed_at_unix;
    updated.image = status.image;
    updated.error_code = status.error_code;
    updated.error_message = status.message;
    updated.diagnostics = status.diagnostics;
    if updated == candidate {
        return Ok(candidate);
    }
    store
        .update_candidate_build(workspace, &updated)
        .map_err(Error::from)
}

pub async fn sweep(cells: &crate::cell::Cells, after: &str) -> Result<(String, bool), Error> {
    if cells
        .store
        .authorize(
            &cells.workspace,
            &cells.principal,
            proofstorm_core::Capability::CandidateRead,
        )
        .is_err()
    {
        return Ok((String::new(), false));
    }
    let page =
        cells
            .store
            .candidate_build_page(&cells.workspace, &cells.principal, after, true, 20)?;
    let next = if page.len() == 20 {
        page.last().map(|c| c.id.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    let outcomes = stream::iter(page)
        .map(|candidate| async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                observe(&cells.runtime, &cells.store, &cells.workspace, candidate),
            )
            .await
            .is_ok_and(|result| result.is_ok())
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    Ok((next, outcomes.contains(&false)))
}
