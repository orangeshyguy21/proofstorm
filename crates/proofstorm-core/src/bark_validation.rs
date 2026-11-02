//! Dependency graph of the reserved Bark stack. These checks do not
//! enable catalog support or substitute for managed runtime qualification.
use crate::{
    BitcoinNetwork, CellSpec, ComponentKind, ComponentSpec, DependencyBinding, LinkKind,
    ValidationIssue,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD},
};

use super::{
    issue,
    stack::{CHAIN, DATABASE, Requirement, bolt11_sat, validate_requirements},
};

const ARK: Requirement = Requirement {
    link_kind: LinkKind::ArkBackend,
    target_kind: ComponentKind::ArkServer,
    target_impl: BARK_SERVER,
    label: "ark/regtest",
    binding: |binding| {
        matches!(
            binding,
            DependencyBinding::Ark {
                network: BitcoinNetwork::Regtest
            }
        )
    },
};

const LIGHTNING: Requirement = Requirement {
    link_kind: LinkKind::PaymentBackend,
    target_kind: ComponentKind::Lightning,
    target_impl: CLN_HOLD,
    label: "bolt11/sat",
    binding: bolt11_sat,
};

pub(super) fn validate_topology(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in cell.components.iter().enumerate() {
        let (kind, requirements): (_, &[Requirement]) = match component.implementation.as_str() {
            BARK_PROCESSOR => (ComponentKind::PaymentProcessor, &[CHAIN, ARK]),
            BARK_SERVER => (ComponentKind::ArkServer, &[CHAIN, DATABASE, LIGHTNING]),
            CLN_HOLD => (ComponentKind::Lightning, &[CHAIN]),
            _ => continue,
        };
        let path = format!("/components/{index}");
        validate_requirements(cell, component, &path, "bark", kind, requirements, issues);
        validate_chain(cell, component, &path, issues);
    }
}

fn validate_chain(
    cell: &CellSpec,
    component: &ComponentSpec,
    path: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(chain) = CHAIN.resolve(cell, component) else {
        return;
    };
    if chain
        .config
        .get("txindex")
        .is_some_and(|value| value != &serde_json::Value::Bool(true))
    {
        issue(
            issues,
            "bark_txindex_required",
            path,
            format!(
                "Bark requires txindex=true on Bitcoin component {:?}",
                chain.id
            ),
        );
    }
    let downstream = match component.implementation.as_str() {
        BARK_PROCESSOR => ARK.resolve(cell, component),
        BARK_SERVER => LIGHTNING.resolve(cell, component),
        _ => None,
    };
    if let Some(downstream) = downstream
        && let Some(other_chain) = CHAIN.resolve(cell, downstream)
        && chain.id != other_chain.id
    {
        issue(
            issues,
            "bark_shared_chain_required",
            path,
            format!(
                "{:?} and {:?} must use the same Bitcoin component; found {:?} and {:?}",
                component.id, downstream.id, chain.id, other_chain.id
            ),
        );
    }
}
