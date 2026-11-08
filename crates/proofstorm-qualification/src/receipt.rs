use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{Component, Identity, Plan};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageEvidence {
    pub manifest: String,
    pub config: String,
}

/// Public summary only. Raw native output, credentials and private runtime
/// state must remain outside uploaded qualification evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub format_version: u32,
    pub identity: Identity,
    pub plan_digest: String,
    pub case_id: String,
    pub platform: String,
    pub components: Vec<Component>,
    pub claims: BTreeSet<String>,
    pub images: BTreeMap<String, ImageEvidence>,
    pub passed: bool,
    pub cleanup_verified: bool,
    pub preservation_verified: bool,
    pub stage: String,
    pub elapsed_seconds: u64,
}

/// Require exactly one successful receipt for every scheduled case. Artifacts
/// from stale attempts, unplanned cases, and duplicate results are refused.
///
/// # Errors
/// Rejects incomplete, failed, duplicate, stale or mismatched evidence.
pub fn verify_receipts(plan: &Plan, receipts: &[Receipt]) -> Result<()> {
    plan.validate()?;
    let expected: BTreeSet<_> = plan
        .cases
        .iter()
        .filter(|case| case.required)
        .map(|case| case.id.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    let digest = plan.digest();
    for receipt in receipts {
        check(plan, &digest, receipt)?;
        ensure!(
            seen.insert(receipt.case_id.as_str()),
            "duplicate qualification receipt"
        );
    }
    let missing: Vec<_> = expected.difference(&seen).collect();
    ensure!(
        missing.is_empty(),
        "missing required qualification receipts: {missing:?}"
    );
    Ok(())
}

/// One receipt's evidence for its exact case in an already validated plan.
/// The shard runner uses this to keep passes from an earlier attempt.
pub(crate) fn check(plan: &Plan, digest: &str, receipt: &Receipt) -> Result<()> {
    let case = plan.case(&receipt.case_id)?;
    ensure!(
        case.required,
        "receipt for an unscheduled qualification case"
    );
    ensure!(
        receipt.format_version == 1
            && receipt.identity == plan.identity
            && receipt.plan_digest == digest,
        "stale or foreign qualification receipt"
    );
    ensure!(
        receipt.platform == case.platform
            && receipt.components == case.components
            && receipt.claims == case.claims,
        "qualification receipt does not match its exact case"
    );
    ensure!(
        receipt.passed
            && receipt.cleanup_verified
            && receipt.preservation_verified
            && receipt.stage == "complete",
        "failed qualification or unverified cleanup/preservation: {}",
        receipt.case_id
    );
    let sources: BTreeSet<_> = case
        .components
        .iter()
        .map(|component| component.source.as_str())
        .collect();
    ensure!(
        sources == receipt.images.keys().map(String::as_str).collect(),
        "image evidence is incomplete"
    );
    for image in receipt.images.values() {
        for digest in [&image.manifest, &image.config] {
            ensure!(
                digest.strip_prefix("sha256:").is_some_and(
                    |hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                ),
                "invalid observed image identity"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Scenario, plan};

    fn fixture() -> (Plan, Vec<Receipt>) {
        let plan = plan(
            Identity {
                revision: "a".repeat(40),
                run_id: "123".into(),
                attempt: 1,
            },
            crate::Mode::Compatibility,
        )
        .unwrap();
        let receipts = plan
            .cases
            .iter()
            .filter(|case| case.required)
            .map(|case| Receipt {
                format_version: 1,
                identity: plan.identity.clone(),
                plan_digest: plan.digest(),
                case_id: case.id.clone(),
                platform: case.platform.clone(),
                components: case.components.clone(),
                claims: case.claims.clone(),
                images: case
                    .components
                    .iter()
                    .map(|component| {
                        (
                            component.source.clone(),
                            ImageEvidence {
                                manifest: format!("sha256:{}", "a".repeat(64)),
                                config: format!("sha256:{}", "b".repeat(64)),
                            },
                        )
                    })
                    .collect(),
                passed: true,
                cleanup_verified: true,
                preservation_verified: true,
                stage: "complete".into(),
                elapsed_seconds: 1,
            })
            .collect();
        (plan, receipts)
    }

    #[test]
    fn all_scenarios_require_current_complete_successful_evidence() {
        let (plan, receipts) = fixture();
        verify_receipts(&plan, &receipts).unwrap();
        for mutate in [
            |receipt: &mut Receipt| receipt.identity.attempt += 1,
            |receipt: &mut Receipt| receipt.identity.revision = "b".repeat(40),
            |receipt: &mut Receipt| receipt.plan_digest = "sha256:foreign".into(),
            |receipt: &mut Receipt| receipt.platform = "linux/other".into(),
            |receipt: &mut Receipt| {
                receipt.claims.insert("undeclared-coverage".into());
            },
            |receipt: &mut Receipt| receipt.passed = false,
            |receipt: &mut Receipt| receipt.cleanup_verified = false,
            |receipt: &mut Receipt| receipt.preservation_verified = false,
            |receipt: &mut Receipt| receipt.images.clear(),
            |receipt: &mut Receipt| receipt.stage = "setup".into(),
        ] {
            let mut changed = receipts.clone();
            mutate(&mut changed[0]);
            assert!(verify_receipts(&plan, &changed).is_err());
        }
        assert!(verify_receipts(&plan, &receipts[1..]).is_err());
        let mut duplicate = receipts.clone();
        duplicate.push(receipts[0].clone());
        assert!(verify_receipts(&plan, &duplicate).is_err());
    }

    #[test]
    fn plan_rejects_missing_cases_and_a_different_shipped_image() {
        let (mut plan, _) = fixture();
        plan.cases.pop();
        assert!(plan.validate().is_err());
        let (mut plan, _) = fixture();
        let cln = plan.cases.iter_mut().find(|case| matches!(&case.scenario, Scenario::Image { component } if component.implementation == "cln")).unwrap();
        cln.components[0].image = format!("example.invalid/cln@sha256:{}", "b".repeat(64));
        assert!(plan.validate().is_err());
    }
}
