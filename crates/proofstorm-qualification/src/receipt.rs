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
        let case = plan.case(&receipt.case_id)?;
        ensure!(
            case.required,
            "receipt for an unscheduled qualification case"
        );
        ensure!(
            seen.insert(receipt.case_id.as_str()),
            "duplicate qualification receipt"
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
    }
    let missing: Vec<_> = expected.difference(&seen).collect();
    ensure!(
        missing.is_empty(),
        "missing required qualification receipts: {missing:?}"
    );
    Ok(())
}

/// A stable merge check must explicitly require success from its dependencies.
/// Unknown jobs are refused so renaming a lane cannot silently remove coverage.
///
/// # Errors
/// Rejects any missing, unexpected or unsuccessful dependency.
pub fn aggregate(needs: &serde_json::Value) -> Result<()> {
    let jobs = needs
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("missing CI dependency results"))?;
    let expected = BTreeSet::from(["quick", "rust", "macos-installer-contract", "qualification"]);
    ensure!(
        jobs.keys().map(String::as_str).collect::<BTreeSet<_>>() == expected,
        "merge check dependency set changed"
    );
    let statuses: BTreeMap<_, _> = jobs
        .iter()
        .map(|(name, value)| (name, value["result"].as_str()))
        .collect();
    ensure!(
        statuses.values().all(|status| *status == Some("success")),
        "merge qualification requires every dependency to succeed: {statuses:?}"
    );
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

    #[test]
    fn failed_cancelled_skipped_missing_and_unknown_jobs_block_merges() {
        let names = ["quick", "rust", "macos-installer-contract", "qualification"];
        let good: serde_json::Value = names
            .into_iter()
            .map(|name| (name.to_owned(), serde_json::json!({"result":"success"})))
            .collect::<serde_json::Map<_, _>>()
            .into();
        aggregate(&good).unwrap();
        for name in names {
            for status in ["failure", "cancelled", "skipped", "neutral", ""] {
                let mut changed = good.clone();
                changed[name]["result"] = status.into();
                assert!(aggregate(&changed).is_err());
            }
            let mut changed = good.clone();
            changed.as_object_mut().unwrap().remove(name);
            assert!(aggregate(&changed).is_err());
        }
        let mut changed = good;
        changed["unknown"] = serde_json::json!({"result":"success"});
        assert!(aggregate(&changed).is_err());
    }
}
