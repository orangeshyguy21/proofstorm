//! Dependency graph of the reserved Bark stack. These checks do not
//! enable catalog support or substitute for managed runtime qualification.
use crate::{
    BitcoinNetwork, CellSpec, ComponentKind, ComponentSpec, DatabaseRole, DependencyBinding,
    LinkKind, PaymentMethod, ValidationIssue,
    processor_ids::{BARK_PROCESSOR, BARK_SERVER, CLN_HOLD},
};

use super::issue;

#[derive(Clone, Copy)]
enum Dependency {
    Chain,
    Ark,
    Database,
    Lightning,
}

impl Dependency {
    const fn kind(self) -> LinkKind {
        match self {
            Self::Chain => LinkKind::ChainBackend,
            Self::Ark => LinkKind::ArkBackend,
            Self::Database => LinkKind::DatabaseBackend,
            Self::Lightning => LinkKind::PaymentBackend,
        }
    }

    const fn target(self) -> (ComponentKind, &'static str, &'static str) {
        match self {
            Self::Chain => (ComponentKind::Bitcoin, "bitcoin-core", "chain/regtest"),
            Self::Ark => (ComponentKind::ArkServer, BARK_SERVER, "ark/regtest"),
            Self::Database => (ComponentKind::Database, "postgresql", "database/primary"),
            Self::Lightning => (ComponentKind::Lightning, CLN_HOLD, "bolt11/sat"),
        }
    }

    fn matches(self, binding: Option<&DependencyBinding>) -> bool {
        match (self, binding) {
            (
                Self::Chain,
                Some(DependencyBinding::Chain {
                    network: BitcoinNetwork::Regtest,
                }),
            )
            | (
                Self::Ark,
                Some(DependencyBinding::Ark {
                    network: BitcoinNetwork::Regtest,
                }),
            )
            | (
                Self::Database,
                Some(DependencyBinding::Database {
                    role: DatabaseRole::Primary,
                    ..
                }),
            ) => true,
            (
                Self::Lightning,
                Some(DependencyBinding::Payment {
                    method: PaymentMethod::Bolt11,
                    unit,
                }),
            ) => unit == "sat",
            _ => false,
        }
    }

    fn resolve<'a>(
        self,
        cell: &'a CellSpec,
        component: &ComponentSpec,
    ) -> Option<&'a ComponentSpec> {
        let mut links = cell
            .links
            .iter()
            .filter(|link| link.from == component.id && link.kind == self.kind());
        let link = links.next()?;
        if links.next().is_some() || !self.matches(link.binding.as_ref()) {
            return None;
        }
        let (kind, implementation, _) = self.target();
        cell.components.iter().find(|target| {
            target.id == link.to && target.kind == kind && target.implementation == implementation
        })
    }
}

pub(super) fn validate_topology(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in cell.components.iter().enumerate() {
        let (kind, dependencies): (_, &[Dependency]) = match component.implementation.as_str() {
            BARK_PROCESSOR => (
                ComponentKind::PaymentProcessor,
                &[Dependency::Chain, Dependency::Ark],
            ),
            BARK_SERVER => (
                ComponentKind::ArkServer,
                &[
                    Dependency::Chain,
                    Dependency::Database,
                    Dependency::Lightning,
                ],
            ),
            CLN_HOLD => (ComponentKind::Lightning, &[Dependency::Chain]),
            _ => continue,
        };
        let path = format!("/components/{index}");
        if component.kind != kind {
            issue(
                issues,
                "bark_component_kind_mismatch",
                &path,
                format!(
                    "{} requires component kind {kind:?}",
                    component.implementation
                ),
            );
        }
        for dependency in dependencies {
            if dependency.resolve(cell, component).is_none() {
                let (_, implementation, binding) = dependency.target();
                issue(
                    issues,
                    "bark_dependency_required",
                    &path,
                    format!(
                        "{} requires exactly one {:?} link with {binding} binding to {implementation}",
                        component.implementation,
                        dependency.kind()
                    ),
                );
            }
        }
        for link in cell.links.iter().filter(|link| link.from == component.id) {
            if matches!(
                link.kind,
                LinkKind::ChainBackend
                    | LinkKind::ArkBackend
                    | LinkKind::PaymentBackend
                    | LinkKind::DatabaseBackend
                    | LinkKind::AuthenticationBackend
            ) && !dependencies
                .iter()
                .any(|dependency| dependency.kind() == link.kind)
            {
                issue(
                    issues,
                    "bark_unexpected_dependency",
                    &path,
                    format!(
                        "{} does not use a {:?} dependency ({:?})",
                        component.implementation, link.kind, link.id
                    ),
                );
            }
        }
        validate_chain(cell, component, &path, issues);
    }
}

fn validate_chain(
    cell: &CellSpec,
    component: &ComponentSpec,
    path: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(chain) = Dependency::Chain.resolve(cell, component) else {
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
        BARK_PROCESSOR => Dependency::Ark.resolve(cell, component),
        BARK_SERVER => Dependency::Lightning.resolve(cell, component),
        _ => None,
    };
    if let Some(downstream) = downstream {
        if let Some(other_chain) = Dependency::Chain.resolve(cell, downstream) {
            if chain.id != other_chain.id {
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
    }
}
