//! Topology constraints of the installed LDK Server processor profile.
use super::{
    BTreeSet, CellSpec, ComponentKind, DependencyBinding, LinkKind, PaymentMethod, ValidationIssue,
    issue,
};

pub(super) fn validate_topology(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in cell.components.iter().enumerate() {
        let links = cell
            .links
            .iter()
            .filter(|link| link.from == component.id && link.kind == LinkKind::PaymentBackend)
            .collect::<Vec<_>>();
        let processor = component.implementation == "cdk-ldk-server-processor";
        let grpc_mint = component.implementation == "cdk"
            && links.iter().any(|link| {
                cell.components.iter().any(|target| {
                    target.id == link.to && target.kind == ComponentKind::PaymentProcessor
                })
            });
        if !processor && !grpc_mint {
            continue;
        }
        let targets = links
            .iter()
            .map(|link| link.to.as_str())
            .collect::<BTreeSet<_>>();
        let methods = links
            .iter()
            .filter_map(|link| match &link.binding {
                Some(DependencyBinding::Payment { method, unit }) if unit == "sat" => Some(*method),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        // This profile advertises both methods. Keep the authored topology and
        // CDK's GetSettings-based registration identical, with one endpoint.
        let target_implementation = if processor {
            "ldk-server"
        } else {
            "cdk-ldk-server-processor"
        };
        if links.len() != 2
            || targets.len() != 1
            || methods != BTreeSet::from([PaymentMethod::Bolt11, PaymentMethod::Bolt12])
            || !targets.iter().all(|id| {
                cell.components.iter().any(|target| {
                    target.id == *id && target.implementation == target_implementation
                })
            })
        {
            issue(
                issues,
                "ldk_processor_payment_bindings",
                format!("/components/{index}"),
                format!(
                    "this profile requires bolt11/sat and bolt12/sat payment bindings to one {target_implementation} component"
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate_cell;

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
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == "ldk_processor_payment_bindings"),
                "{mutation}: {report:?}"
            );
        }
    }
}
