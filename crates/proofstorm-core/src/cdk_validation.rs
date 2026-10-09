//! Topology constraints of the unified CDK mint: linked and embedded payment
//! paths must be unambiguous, and embedded backends need exactly one chain.
use super::{
    BTreeMap, BTreeSet, CellSpec, DependencyBinding, LinkKind, PaymentMethod, ValidationIssue,
    is_cashu_unit, issue,
};
use crate::DatabaseRole;

fn configured<'a>(component: &'a crate::ComponentSpec, field: &str, default: &'a str) -> &'a str {
    component
        .config
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(default)
}

/// The (method, unit, backend) tuples a CDK mint serves from embedded
/// backends. Upstream mintd always registers BDK on-chain in sat.
pub(crate) fn embedded_payment_tuples(
    component: &crate::ComponentSpec,
) -> Vec<(PaymentMethod, String, &'static str)> {
    let mut tuples = Vec::new();
    if configured(component, "embedded_lightning", "none") == "ldk-node" {
        let unit = configured(component, "embedded_lightning_unit", "sat");
        for method in [PaymentMethod::Bolt11, PaymentMethod::Bolt12] {
            tuples.push((method, unit.to_owned(), "ldk-node"));
        }
    }
    if configured(component, "embedded_onchain", "none") == "bdk" {
        tuples.push((PaymentMethod::Onchain, "sat".into(), "bdk"));
    }
    tuples
}

