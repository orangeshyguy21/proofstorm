//! Immutable inputs shared by preparation and every qualification consumer.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use proofstorm_core::{SupportLifecycle, catalog_image_source};
use serde::{Deserialize, Serialize};

use crate::{Plan, catalog};

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInputs {
    pub format_version: u32,
    pub plan_digest: String,
    pub images: BTreeMap<String, BTreeSet<String>>,
}

/// Include all scheduled components and the default workspace/probe image.
///
/// # Errors
/// Rejects an invalid plan or an incomplete catalog.
pub fn image_inputs(plan: &Plan) -> Result<ImageInputs> {
    plan.validate()?;
    let mut images: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for case in plan.cases.iter().filter(|case| case.required) {
        for component in &case.components {
            images
                .entry(component.source.clone())
                .or_default()
                .insert(case.platform.clone());
        }
    }
    for platform in ["linux/amd64", "linux/arm64"] {
        let catalog = catalog(platform)?;
        let workspace = catalog
            .entries
            .iter()
            .find(|entry| {
                entry.id == "workspace" && entry.support_lifecycle == SupportLifecycle::Preferred
            })
            .context("missing default workspace/probe image")?;
        images
            .entry(catalog_image_source(&workspace.image).map_err(anyhow::Error::msg)?)
            .or_default()
            .insert(platform.into());
    }
    Ok(ImageInputs {
        format_version: 1,
        plan_digest: plan.digest(),
        images,
    })
}

/// Validate the explicit, job-local cache address. No remote registry overrides.
///
/// # Errors
/// Rejects URLs, credentials, paths, wildcard addresses and zero ports.
pub fn validate_cache_endpoint(endpoint: &str) -> Result<()> {
    ensure!(
        endpoint.strip_prefix("127.0.0.1:").is_some_and(|port| {
            port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port > 0)
        }),
        "qualification image cache must be an explicit 127.0.0.1 port"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, Mode, plan};

    #[test]
    fn inputs_cover_scheduled_cases_and_bind_the_exact_attempt() {
        for mode in [Mode::Pull, Mode::Compatibility, Mode::Full] {
            let mut plan = plan(
                Identity {
                    revision: "a".repeat(40),
                    run_id: "1".into(),
                    attempt: 1,
                },
                mode,
            )
            .unwrap();
            let inputs = image_inputs(&plan).unwrap();
            for case in plan.cases.iter().filter(|case| case.required) {
                for component in &case.components {
                    assert!(inputs.images[&component.source].contains(&case.platform));
                }
            }
            assert!(
                inputs
                    .images
                    .keys()
                    .any(|source| source.contains("/busybox@sha256:"))
            );
            plan.identity.attempt += 1;
            assert_ne!(inputs, image_inputs(&plan).unwrap());
            plan.cases[0].components[0].source = "unplanned".into();
            assert!(image_inputs(&plan).is_err());
        }
    }

    #[test]
    fn published_bark_images_and_managed_gate_have_exact_native_obligations() {
        for mode in [
            Mode::Pull,
            Mode::Documentation,
            Mode::Compatibility,
            Mode::Full,
        ] {
            let plan = plan(
                Identity {
                    revision: "a".repeat(40),
                    run_id: "1".into(),
                    attempt: 1,
                },
                mode,
            )
            .unwrap();
            let inputs = image_inputs(&plan).unwrap();
            for platform in ["linux/arm64", "linux/amd64"] {
                let claims = &plan.obligations[platform];
                let gates: Vec<_> = plan.cases.iter().filter(|case| case.platform == platform
                    && matches!(&case.scenario, crate::Scenario::Gate { name, .. } if name == "bark-processor")).collect();
                assert_eq!(gates.len(), 1);
                let gate = gates[0];
                assert_eq!(
                    gate.required,
                    matches!(mode, Mode::Compatibility | Mode::Full)
                );
                assert_eq!(gate.components.len(), 8);
                // The gate checks registration of every rail and pays bolt11 and onchain.
                for method in ["bolt11", "onchain", "arkoor"] {
                    let claim = format!(
                        "cdk@0.18.1:payment:[\"{method}\",\"sat\",\"cdk-bark-processor\",\"0.1.0-fe468ca\"]"
                    );
                    assert!(gate.claims.contains(&claim), "{claim}");
                }
                for id in ["cdk-bark-processor", "bark-server", "cln-hold"] {
                    let component = gate
                        .components
                        .iter()
                        .find(|component| component.implementation == id)
                        .unwrap();
                    let image_cases: Vec<_> = plan.cases.iter().filter(|case| case.platform == platform
                        && matches!(&case.scenario, crate::Scenario::Image { component } if component.implementation == id)).collect();
                    assert_eq!(image_cases.len(), 1);
                    assert_eq!(image_cases[0].required, mode != Mode::Pull);
                    if mode == Mode::Pull {
                        assert!(!inputs.images.contains_key(&component.source));
                    } else {
                        assert_eq!(
                            inputs.images[&component.source],
                            [platform.to_owned()].into()
                        );
                    }
                    let prefix = format!("{id}@");
                    let obligations: BTreeSet<_> = claims
                        .iter()
                        .filter(|claim| claim.starts_with(&prefix))
                        .cloned()
                        .collect();
                    assert!(!obligations.is_empty());
                    let covered: BTreeSet<_> = gate
                        .claims
                        .union(&image_cases[0].claims)
                        .filter(|claim| claim.starts_with(&prefix))
                        .cloned()
                        .collect();
                    assert_eq!(obligations, covered);
                }
            }
        }
    }

    #[test]
    fn cache_addresses_cannot_redirect_reads_or_writes_off_host() {
        for endpoint in [
            "127.0.0.1:0",
            "localhost:5000",
            "0.0.0.0:5000",
            "https://127.0.0.1:5000",
            "127.0.0.1:5000/path",
            "127.0.0.1:65536",
            "127.0.0.1:+5",
        ] {
            assert!(validate_cache_endpoint(endpoint).is_err());
        }
        validate_cache_endpoint("127.0.0.1:5000").unwrap();
    }
}
