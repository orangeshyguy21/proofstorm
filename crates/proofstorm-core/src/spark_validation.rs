//! Dependency graph of the experimental Spark stack. The processor links only
//! to its SSP and reaches the operators through the SSP's own link, so the two
//! can never name different federations.
use crate::{
    BitcoinNetwork, CellSpec, ComponentKind, ComponentSpec, DatabaseRole, DependencyBinding,
    LinkKind, PaymentMethod, ValidationIssue,
    processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR},
};

use super::issue;

#[derive(Clone, Copy)]
enum Dependency {
    Chain,
    Database,
    Operators,
    Ssp,
    Lightning,
}

impl Dependency {
    const fn kind(self) -> LinkKind {
        match self {
            Self::Chain => LinkKind::ChainBackend,
            Self::Database => LinkKind::DatabaseBackend,
            Self::Operators | Self::Ssp => LinkKind::SparkBackend,
            Self::Lightning => LinkKind::PaymentBackend,
        }
    }

    const fn target(self) -> (ComponentKind, &'static str, &'static str) {
        match self {
            Self::Chain => (ComponentKind::Bitcoin, "bitcoin-core", "chain/regtest"),
            Self::Database => (ComponentKind::Database, "postgresql", "database/primary"),
            Self::Operators => (ComponentKind::SparkServer, SPARK_OPERATORS, "spark/regtest"),
            Self::Ssp => (ComponentKind::SparkServer, OPEN_SSP, "spark/regtest"),
            Self::Lightning => (ComponentKind::Lightning, "ldk-server", "bolt11/sat"),
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
                Self::Operators | Self::Ssp,
                Some(DependencyBinding::Spark {
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
            SPARK_PROCESSOR => (ComponentKind::PaymentProcessor, &[Dependency::Ssp]),
            OPEN_SSP => (
                ComponentKind::SparkServer,
                &[Dependency::Operators, Dependency::Lightning],
            ),
            SPARK_OPERATORS => (
                ComponentKind::SparkServer,
                &[Dependency::Chain, Dependency::Database],
            ),
            _ => continue,
        };
        let path = format!("/components/{index}");
        if component.kind != kind {
            issue(
                issues,
                "spark_component_kind_mismatch",
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
                    "spark_dependency_required",
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
                    | LinkKind::SparkBackend
                    | LinkKind::PaymentBackend
                    | LinkKind::DatabaseBackend
                    | LinkKind::AuthenticationBackend
            ) && !dependencies
                .iter()
                .any(|dependency| dependency.kind() == link.kind)
            {
                issue(
                    issues,
                    "spark_unexpected_dependency",
                    &path,
                    format!(
                        "{} does not use a {:?} dependency ({:?})",
                        component.implementation, link.kind, link.id
                    ),
                );
            }
        }
        match component.implementation.as_str() {
            OPEN_SSP => validate_shared_chain(cell, component, &path, issues),
            SPARK_OPERATORS => validate_database_names(cell, component, &path, issues),
            _ => {}
        }
    }
}

/// Each operator creates `{name}_{i}` and `{name}_{i}_ephemeral` from the
/// link's database name, and PostgreSQL identifiers end at 63 bytes.
fn validate_database_names(
    cell: &CellSpec,
    operators: &ComponentSpec,
    path: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    const LONGEST_SUFFIX: usize = "_4_ephemeral".len();
    for name in cell
        .links
        .iter()
        .filter(|link| link.from == operators.id && link.kind == LinkKind::DatabaseBackend)
        .filter_map(crate::LinkSpec::database_name)
    {
        if name.len() + LONGEST_SUFFIX > 63 {
            issue(
                issues,
                "spark_database_name_too_long",
                path,
                format!(
                    "operators create {name}_<n>_ephemeral databases; keep the database name at most {} bytes",
                    63 - LONGEST_SUFFIX
                ),
            );
        }
    }
}

/// The operators watch one chain and the SSP's Lightning node pays on one
/// chain; deposits and channels only meet if both are the same node.
fn validate_shared_chain(
    cell: &CellSpec,
    ssp: &ComponentSpec,
    path: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let chain = |dependency: Dependency| {
        dependency
            .resolve(cell, ssp)
            .and_then(|target| Dependency::Chain.resolve(cell, target))
    };
    if let (Some(operators), Some(lightning)) =
        (chain(Dependency::Operators), chain(Dependency::Lightning))
        && operators.id != lightning.id
    {
        issue(
            issues,
            "spark_shared_chain_required",
            path,
            format!(
                "the Spark operators and {:?}'s Lightning node must use the same Bitcoin component; found {:?} and {:?}",
                ssp.id, operators.id, lightning.id
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        BitcoinNetwork, CellSpec, ComponentSpec, DependencyBinding, LinkKind, LinkSpec,
        SupportLifecycle, catalog::catalog_with_spark_test_images, resolve_lock, validate_cell,
    };

    fn fixture() -> CellSpec {
        serde_json::from_str(include_str!("../tests/fixtures/spark-topology.json")).unwrap()
    }

    fn refuses(cell: &CellSpec, code: &str) {
        let report = validate_cell(cell);
        assert!(
            !report.valid && report.issues.iter().any(|issue| issue.code == code),
            "{code}: {report:?}"
        );
    }

    fn component<'a>(cell: &'a mut CellSpec, id: &str) -> &'a mut ComponentSpec {
        cell.components.iter_mut().find(|c| c.id == id).unwrap()
    }

    fn link<'a>(cell: &'a mut CellSpec, id: &str) -> &'a mut LinkSpec {
        cell.links.iter_mut().find(|l| l.id == id).unwrap()
    }

