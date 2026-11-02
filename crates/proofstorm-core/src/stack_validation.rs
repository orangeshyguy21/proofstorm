//! Dependency checks shared by the experimental Bark and Spark stacks: each
//! stack component needs exactly one typed link per requirement and no other
//! backend links.
use crate::{
    BitcoinNetwork, CellSpec, ComponentKind, ComponentSpec, DatabaseRole, DependencyBinding,
    LinkKind, PaymentMethod, ValidationIssue,
};

use super::issue;

/// One backend link a stack component needs, and the component it must reach.
pub(super) struct Requirement {
    pub link_kind: LinkKind,
    pub target_kind: ComponentKind,
    pub target_impl: &'static str,
    /// The binding the link must carry, as shown in issue messages.
    pub label: &'static str,
    pub binding: fn(&DependencyBinding) -> bool,
}

pub(super) const CHAIN: Requirement = Requirement {
    link_kind: LinkKind::ChainBackend,
    target_kind: ComponentKind::Bitcoin,
    target_impl: "bitcoin-core",
    label: "chain/regtest",
    binding: |binding| {
        matches!(
            binding,
            DependencyBinding::Chain {
                network: BitcoinNetwork::Regtest
            }
        )
    },
};

pub(super) const DATABASE: Requirement = Requirement {
    link_kind: LinkKind::DatabaseBackend,
    target_kind: ComponentKind::Database,
    target_impl: "postgresql",
    label: "database/primary",
    binding: |binding| {
        matches!(
            binding,
            DependencyBinding::Database {
                role: DatabaseRole::Primary,
                ..
            }
        )
    },
};

/// A `bolt11/sat` payment binding.
pub(super) fn bolt11_sat(binding: &DependencyBinding) -> bool {
    matches!(
        binding,
        DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit,
        } if unit == "sat"
    )
}

impl Requirement {
    /// The target of the component's only link of this kind, when that link
    /// carries the required binding and reaches the required implementation.
    pub(super) fn resolve<'a>(
        &self,
        cell: &'a CellSpec,
        component: &ComponentSpec,
    ) -> Option<&'a ComponentSpec> {
        let mut links = cell
            .links
            .iter()
            .filter(|link| link.from == component.id && link.kind == self.link_kind);
        let link = links.next()?;
        if links.next().is_some() || !link.binding.as_ref().is_some_and(self.binding) {
            return None;
        }
        cell.components.iter().find(|target| {
            target.id == link.to
                && target.kind == self.target_kind
                && target.implementation == self.target_impl
        })
    }
}

/// Checks one stack component's kind, its required links and that it has no
/// other backend links; `prefix` names the stack in issue codes.
pub(super) fn validate_requirements(
    cell: &CellSpec,
    component: &ComponentSpec,
    path: &str,
    prefix: &str,
    kind: ComponentKind,
    requirements: &[Requirement],
    issues: &mut Vec<ValidationIssue>,
) {
    if component.kind != kind {
        issue(
            issues,
            &format!("{prefix}_component_kind_mismatch"),
            path,
            format!(
                "{} requires component kind {kind:?}",
                component.implementation
            ),
        );
    }
    for requirement in requirements {
        if requirement.resolve(cell, component).is_none() {
            issue(
                issues,
                &format!("{prefix}_dependency_required"),
                path,
                format!(
                    "{} requires exactly one {:?} link with {} binding to {}",
                    component.implementation,
                    requirement.link_kind,
                    requirement.label,
                    requirement.target_impl
                ),
            );
        }
    }
    for link in cell.links.iter().filter(|link| link.from == component.id) {
        if link.kind.is_backend()
            && !requirements
                .iter()
                .any(|requirement| requirement.link_kind == link.kind)
        {
            issue(
                issues,
                &format!("{prefix}_unexpected_dependency"),
                path,
                format!(
                    "{} does not use a {:?} dependency ({:?})",
                    component.implementation, link.kind, link.id
                ),
            );
        }
    }
}
