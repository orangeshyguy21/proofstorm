use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::{CatalogEntry, CatalogResponse, ReleaseChannel, SupportLifecycle, digest_json};

pub const CANDIDATE_BUILD_API_VERSION: &str = "proofstorm/candidate-build/v1alpha1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateInput {
    PullRequest {
        url: String,
    },
    Commit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Tag {
        tag: String,
    },
}

impl CandidateInput {
    /// Normalize author input before checking immutable request identity or resolving GitHub.
    ///
    /// # Errors
    /// Rejects ambiguous commit inputs, abbreviated SHAs, invalid tags and URLs outside the canonical repository.
    pub fn normalized(&self, repository: &str) -> Result<Self, String> {
        let prefix = format!("https://github.com/{repository}/");
        match self {
            Self::PullRequest { url } => {
                let path = url.trim_end_matches('/');
                let number = path.strip_prefix(&format!("{prefix}pull/")).and_then(|n| n.parse::<u64>().ok()).filter(|n| *n > 0).ok_or("candidate_pr_url_invalid: expected a PR in the component's canonical repository")?;
                Ok(Self::PullRequest {
                    url: format!("{prefix}pull/{number}"),
                })
            }
            Self::Commit { sha, url } => {
                let value =
                    match (sha, url) {
                        (Some(sha), None) => sha.as_str(),
                        (None, Some(url)) => url
                            .strip_prefix(&format!("{prefix}commit/"))
                            .ok_or("candidate_repository_mismatch")?,
                        _ => return Err(
                            "candidate_source_invalid: commit requires exactly one of sha or url"
                                .into(),
                        ),
                    };
                if value.len() != 40 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(
                        "candidate_sha_invalid: full 40-character commit SHA required".into(),
                    );
                }
                Ok(Self::Commit {
                    sha: Some(value.to_ascii_lowercase()),
                    url: None,
                })
            }
            Self::Tag { tag } => {
                if tag.is_empty()
                    || tag.len() > 256
                    || tag.starts_with('-')
                    || tag.contains("..")
                    || tag.chars().any(|c| c.is_control() || c.is_whitespace())
                {
                    return Err("candidate_tag_invalid".into());
                }
                Ok(self.clone())
            }
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::PullRequest { url } => url,
            Self::Commit { sha, url } => sha.as_deref().or(url.as_deref()).unwrap_or_default(),
            Self::Tag { tag } => tag,
        }
    }
}