    #[test]
    fn complete_graph_resolves_once_images_are_pinned() {
        let cell = fixture();
        assert!(validate_cell(&cell).valid, "{:?}", validate_cell(&cell));
        for (amd64, platform) in [(true, "linux/amd64"), (false, "linux/arm64")] {
            let catalog = catalog_with_spark_test_images(amd64);
            resolve_lock(&cell, &catalog).unwrap();
            for (id, version) in [
                ("spark-operators", "0.0.0-5e0c879"),
                ("open-ssp", "0.1.0-9c25f69"),
                ("cdk-spark-processor", "0.1.0-d7244e2"),
                ("ldk-server", "0.1.0-762bc47"),
            ] {
                let entry = catalog
                    .entries
                    .iter()
                    .find(|e| e.id == id && e.version == version)
                    .unwrap();
                assert_eq!(entry.support_lifecycle, SupportLifecycle::Experimental);
                assert_eq!(entry.build_provenance.as_ref().unwrap().platform, platform);
            }
        }
        let encoded = serde_json::to_string(&cell).unwrap();
        assert_eq!(serde_json::from_str::<CellSpec>(&encoded).unwrap(), cell);
    }

    #[test]
    fn every_spark_dependency_is_required_and_unambiguous() {
        let cell = fixture();
        for link in cell
            .links
            .iter()
            .filter(|link| !matches!(link.from.as_str(), "mint" | "ssp-node"))
        {
            let mut missing = cell.clone();
            missing.links.retain(|candidate| candidate.id != link.id);
            refuses(&missing, "spark_dependency_required");
            let mut duplicate = cell.clone();
            let mut extra = link.clone();
            extra.id = format!("{}-duplicate", link.id);
            duplicate.links.push(extra);
            refuses(&duplicate, "spark_dependency_required");
            let mut missing_binding = cell.clone();
            self::link(&mut missing_binding, &link.id).binding = None;
            refuses(&missing_binding, "missing_dependency_binding");
            refuses(&missing_binding, "spark_dependency_required");
        }
    }

    #[test]
    fn the_processor_links_only_to_its_ssp() {
        // Straight to the operators: the SSP is skipped.
        let mut direct = fixture();
        link(&mut direct, "processor-ssp").to = "operators".into();
        refuses(&direct, "spark_dependency_required");
        // A second path to the operators could name another federation.
        let mut both = fixture();
        both.links.push(LinkSpec {
            id: "processor-operators".into(),
            kind: LinkKind::SparkBackend,
            from: "processor".into(),
            to: "operators".into(),
            binding: Some(DependencyBinding::Spark {
                network: BitcoinNetwork::Regtest,
            }),
        });
        refuses(&both, "spark_dependency_required");
        let mut chain = fixture();
        chain.links.push(LinkSpec {
            id: "processor-chain".into(),
            kind: LinkKind::ChainBackend,
            from: "processor".into(),
            to: "chain".into(),
            binding: Some(DependencyBinding::Chain {
                network: BitcoinNetwork::Regtest,
            }),
        });
        refuses(&chain, "spark_unexpected_dependency");
    }

