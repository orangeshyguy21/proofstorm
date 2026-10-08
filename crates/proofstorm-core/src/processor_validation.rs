//! Authored payment bindings must match the selected gRPC processor's full rail
//! set. Backend dependency topology is validated separately from mint bindings.
use super::{CellSpec, ComponentKind, LinkKind, ValidationIssue, issue};
use crate::{ProcessorProfile, method_list, processor_ids::LDK_PROCESSOR};

pub(super) fn validate_topology(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in cell.components.iter().enumerate() {
        let links = cell
            .links
            .iter()
            .filter(|link| link.from == component.id && link.kind == LinkKind::PaymentBackend)
            .collect::<Vec<_>>();
        let (profile, target_implementation, target_kind, advertised) = if component.implementation
            == LDK_PROCESSOR
        {
            let profile = ProcessorProfile::LdkServer;
            (
                profile,
                "ldk-server",
                ComponentKind::Lightning,
                profile.advertised_methods(&component.config),
            )
        } else if component.implementation == "cdk" {
            let Some(target) = links.iter().find_map(|link| {
                cell.components.iter().find(|target| {
                    target.id == link.to
                        && (target.kind == ComponentKind::PaymentProcessor
                            || ProcessorProfile::for_implementation(&target.implementation)
                                .is_some())
                })
            }) else {
                continue;
            };
            let Some(profile) = ProcessorProfile::for_implementation(&target.implementation) else {
                issue(
                    issues,
                    "unsupported_processor_profile",
                    format!("/components/{index}"),
                    format!(
                        "unsupported payment processor implementation {:?}",
                        target.implementation
                    ),
                );
                continue;
            };
            (
                profile,
                profile.implementation(),
                ComponentKind::PaymentProcessor,
                profile.advertised_methods(&target.config),
            )
        } else {
            continue;
        };
        // Configuration validation reports an invalid advertised selection.
        let Some(advertised) = advertised else {
            continue;
        };
        let target_id = links.first().map(|link| &link.to);
        // CDK registers every advertised method, so bindings must name exactly that set.
        if profile
            .bound_methods(links.iter().map(|link| link.binding.as_ref()))
            .is_none_or(|(_, bound)| bound != advertised)
            || !links.iter().all(|link| {
                cell.components.iter().any(|target| {
                    Some(&link.to) == target_id
                        && target.id == link.to
                        && target.kind == target_kind
                        && target.implementation == target_implementation
                })
            })
        {
            issue(
                issues,
                match profile {
                    ProcessorProfile::LdkServer => "ldk_processor_payment_bindings",
                    ProcessorProfile::Bark => "bark_processor_payment_bindings",
                },
                format!("/components/{index}"),
                format!(
                    "this profile requires {} payment bindings to one {target_implementation} component; it advertises {}",
                    profile.binding_description(),
                    method_list(&advertised)
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DependencyBinding, PaymentMethod, validate_cell};

    fn example() -> CellSpec {
        serde_json::from_str(include_str!("../../../examples/ldk-server-cell.json")).unwrap()
    }

    #[test]
    fn processor_topology_keeps_methods_units_and_endpoints_unambiguous() {
        let cell = example();
        assert!(validate_cell(&cell).valid);
        for mutation in [
            "missing-method",
            "duplicate-method",
            "wrong-unit",
            "second-endpoint",
            "wrong-service",
        ] {
            let mut invalid = cell.clone();
            let index = invalid
                .links
                .iter()
                .position(|link| link.id == "mint-bolt12")
                .unwrap();
            match mutation {
                "missing-method" => {
                    invalid.links.remove(index);
                }
                "duplicate-method" => {
                    invalid.links[index].binding = Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt11,
                        unit: "sat".into(),
                    });
                }
                "wrong-unit" => {
                    invalid.links[index].binding = Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt12,
                        unit: "msat".into(),
                    });
                }
                "second-endpoint" => invalid.links[index].to = "payer".into(),
                "wrong-service" => {
                    invalid
                        .components
                        .iter_mut()
                        .find(|c| c.id == "processor")
                        .unwrap()
                        .implementation = "unknown-processor".into();
                }
                _ => unreachable!(),
            }
            let report = validate_cell(&invalid);
            assert!(
                report.issues.iter().any(|issue| issue.code
                    == if mutation == "wrong-service" {
                        "unsupported_processor_profile"
                    } else {
                        "ldk_processor_payment_bindings"
                    }),
                "{mutation}: {report:?}"
            );
        }
    }

    // Mint-side topology fixtures do not install Bark: catalog resolution still
    // requires the separately qualified backend, images and dependency graph.
    fn mint_cell(profile: ProcessorProfile) -> CellSpec {
        match profile {
            ProcessorProfile::LdkServer => example(),
            ProcessorProfile::Bark => {
                serde_json::from_str(include_str!("../tests/fixtures/bark-topology.json")).unwrap()
            }
        }
    }

    fn corrupt_mint(invalid: &mut CellSpec, profile: ProcessorProfile, mutation: &str) {
        let link_index = invalid
            .links
            .iter()
            .position(|l| l.id == "mint-bolt11")
            .unwrap();
        let processor_index = invalid
            .components
            .iter()
            .position(|c| c.id == "processor")
            .unwrap();
        match mutation {
            "missing-binding" => invalid.links[link_index].binding = None,
            "non-payment-binding" => {
                invalid.links[link_index].binding = Some(DependencyBinding::Chain {
                    network: crate::BitcoinNetwork::Regtest,
                });
            }
            "duplicate-binding" => {
                let mut duplicate = invalid.links[link_index].clone();
                duplicate.id = "duplicate".into();
                invalid.links.push(duplicate);
            }
            "wrong-unit" => {
                invalid.links[link_index].binding = Some(DependencyBinding::Payment {
                    method: PaymentMethod::Bolt11,
                    unit: "msat".into(),
                });
            }
            "onchain" => {
                invalid.links[link_index].binding = Some(DependencyBinding::Payment {
                    method: PaymentMethod::Onchain,
                    unit: "sat".into(),
                });
            }
            "wrong-kind" => invalid.components[processor_index].kind = ComponentKind::Lightning,
            "unknown-profile" => {
                invalid.components[processor_index].implementation = "unknown-processor".into();
            }
            "mixed-endpoints" | "mixed-profiles" => {
                let mut second = invalid.components[processor_index].clone();
                second.id = "other-processor".into();
                if mutation == "mixed-profiles" {
                    second.implementation = match profile {
                        ProcessorProfile::LdkServer => ProcessorProfile::Bark,
                        ProcessorProfile::Bark => ProcessorProfile::LdkServer,
                    }
                    .implementation()
                    .into();
                }
                invalid.components.push(second);
                if profile == ProcessorProfile::LdkServer {
                    invalid
                        .links
                        .iter_mut()
                        .find(|l| l.id == "mint-bolt12")
                        .unwrap()
                        .to = "other-processor".into();
                } else {
                    let mut extra = invalid.links[link_index].clone();
                    extra.id = "other-payment".into();
                    extra.to = "other-processor".into();
                    extra.binding = Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt12,
                        unit: "sat".into(),
                    });
                    invalid.links.push(extra);
                }
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn mint_bindings_require_the_selected_profile_not_the_ldk_default() {
        for profile in [ProcessorProfile::LdkServer, ProcessorProfile::Bark] {
            let cell = mint_cell(profile);
            assert!(validate_cell(&cell).valid, "{profile:?}");
            for mutation in [
                "missing-binding",
                "non-payment-binding",
                "duplicate-binding",
                "wrong-unit",
                "onchain",
                "wrong-kind",
                "unknown-profile",
                "mixed-endpoints",
                "mixed-profiles",
            ] {
                let mut invalid = cell.clone();
                corrupt_mint(&mut invalid, profile, mutation);
                let report = validate_cell(&invalid);
                let mint_path = format!(
                    "/components/{}",
                    invalid
                        .components
                        .iter()
                        .position(|c| c.id == "mint")
                        .unwrap()
                );
                assert!(
                    report
                        .issues
                        .iter()
                        .any(|issue| issue.path == mint_path && issue.code.contains("processor")),
                    "{profile:?}, {mutation}: {report:?}"
                );
                // Selection/refusal must not depend on authored link order.
                invalid.links.reverse();
                let reversed = validate_cell(&invalid);
                assert!(
                    reversed
                        .issues
                        .iter()
                        .any(|issue| issue.path == mint_path && issue.code.contains("processor")),
                    "{profile:?}, {mutation}, reversed: {reversed:?}"
                );
            }
        }
    }

    #[test]
    fn bark_refuses_bolt12_and_ldk_still_requires_it() {
        let mut bark = mint_cell(ProcessorProfile::Bark);
        let extra = example()
            .links
            .into_iter()
            .find(|l| l.id == "mint-bolt12")
            .unwrap();
        bark.links.push(extra);
        assert!(
            validate_cell(&bark)
                .issues
                .iter()
                .any(|i| i.code == "bark_processor_payment_bindings")
        );
        let mut ldk = mint_cell(ProcessorProfile::LdkServer);
        ldk.links.retain(|l| l.id != "mint-bolt12");
        assert!(
            validate_cell(&ldk)
                .issues
                .iter()
                .any(|i| i.code == "ldk_processor_payment_bindings")
        );
    }

    #[test]
    fn bark_profile_resolves_on_both_native_platforms() {
        let bark = mint_cell(ProcessorProfile::Bark);
        assert!(validate_cell(&bark).valid);
        for platform in [
            crate::CatalogPlatform::LinuxArm64,
            crate::CatalogPlatform::LinuxAmd64,
        ] {
            let catalog = crate::catalog_for_platform(platform);
            crate::resolve_lock(&bark, &catalog).unwrap();
        }
    }
}