impl CandidateProvenance {
    /// Validate versioned evidence without replacing its saved recipe with today's recipe.
    ///
    /// # Errors
    /// Rejects unknown evidence versions, altered recipe digests, unsupported platforms and invalid resource bounds.
    pub fn validate(&self) -> Result<(), String> {
        let shared = &self.profile.catalog_implementations;
        if self.schema_version != 1
            || (!shared.is_empty()
                && (self.profile.id != "cdk-mint-source"
                    || self.profile.version < 5
                    || *shared
                        != crate::candidate_profiles::CDK_MINT_IMPLEMENTATIONS
                            .map(str::to_owned)
                            .into()))
            || self.profile.version == 0
            || self.profile_digest != self.profile.digest()
            || !matches!(self.platform.as_str(), "linux/amd64" | "linux/arm64")
            || !self.profile.platforms.contains(&self.platform)
            || !(1..=crate::CANDIDATE_BUILD_MAX_CPU_MILLICORES)
                .contains(&self.profile.cpu_limit_millicores)
            || !(1..=4096).contains(&self.profile.memory_limit_mib)
            || !(1..=crate::CANDIDATE_BUILD_MAX_DEADLINE_SECONDS)
                .contains(&self.profile.deadline_seconds)
            || [&self.source_image, &self.builder_image]
                .iter()
                .any(|image| {
                    image.rsplit_once("@sha256:").is_none_or(|(_, sha)| {
                        sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                })
        {
            return Err(
                "candidate_provenance_invalid: inconsistent or unsupported build evidence".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateProvenance {
    pub schema_version: u32,
    pub input_digest: String,
    // Kubernetes needs one structural shape for tagged source inputs. Admission
    // still decodes CandidateInput and validates its exclusive fields.
    #[schemars(schema_with = "candidate_input_schema")]
    pub requested_source: CandidateInput,
    pub platform: String,
    pub source_image: String,
    pub builder_image: String,
    pub baseline_digest: String,
    pub profile: crate::CandidateBuildProfile,
    pub profile_digest: String,
}

fn candidate_input_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":"object", "additionalProperties":false, "required":["type"],
        "properties": {
            "type":{"type":"string","enum":["pull_request","commit","tag"]},
            "url":{"type":"string"},"sha":{"type":"string"},"tag":{"type":"string"}
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateDiagnostics {
    pub captured_at_unix: i64,
    pub logs: std::collections::BTreeMap<String, CandidateLog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateLog {
    pub text: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<CandidateProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidateBuild {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<CandidateDiagnostics>,
    pub api_version: String,
    pub id: String,
    pub workspace_id: String,
    pub principal_id: String,
    pub implementation: String,
    pub base_version: String,
    pub pull_request_url: String,
    pub resource_name: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<CandidateProvenance>,
    /// Packaging guarantees recorded at admission; old builds inherit none.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub build_features: BTreeSet<crate::CatalogFeature>,
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
    /// Exact runtime presets declared by the saved recipe, never today's profile.
    #[must_use]
    pub fn catalog_implementations(&self) -> BTreeSet<&str> {
        self.provenance.as_ref().map_or_else(
            || BTreeSet::from([self.implementation.as_str()]),
            |p| {
                if p.profile.catalog_implementations.is_empty() {
                    BTreeSet::from([self.implementation.as_str()])
                } else {
                    p.profile
                        .catalog_implementations
                        .iter()
                        .map(String::as_str)
                        .collect()
                }
            },
        )
    }

    #[must_use]
    pub fn source(&self) -> Option<CandidateSource> {
        Some(CandidateSource {
            candidate_id: self.id.clone(),
            pull_request_url: self.pull_request_url.clone(),
            repository: self.repository.clone()?,
            commit_sha: self.commit_sha.clone()?,
            provenance: self.provenance.clone(),
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
    if candidate.api_version != CANDIDATE_BUILD_API_VERSION {
        return Err("candidate_record_version_unsupported".into());
    }
    if let Some(provenance) = &candidate.provenance {
        provenance.validate()?;
    }
    if candidate.phase != CandidateBuildPhase::Succeeded {
        return Err(format!(
            "candidate_not_succeeded: candidate {:?} is {:?}",
            candidate.id, candidate.phase
        ));
    }
    let implementations = candidate.catalog_implementations();
    if !implementations.contains(candidate.implementation.as_str())
        || !implementations.contains(base.id.as_str())
        || candidate.base_version != base.version
    {
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
        base.description,
        source
            .provenance
            .as_ref()
            .map_or(source.pull_request_url.as_str(), |p| p
                .requested_source
                .label())
    );
    entry.version.clone_from(version);
    entry.release_channel = ReleaseChannel::Prerelease;
    entry.support_lifecycle = SupportLifecycle::Experimental;
    entry.image.clone_from(image);
    entry.build_provenance = None;
    for feature in [
        crate::CatalogFeature::MintManagementRpc,
        crate::CatalogFeature::NativeCliEntrypoints,
    ] {
        if !candidate.build_features.contains(&feature) {
            entry.features.remove(&feature);
        }
    }
    entry.source_digest = digest_json(&(
        base.source_digest.as_str(),
        source.candidate_id.as_str(),
        source.pull_request_url.as_str(),
        source.repository.as_str(),
        source.commit_sha.as_str(),
        image.as_str(),
    ));
    if let Some(provenance) = &source.provenance {
        entry.source_digest = digest_json(&(&entry.source_digest, provenance));
    }
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
    let mut successful = candidates
        .iter()
        .filter(|c| c.phase == CandidateBuildPhase::Succeeded)
        .collect::<Vec<_>>();
    successful.sort_by(|a, b| {
        a.implementation
            .cmp(&b.implementation)
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.id.cmp(&b.id))
    });
    for candidate in &successful {
        for implementation in candidate.catalog_implementations() {
            let base = built_in
                .entries
                .iter()
                .find(|entry| entry.id == implementation && entry.version == candidate.base_version)
                .ok_or_else(|| {
                    format!(
                        "candidate_base_missing: candidate {:?} targets unavailable {} {}",
                        candidate.id, implementation, candidate.base_version
                    )
                })?;
            let candidate_entry = candidate_catalog_entry(base, candidate)?;
            entries.push(candidate_entry);
        }
    }
    // Expand all contracts only after every successful candidate has been added.
    // This makes wallet/mint candidate compatibility independent of build order.
    for candidate in successful {
        let implementations = candidate.catalog_implementations();
        let candidate_version = candidate
            .version
            .as_ref()
            .ok_or("candidate_version_missing")?;
        for entry in &mut entries {
            for dependency in &mut entry.compatible_dependencies {
                if implementations.contains(dependency.implementation.as_str())
                    && dependency.versions.contains(&candidate.base_version)
                {
                    dependency.versions.insert(candidate_version.clone());
                }
            }
            entry.support_matrix.payment_bindings = entry
                .support_matrix
                .payment_bindings
                .iter()
                .cloned()
                .map(|mut binding| {
                    if implementations.contains(binding.backend.implementation.as_str())
                        && binding.backend.versions.contains(&candidate.base_version)
                    {
                        binding.backend.versions.insert(candidate_version.clone());
                    }
                    binding
                })
                .collect();
            for wallet in &mut entry.support_matrix.compatible_wallet_adapters {
                if implementations.contains(wallet.implementation.as_str())
                    && wallet.versions.contains(&candidate.base_version)
                {
                    wallet.versions.insert(candidate_version.clone());
                }
            }
        }
    }
    CatalogResponse::try_new(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_catalog;

    #[test]
    fn candidate_source_forms_are_exact_and_canonical() {
        let sha = "a".repeat(40);
        let raw = CandidateInput::Commit {
            sha: Some(sha.clone()),
            url: None,
        };
        let url = CandidateInput::Commit {
            sha: None,
            url: Some(format!("https://github.com/cashubtc/cdk/commit/{sha}")),
        };
        assert_eq!(
            raw.normalized("cashubtc/cdk").unwrap(),
            url.normalized("cashubtc/cdk").unwrap()
        );
        assert!(url.normalized("cashubtc/nutshell").is_err());
        assert!(
            CandidateInput::Commit {
                sha: Some("abc123".into()),
                url: None
            }
            .normalized("cashubtc/cdk")
            .is_err()
        );
        assert!(
            CandidateInput::Commit {
                sha: Some(sha),
                url: Some("https://github.com/cashubtc/cdk/commit/main".into())
            }
            .normalized("cashubtc/cdk")
            .is_err()
        );
        assert!(
            CandidateInput::PullRequest {
                url: "https://github.com/cashubtc/cdk/pull/0".into()
            }
            .normalized("cashubtc/cdk")
            .is_err()
        );
        assert!(
            CandidateInput::Tag {
                tag: "v0.18.1".into()
            }
            .normalized("cashubtc/cdk")
            .is_ok()
        );
        assert!(
            CandidateInput::Tag {
                tag: "../main".into()
            }
            .normalized("cashubtc/cdk")
            .is_err()
        );
    }

    #[test]
    fn candidate_merge_order_and_legacy_source_digest_are_stable() {
        let mint = succeeded_candidate();
        let mut wallet = mint.clone();
        wallet.id = "wallet".into();
        wallet.implementation = "nutshell-wallet".into();
        wallet.version = Some("candidate-wallet".into());
        let a = effective_catalog(default_catalog(), &[mint.clone(), wallet.clone()]).unwrap();
        let b = effective_catalog(default_catalog(), &[wallet, mint.clone()]).unwrap();
        assert_eq!(digest_json(&a), digest_json(&b));
        let base = default_catalog()
            .entries
            .iter()
            .find(|e| e.id == "nutshell")
            .unwrap();
        let entry = candidate_catalog_entry(base, &mint).unwrap();
        let source = entry.source.as_ref().unwrap();
        assert_eq!(
            entry.source_digest,
            digest_json(&(
                &base.source_digest,
                &source.candidate_id,
                &source.pull_request_url,
                &source.repository,
                &source.commit_sha,
                mint.image.as_ref().unwrap()
            ))
        );
        let record = serde_json::to_value(&mint).unwrap();
        assert!(record.get("provenance").is_none());
        assert!(record.get("diagnostics").is_none());
        assert!(
            serde_json::to_value(source)
                .unwrap()
                .get("provenance")
                .is_none()
        );
    }

    fn succeeded_candidate() -> CandidateBuild {
        CandidateBuild {
            diagnostics: None,
            api_version: CANDIDATE_BUILD_API_VERSION.into(),
            id: "nutshell-pr-1095".into(),
            workspace_id: "test".into(),
            principal_id: "agent".into(),
            implementation: "nutshell".into(),
            base_version: "0.21.0".into(),
            pull_request_url: "https://github.com/cashubtc/nutshell/pull/1095".into(),
            resource_name: "candidate-aabbccdd".into(),
            request_digest: "sha256:request".into(),
            provenance: None,
            build_features: BTreeSet::from([
                crate::CatalogFeature::MintManagementRpc,
                crate::CatalogFeature::NativeCliEntrypoints,
            ]),
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
    fn old_candidates_cannot_inherit_new_management_packaging_guarantees() {
        let candidate = succeeded_candidate();
        let mut old_json = serde_json::to_value(&candidate).unwrap();
        old_json.as_object_mut().unwrap().remove("build_features");
        let old: CandidateBuild = serde_json::from_value(old_json).unwrap();
        let base = default_catalog()
            .entries
            .iter()
            .find(|e| e.id == "nutshell")
            .unwrap();
        assert!(
            !candidate_catalog_entry(base, &old)
                .unwrap()
                .features
                .contains(&crate::CatalogFeature::MintManagementRpc)
        );
        assert!(
            candidate_catalog_entry(base, &candidate)
                .unwrap()
                .features
                .contains(&crate::CatalogFeature::MintManagementRpc)
        );
        let mut without_entrypoints = candidate.clone();
        without_entrypoints
            .build_features
            .remove(&crate::CatalogFeature::NativeCliEntrypoints);
        let old = candidate_catalog_entry(base, &without_entrypoints).unwrap();
        assert!(
            old.features
                .contains(&crate::CatalogFeature::MintManagementRpc)
        );
        assert!(
            !old.features
                .contains(&crate::CatalogFeature::NativeCliEntrypoints)
        );
        assert!(
            candidate_catalog_entry(base, &candidate)
                .unwrap()
                .features
                .contains(&crate::CatalogFeature::NativeCliEntrypoints)
        );
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
        assert_eq!(support.preferred_version.as_deref(), Some("0.21.0"));
        assert!(
            !support
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
        candidate.base_version = "0.21.3-beta".into();
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