    #[test]
    fn operators_and_the_ssp_node_share_one_chain() {
        let mut cell = fixture();
        let mut other = cell.components[0].clone();
        other.id = "chain-b".into();
        cell.components.push(other);
        link(&mut cell, "ssp-node-chain").to = "chain-b".into();
        refuses(&cell, "spark_shared_chain_required");
    }

    #[test]
    fn operator_database_names_leave_room_for_each_operator() {
        let named = |name: &str| {
            let mut cell = fixture();
            link(&mut cell, "operators-database").binding = Some(DependencyBinding::Database {
                role: crate::DatabaseRole::Primary,
                database: Some(name.into()),
            });
            cell
        };
        assert!(validate_cell(&named(&"a".repeat(51))).valid);
        refuses(&named(&"a".repeat(52)), "spark_database_name_too_long");
    }

    #[test]
    fn a_spark_processor_plan_carries_its_ssps_operator_link() {
        let cell = fixture();
        let ids = |component: &str| {
            cell.plan_links(component)
                .into_iter()
                .map(|link| link.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("processor"), ["processor-ssp", "ssp-operators"]);
        assert_eq!(ids("ssp"), ["ssp-operators", "ssp-lightning"]);
        assert_eq!(ids("mint"), ["mint-bolt11"]);
    }

    #[test]
    fn components_keep_their_kinds() {
        let mut cell = fixture();
        component(&mut cell, "ssp").kind = crate::ComponentKind::ArkServer;
        refuses(&cell, "spark_component_kind_mismatch");
    }

    #[test]
    fn the_federation_needs_threshold_within_its_operators() {
        let catalog = catalog_with_spark_test_images(false);
        let lock = |operators: u64, threshold: u64| {
            let mut cell = fixture();
            component(&mut cell, "operators").config = [
                ("operators".to_owned(), serde_json::json!(operators)),
                ("threshold".to_owned(), serde_json::json!(threshold)),
            ]
            .into();
            resolve_lock(&cell, &catalog)
        };
        lock(3, 2).unwrap();
        lock(1, 1).unwrap();
        let refused = lock(2, 3).unwrap_err();
        assert!(
            refused.contains("/config/threshold must be less than or equal to /config/operators"),
            "{refused}"
        );
        // Each field also keeps its own bounds.
        for (operators, threshold) in [(6, 2), (3, 0)] {
            let refused = lock(operators, threshold).unwrap_err();
            assert!(!refused.contains("config_order_violation"), "{refused}");
        }
    }

    #[test]
    fn spark_links_need_declared_catalog_compatibility() {
        let fixture = fixture();
        for (consumer, target) in [
            ("cdk-spark-processor", "open-ssp"),
            ("open-ssp", "spark-operators"),
        ] {
            let mut catalog = catalog_with_spark_test_images(false);
            catalog
                .entries
                .iter_mut()
                .find(|entry| entry.id == consumer)
                .unwrap()
                .compatible_dependencies
                .retain(|support| support.implementation != target);
            let refused = resolve_lock(&fixture, &catalog).unwrap_err();
            assert!(
                refused.contains("does not declare SparkBackend compatibility"),
                "{consumer}: {refused}"
            );
        }
    }

    #[test]
    fn each_ldk_server_version_serves_only_its_consumer() {
        let catalog = catalog_with_spark_test_images(false);
        // open-ssp needs macaroon authentication.
        let mut spark = fixture();
        component(&mut spark, "ssp-node").version = Some("0.1.0-50fe752".into());
        let refused = resolve_lock(&spark, &catalog).unwrap_err();
        assert!(
            refused
                .contains("does not declare PaymentBackend compatibility with target \"ssp-node\""),
            "{refused}"
        );
        // The CDK LDK Server processor needs the API-key era.
        let mut ldk: CellSpec =
            serde_json::from_str(include_str!("../../../examples/ldk-server-cell.json")).unwrap();
        resolve_lock(&ldk, &catalog).unwrap();
        for component in ldk
            .components
            .iter_mut()
            .filter(|c| c.implementation == "ldk-server")
        {
            component.version = Some("0.1.0-762bc47".into());
        }
        let refused = resolve_lock(&ldk, &catalog).unwrap_err();
        assert!(
            refused.contains("does not declare PaymentBackend compatibility"),
            "{refused}"
        );
    }
}
