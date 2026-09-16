//! Shared candidate admission and observation. Reads never initiate builds.
mod observe;
mod receipt;
mod source;
#[cfg(test)]
mod tests;
pub use receipt::receipt;
mod directory;
pub use directory::directory;
mod read;
use crate::Error;
pub use observe::{observe, sweep};
use proofstorm_core::{
    CandidateBuild, CandidateBuildPhase, CandidateInput, CandidateProvenance, Capability,
    candidate_build_profile, default_catalog, digest_json,
};
use proofstorm_store::{Store, StoreError};
pub use read::{CandidateReadQuery, read};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateBuildRequest {
    pub candidate_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pull_request_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CandidateInput>,
    pub implementation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_profile: Option<String>,
    #[serde(rename = "request_id")]
    pub idempotency_key: String,
}

pub async fn admit(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &CandidateBuildRequest,
) -> Result<CandidateBuild, Error> {
    admit_with_resolver(
        store,
        workspace,
        principal,
        request,
        |source, repository| async move { source::resolve(&source, &repository).await },
    )
    .await
}

async fn admit_with_resolver<F, Fut>(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &CandidateBuildRequest,
    resolve: F,
) -> Result<CandidateBuild, Error>
where
    F: FnOnce(CandidateInput, String) -> Fut,
    Fut: std::future::Future<Output = Result<(String, String), Error>>,
{
    for cap in [
        Capability::CandidateBuild,
        Capability::CandidateRead,
        Capability::CatalogRead,
    ] {
        store.authorize(workspace, principal, cap)?;
    }
    validate_request(request)?;
    let profile = candidate_build_profile(&request.implementation).ok_or_else(|| {
        Error::problem(
            "candidate_implementation_unsupported",
            "This implementation has no source build profile",
        )
    })?;
    let source = match (&request.source, request.pull_request_url.is_empty()) {
        (Some(source), true) => source.clone(),
        (None, false) => CandidateInput::PullRequest {
            url: request.pull_request_url.clone(),
        },
        _ => {
            return Err(Error::problem(
                "candidate_source_invalid",
                "Supply exactly one of source or legacy pull_request_url",
            ));
        }
    }
    .normalized(&profile.repository)
    .map_err(|message| Error::problem("candidate_source_invalid", message))?;
    let input_digest = digest_json(&(
        &request.candidate_id,
        &request.implementation,
        &source,
        &request.base_version,
        &request.build_profile,
    ));
    if let Some(candidate) = store.candidate_request_replay(
        workspace,
        principal,
        &request.idempotency_key,
        &input_digest,
    )? {
        return Ok(candidate);
    }
    match store.candidate_build(workspace, principal, &request.candidate_id) {
        Ok(candidate) => {
            let same = candidate.provenance.as_ref().map_or_else(
                || {
                    candidate.implementation == request.implementation
                        && (CandidateInput::PullRequest {
                            url: candidate.pull_request_url.clone(),
                        })
                        .normalized(&profile.repository)
                        .as_ref()
                            == Ok(&source)
                        && request
                            .base_version
                            .as_ref()
                            .is_none_or(|v| v == &candidate.base_version)
                        && request.build_profile.is_none()
                },
                |p| p.input_digest == input_digest,
            );
            if !same || candidate.principal_id != principal {
                return Err(Error::problem(
                    "candidate_identity_conflict",
                    "candidate_id already identifies a different request",
                ));
            }
            store.record_candidate_request(
                workspace,
                principal,
                &request.idempotency_key,
                &input_digest,
                &candidate,
            )?;
            return Ok(candidate);
        }
        Err(StoreError::NotFound { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    resolve_and_record(
        store,
        workspace,
        principal,
        request,
        Admission {
            profile,
            source,
            input_digest,
        },
        resolve,
    )
    .await
}

async fn resolve_and_record<F, Fut>(
    store: &Store,
    workspace: &str,
    principal: &str,
    request: &CandidateBuildRequest,
    admission: Admission,
    resolve: F,
) -> Result<CandidateBuild, Error>
where
    F: FnOnce(CandidateInput, String) -> Fut,
    Fut: std::future::Future<Output = Result<(String, String), Error>>,
{
    let Admission {
        profile,
        source,
        input_digest,
    } = admission;
    if request
        .build_profile
        .as_ref()
        .is_some_and(|id| id != &profile.id)
    {
        return Err(Error::problem(
            "candidate_profile_unsupported",
            "Select the registered build profile for this implementation",
        ));
    }
    let base_version = request
        .base_version
        .clone()
        .unwrap_or_else(|| profile.baseline.clone());
    validate_shared_baselines(&profile, &base_version)?;
    let base = default_catalog()
        .entries
        .iter()
        .find(|e| e.id == request.implementation && e.version == base_version && e.source.is_none())
        .ok_or_else(|| {
            Error::problem(
                "candidate_base_missing",
                "Select an installed built-in version",
            )
        })?;
    let platform = crate::platform::container_platform()
        .map_err(|e| Error::problem("platform_unsupported", e.to_string()))?;
    let provenance = CandidateProvenance {
        schema_version: 1,
        input_digest,
        requested_source: source.clone(),
        platform,
        source_image: proofstorm_kube::images::GIT_IMAGE.into(),
        builder_image: proofstorm_kube::images::BUILDKIT_IMAGE.into(),
        baseline_digest: digest_json(base),
        profile_digest: profile.digest(),
        profile,
    };
    let resolution = resolve(source.clone(), provenance.profile.repository.clone()).await;
    let (repository, commit_sha, failure) = match resolution {
        Ok((repository, sha)) => (Some(repository), Some(sha), None),
        Err(error) => (None, None, Some(error)),
    };
    let request_digest = digest_json(&(
        workspace,
        principal,
        &request.candidate_id,
        &request.implementation,
        &base_version,
        &repository,
        &commit_sha,
        &provenance,
    ));
    let candidate = CandidateBuild {
        diagnostics: None,
        api_version: proofstorm_core::CANDIDATE_BUILD_API_VERSION.into(),
        id: request.candidate_id.clone(),
        workspace_id: workspace.into(),
        principal_id: principal.into(),
        implementation: request.implementation.clone(),
        base_version,
        pull_request_url: if let CandidateInput::PullRequest { url } = source {
            url
        } else {
            String::new()
        },
        resource_name: format!("candidate-{}", &request_digest[7..26]),
        request_digest,
        build_features: provenance.profile.features.clone(),
        provenance: Some(provenance),
        phase: if failure.is_some() {
            CandidateBuildPhase::Failed
        } else {
            CandidateBuildPhase::Pending
        },
        accepted_at_unix: now_unix(),
        started_at_unix: None,
        completed_at_unix: failure.as_ref().map(|_| now_unix()),
        repository,
        commit_sha,
        version: Some(format!("candidate-{}", request.candidate_id)),
        image: None,
        error_code: failure
            .as_ref()
            .map(|_| "candidate_source_resolution_failed".into()),
        error_message: failure.map(|error| error.message),
    };
    store
        .create_candidate_build(workspace, principal, &candidate, &request.idempotency_key)
        .map_err(Error::from)
}

fn validate_shared_baselines(
    profile: &proofstorm_core::CandidateBuildProfile,
    base_version: &str,
) -> Result<(), Error> {
    for implementation in &profile.catalog_implementations {
        if !default_catalog().entries.iter().any(|entry| {
            entry.id == *implementation && entry.version == base_version && entry.source.is_none()
        }) {
            return Err(Error::problem(
                "candidate_base_missing",
                "The shared build requires an installed baseline for every runtime preset",
            ));
        }
    }
    Ok(())
}

fn validate_request(request: &CandidateBuildRequest) -> Result<(), Error> {
    if !(1..=63).contains(&request.candidate_id.len())
        || request.candidate_id.starts_with('-')
        || request.candidate_id.ends_with('-')
        || request.candidate_id.contains("--")
        || !request
            .candidate_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == 45)
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 256
    {
        return Err(Error::problem(
            "candidate_request_invalid",
            "Use a candidate slug and a nonempty request_id of at most 256 bytes",
        ));
    }
    Ok(())
}

struct Admission {
    profile: proofstorm_core::CandidateBuildProfile,
    source: CandidateInput,
    input_digest: String,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
