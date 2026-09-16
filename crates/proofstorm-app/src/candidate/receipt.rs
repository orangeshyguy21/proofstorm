use proofstorm_core::{CandidateBuild, CandidateBuildPhase};
use proofstorm_view::{CandidateBuildReceipt, CandidateCatalogSelector};
#[must_use]
pub fn receipt(candidate: &CandidateBuild, timed_out: bool) -> CandidateBuildReceipt {
    let next_tool = match candidate.phase {
        CandidateBuildPhase::Succeeded => "cell_plan",
        CandidateBuildPhase::Failed | CandidateBuildPhase::Cancelled => "none",
        CandidateBuildPhase::Pending
        | CandidateBuildPhase::Resolving
        | CandidateBuildPhase::Building
        | CandidateBuildPhase::Pushing => "candidate_wait",
    };
    let version = candidate.version.clone().unwrap_or_default();
    CandidateBuildReceipt {
        record_resource_uri: format!("proofstorm://candidate-build/{}/record", candidate.id),
        source_resolved: candidate.commit_sha.is_some(),
        compatibility_basis: "inherited_unverified".into(),
        candidate_id: candidate.id.clone(),
        base_version: candidate.base_version.clone(),
        catalog_entry: CandidateCatalogSelector {
            implementation: candidate.implementation.clone(),
            version: version.clone(),
        },
        catalog_entries: candidate
            .catalog_implementations()
            .into_iter()
            .map(|implementation| CandidateCatalogSelector {
                implementation: implementation.into(),
                version: version.clone(),
            })
            .collect(),
        commit_sha: candidate.commit_sha.clone().unwrap_or_default(),
        phase: candidate.phase,
        image: candidate.image.clone(),
        error_code: candidate.error_code.clone(),
        message: candidate.error_message.clone(),
        logs_resource_uri: format!("proofstorm://candidate-build/{}/logs", candidate.id),
        build_profile_notes: candidate.provenance.as_ref().map_or_else(
            || vec!["Legacy build: exact recipe evidence was not recorded.".into()],
            |p| p.profile.notes.clone(),
        ),
        timed_out,
        next_tool: next_tool.into(),
    }
}
