use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CatalogEntry, CatalogResponse, ReleaseChannel, SupportLifecycle, digest_json};

pub const CANDIDATE_BUILD_API_VERSION: &str = "proofstorm/candidate-build/v1alpha1";

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CandidateBuildPhase {
    #[default]
    Pending,
    Resolving,
    Building,
    Pushing,
    Succeeded,
    Failed,
    Cancelled,
}

impl CandidateBuildPhase {
    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateSource {
    pub candidate_id: String,
    pub pull_request_url: String,
    pub repository: String,
    pub commit_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateBuild {
    pub api_version: String,
    pub id: String,
    pub workspace_id: String,
    pub principal_id: String,
    pub implementation: String,
    pub base_version: String,
    pub pull_request_url: String,
    pub resource_name: String,
    pub request_digest: String,
    pub phase: CandidateBuildPhase,
    pub accepted_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl CandidateBuild {
    #[must_use]
    pub fn source(&self) -> Option<CandidateSource> {
        Some(CandidateSource {
            candidate_id: self.id.clone(),
            pull_request_url: self.pull_request_url.clone(),
            repository: self.repository.clone()?,
            commit_sha: self.commit_sha.clone()?,
        })
    }
}

/// Derive one conservative experimental catalog entry from a successful build.
///
/// Candidate code may change implementation behavior, but it cannot use the
/// build path to expand the installed adapter's declared capabilities.
///
/// # Errors
///
/// Returns an error unless the build succeeded with complete immutable source
/// and image identities matching the selected base entry.
pub fn candidate_catalog_entry(
    base: &CatalogEntry,
    candidate: &CandidateBuild,
) -> Result<CatalogEntry, String> {
    if candidate.phase != CandidateBuildPhase::Succeeded {
        return Err(format!(
            "candidate_not_succeeded: candidate {:?} is {:?}",
            candidate.id, candidate.phase
        ));
    }
    if candidate.implementation != base.id || candidate.base_version != base.version {
        return Err(format!(
            "candidate_base_mismatch: candidate {:?} targets {} {} but base is {} {}",
            candidate.id, candidate.implementation, candidate.base_version, base.id, base.version
        ));
    }
    let version = candidate
        .version
        .as_ref()
        .ok_or_else(|| "candidate_version_missing".to_owned())?;
    let image = candidate
        .image
        .as_ref()
        .ok_or_else(|| "candidate_image_missing".to_owned())?;
    let source = candidate
        .source()
        .ok_or_else(|| "candidate_source_missing".to_owned())?;
    let mut entry = base.clone();
    entry.description = format!(
        "{} candidate from {}",
        base.description, source.pull_request_url
    );
    entry.version.clone_from(version);
    entry.release_channel = ReleaseChannel::Prerelease;
    entry.support_lifecycle = SupportLifecycle::Experimental;
    entry.image.clone_from(image);
    entry.build_provenance = None;
    entry.source_digest = digest_json(&(
        base.source_digest.as_str(),
        source.candidate_id.as_str(),
        source.pull_request_url.as_str(),
        source.repository.as_str(),
        source.commit_sha.as_str(),
        image.as_str(),
    ));
    entry.source = Some(source);
    Ok(entry)
}

/// Merge successful workspace candidates into the immutable built-in catalog.
///
/// # Errors
///
/// Returns an error when a succeeded candidate has no built-in base or the
/// merged exact-version catalog violates a catalog invariant.
pub fn effective_catalog(
    built_in: &CatalogResponse,
    candidates: &[CandidateBuild],
) -> Result<CatalogResponse, String> {
    let mut entries = built_in.entries.clone();
    for candidate in candidates {
        if candidate.phase != CandidateBuildPhase::Succeeded {
            continue;
        }
        let base = built_in
            .entries
            .iter()
            .find(|entry| {
                entry.id == candidate.implementation && entry.version == candidate.base_version
            })
            .ok_or_else(|| {
                format!(
                    "candidate_base_missing: candidate {:?} targets unavailable {} {}",
                    candidate.id, candidate.implementation, candidate.base_version
                )
            })?;
        let candidate_entry = candidate_catalog_entry(base, candidate)?;
        for entry in &mut entries {
            for dependency in &mut entry.compatible_dependencies {
                if dependency.implementation == candidate.implementation
                    && dependency.versions.contains(&candidate.base_version)
                {
                    dependency.versions.insert(candidate_entry.version.clone());
                }
            }
            entry.support_matrix.payment_bindings = entry
                .support_matrix
                .payment_bindings
                .iter()
                .cloned()
                .map(|mut binding| {
                    if binding.backend.implementation == candidate.implementation
                        && binding.backend.versions.contains(&candidate.base_version)
                    {
                        binding
                            .backend
                            .versions
                            .insert(candidate_entry.version.clone());
                    }
                    binding
                })
                .collect();
            for wallet in &mut entry.support_matrix.compatible_wallet_adapters {
                if wallet.implementation == candidate.implementation
                    && wallet.versions.contains(&candidate.base_version)
                {
                    wallet.versions.insert(candidate_entry.version.clone());
                }
            }
        }
        entries.push(candidate_entry);
    }
    CatalogResponse::try_new(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_catalog;

    fn succeeded_candidate() -> CandidateBuild {
        CandidateBuild {
            api_version: CANDIDATE_BUILD_API_VERSION.into(),
            id: "nutshell-pr-1095".into(),
            workspace_id: "test".into(),
            principal_id: "agent".into(),
            implementation: "nutshell".into(),
            base_version: "0.20.3".into(),
            pull_request_url: "https://github.com/cashubtc/nutshell/pull/1095".into(),
            resource_name: "candidate-aabbccdd".into(),
            request_digest: "sha256:request".into(),
            phase: CandidateBuildPhase::Succeeded,
            accepted_at_unix: 1,
            started_at_unix: Some(2),
            completed_at_unix: Some(3),
            repository: Some("https://github.com/cashubtc/nutshell.git".into()),
            commit_sha: Some("aabbccddaabbccddaabbccddaabbccddaabbccdd".into()),
            version: Some("candidate-pr1095-aabbccdd".into()),
            image: Some(format!(
                "proofstorm-registry.localhost:5000/proofstorm-candidates/nutshell@sha256:{}",
                "1".repeat(64)
            )),
            error_code: None,
            error_message: None,
        }
    }

    #[test]
    fn successful_candidate_is_an_exact_non_preferred_catalog_version() {
        let candidate = succeeded_candidate();
        let catalog = effective_catalog(default_catalog(), &[candidate.clone()])
            .expect("merge candidate catalog");
        let support = catalog
            .implementations
            .iter()
            .find(|support| support.implementation == "nutshell")
            .expect("Nutshell support");
        assert_eq!(support.preferred_version, "0.20.3");
        assert!(
            support
                .supported_versions
                .contains("candidate-pr1095-aabbccdd")
        );
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.version == "candidate-pr1095-aabbccdd")
            .expect("candidate entry");
        assert_eq!(entry.support_lifecycle, SupportLifecycle::Experimental);
        assert_eq!(entry.source, candidate.source());
        assert_eq!(entry.image, candidate.image.unwrap_or_default());
    }

    #[test]
    fn candidate_backend_substitutes_for_its_compatible_base_version() {
        let mut candidate = succeeded_candidate();
        candidate.id = "lnd-pr-999".into();
        candidate.implementation = "lnd".into();
        candidate.base_version = "0.21.0-beta".into();
        candidate.version = Some("candidate-pr999-aabbccdd".into());
        candidate.pull_request_url = "https://github.com/lightningnetwork/lnd/pull/999".into();
        candidate.repository = Some("https://github.com/lightningnetwork/lnd.git".into());
        let catalog =
            effective_catalog(default_catalog(), &[candidate]).expect("merge Lightning candidate");
        let nutshell = catalog
            .entries
            .iter()
            .find(|entry| entry.id == "nutshell")
            .expect("Nutshell catalog entry");
        assert!(
            nutshell
                .support_matrix
                .payment_bindings
                .iter()
                .any(|binding| {
                    binding.backend.implementation == "lnd"
                        && binding
                            .backend
                            .versions
                            .contains("candidate-pr999-aabbccdd")
                })
        );
    }
}