pub(super) fn validate_topology(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in cell.components.iter().enumerate() {
        if component.implementation != "cdk" {
            continue;
        }
        let path = format!("/components/{index}");
        let ldk = configured(component, "embedded_lightning", "none") == "ldk-node";
        let bdk = configured(component, "embedded_onchain", "none") == "bdk";
        if !is_cashu_unit(configured(component, "embedded_lightning_unit", "sat")) {
            issue(
                issues,
                "invalid_payment_unit",
                format!("{path}/config/embedded_lightning_unit"),
                "must be a lowercase Cashu unit of 1..=64 ASCII letters, digits, '-' or '_'",
            );
        }
        let outgoing = |kind| {
            cell.links
                .iter()
                .filter(move |link| link.from == component.id && link.kind == kind)
        };
        let chains = outgoing(LinkKind::ChainBackend).count();
        if (ldk || bdk) && chains != 1 {
            issue(
                issues,
                "cdk_embedded_chain_backend_required",
                path.clone(),
                "embedded LDK Node or BDK requires exactly one chain_backend link to Bitcoin Core",
            );
        } else if !(ldk || bdk) && chains > 0 {
            issue(
                issues,
                "cdk_chain_backend_unused",
                path.clone(),
                "a chain_backend link is only used by embedded_lightning = ldk-node or embedded_onchain = bdk",
            );
        }
        // Upstream refuses two backends for one (unit, method) pair.
        let mut claimed = embedded_payment_tuples(component)
            .into_iter()
            .map(|(method, unit, _)| (method, unit))
            .collect::<BTreeSet<_>>();
        let payments = outgoing(LinkKind::PaymentBackend).collect::<Vec<_>>();
        // Each linked backend renders one payment section with one unit.
        let mut units = BTreeMap::new();
        for link in &payments {
            let Some(DependencyBinding::Payment { method, unit }) = &link.binding else {
                continue;
            };
            if *units.entry(link.to.as_str()).or_insert(unit) != unit {
                issue(
                    issues,
                    "cdk_payment_backend_unit_mixed",
                    format!("{path}/links/{}", link.id),
                    format!(
                        "every payment_backend link to {:?} must use one unit; that backend renders one payment section",
                        link.to
                    ),
                );
            }
            if !claimed.insert((method.clone(), unit.clone())) {
                issue(
                    issues,
                    "cdk_payment_method_conflict",
                    format!("{path}/links/{}", link.id),
                    format!(
                        "{method:?}/{unit} is already served by another linked or embedded backend of this mint"
                    ),
                );
            }
        }
        if payments.is_empty() && !ldk && !bdk {
            issue(
                issues,
                "cdk_payment_path_required",
                path.clone(),
                "link a payment_backend or enable embedded_lightning or embedded_onchain",
            );
        }
        // Upstream keeps the auth store on the primary engine: SQLite beside
        // the mint database, or a separate required PostgreSQL database.
        let role_bound = |role| {
            outgoing(LinkKind::DatabaseBackend).any(|link| {
                matches!(&link.binding, Some(DependencyBinding::Database { role: bound, .. }) if *bound == role)
            })
        };
        let authenticated = outgoing(LinkKind::AuthenticationBackend).next().is_some();
        let postgres = role_bound(DatabaseRole::Primary);
        let auth_database = role_bound(DatabaseRole::Authentication);
        if authenticated && postgres && !auth_database {
            issue(
                issues,
                "cdk_authentication_database_required",
                path,
                "a CDK mint on PostgreSQL requires a database_backend link with role authentication",
            );
        } else if auth_database && !postgres {
            issue(
                issues,
                "cdk_authentication_database_unused",
                path,
                "a CDK mint on SQLite keeps its auth store in SQLite; remove the authentication database link",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::validate_cell;
    use serde_json::{Value, json};

    fn cell(mint_config: &Value, links: &Value) -> crate::CellSpec {
        serde_json::from_value(json!({
            "api_version": crate::API_VERSION,
            "name": "cdk-topology",
            "components": [
                {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
                {"id": "lightning", "kind": "lightning", "implementation": "lnd", "config_version": "lnd/0.20/v1", "control": "cell", "config": {}},
                {"id": "mint", "kind": "mint", "implementation": "cdk", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": mint_config.clone()}
            ],
            "links": links.clone(),
            "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
        }))
        .unwrap()
    }

    fn lnd() -> Value {
        json!({"id": "mint-lightning", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}})
    }

    fn chain() -> Value {
        json!({"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}})
    }

    fn codes(cell: &crate::CellSpec) -> Vec<String> {
        validate_cell(cell)
            .issues
            .into_iter()
            .map(|issue| issue.code)
            .filter(|code| code.starts_with("cdk_"))
            .collect()
    }

    #[test]
    fn linked_and_embedded_payment_paths_compose_without_ambiguity() {
        assert!(codes(&cell(&json!({}), &json!([lnd()]))).is_empty());
        assert!(
            codes(&cell(
                &json!({"embedded_onchain": "bdk"}),
                &json!([lnd(), chain()])
            ))
            .is_empty()
        );
        assert!(
            codes(&cell(
                &json!({"embedded_lightning": "ldk-node", "embedded_onchain": "bdk"}),
                &json!([chain()])
            ))
            .is_empty()
        );
        assert_eq!(
            codes(&cell(&json!({}), &json!([]))),
            ["cdk_payment_path_required"]
        );
        assert_eq!(
            codes(&cell(
                &json!({"embedded_lightning": "ldk-node"}),
                &json!([])
            )),
            ["cdk_embedded_chain_backend_required"]
        );
        assert_eq!(
            codes(&cell(&json!({}), &json!([lnd(), chain()]))),
            ["cdk_chain_backend_unused"]
        );
        assert_eq!(
            codes(&cell(
                &json!({"embedded_lightning": "ldk-node"}),
                &json!([lnd(), chain()])
            )),
            ["cdk_payment_method_conflict"]
        );
    }

    #[test]
    fn each_payment_section_has_one_valid_unit() {
        // A different embedded unit is a separate (unit, method) pair, not a conflict.
        assert!(
            codes(&cell(
                &json!({"embedded_lightning": "ldk-node", "embedded_lightning_unit": "msat"}),
                &json!([lnd(), chain()])
            ))
            .is_empty()
        );
        let report = validate_cell(&cell(
            &json!({"embedded_lightning": "ldk-node", "embedded_lightning_unit": "MSAT"}),
            &json!([chain()]),
        ));
        assert!(report.issues.iter().any(|issue| {
            issue.code == "invalid_payment_unit"
                && issue.path == "/components/2/config/embedded_lightning_unit"
        }));
        let mut bolt12 = lnd();
        bolt12["id"] = json!("mint-bolt12");
        bolt12["binding"] = json!({"type": "payment", "method": "bolt12", "unit": "msat"});
        assert!(
            codes(&cell(&json!({}), &json!([lnd(), bolt12])))
                .contains(&"cdk_payment_backend_unit_mixed".to_string())
        );
    }

    #[test]
    fn embedded_tuples_follow_configuration() {
        let mint = |config: Value| {
            cell(&config, &json!([chain()]))
                .components
                .into_iter()
                .find(|component| component.id == "mint")
                .unwrap()
        };
        assert!(super::embedded_payment_tuples(&mint(json!({}))).is_empty());
        let tuples = super::embedded_payment_tuples(&mint(json!({
            "embedded_lightning": "ldk-node",
            "embedded_lightning_unit": "msat",
            "embedded_onchain": "bdk"
        })));
        assert_eq!(
            tuples,
            [
                (crate::PaymentMethod::Bolt11, "msat".to_string(), "ldk-node"),
                (crate::PaymentMethod::Bolt12, "msat".to_string(), "ldk-node"),
                (crate::PaymentMethod::Onchain, "sat".to_string(), "bdk"),
            ]
        );
    }

    #[test]
    fn cdk_auth_store_follows_the_primary_database_engine() {
        let database = |role: &str| json!({"id": format!("mint-{role}"), "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": role}});
        let identity = json!({"id": "mint-identity", "kind": "authentication_backend", "from": "mint", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}});
        let with = |links: Value| {
            let mut cell = cell(&json!({}), &links);
            for (id, kind, implementation, version) in [
                ("database", "database", "postgresql", "postgresql/17/v1"),
                (
                    "identity",
                    "identity_provider",
                    "keycloak",
                    "keycloak/25/v1",
                ),
            ] {
                cell.components.push(
                    serde_json::from_value(json!({"id": id, "kind": kind, "implementation": implementation, "config_version": version, "control": "cell", "config": {}}))
                        .unwrap(),
                );
            }
            codes(&cell)
        };
        assert!(with(json!([lnd(), identity])).is_empty());
        assert!(
            with(json!([
                lnd(),
                identity,
                database("primary"),
                database("authentication")
            ]))
            .is_empty()
        );
        assert_eq!(
            with(json!([lnd(), identity, database("primary")])),
            ["cdk_authentication_database_required"]
        );
        assert_eq!(
            with(json!([lnd(), identity, database("authentication")])),
            ["cdk_authentication_database_unused"]
        );
    }
}
