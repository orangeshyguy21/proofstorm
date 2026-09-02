use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    API_VERSION, ComponentKind, DependencyBinding, LabSpec, LinkKind, LinkSpec, PaymentMethod,
};

const HARD_MAX_COMPONENTS: usize = 128;
const HARD_MAX_LINKS: usize = 1_024;
const HARD_MAX_CONFIG_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationIssue {
    pub code: String,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    #[must_use]
    pub fn from_issues(issues: Vec<ValidationIssue>) -> Self {
        Self {
            valid: issues.is_empty(),
            issues,
        }
    }
}

#[must_use]
pub fn validate_lab(lab: &LabSpec) -> ValidationReport {
    let mut issues = Vec::new();

    if lab.api_version != API_VERSION {
        issue(
            &mut issues,
            "unsupported_api_version",
            "/api_version",
            format!("expected {API_VERSION:?}"),
        );
    }
    if !is_slug(&lab.name) {
        issue(
            &mut issues,
            "invalid_lab_name",
            "/name",
            "must be a lowercase kebab-case identifier of 1..=63 bytes",
        );
    }
    if lab.components.len() > HARD_MAX_COMPONENTS {
        issue(
            &mut issues,
            "too_many_components",
            "/components",
            format!("hard maximum is {HARD_MAX_COMPONENTS}"),
        );
    }
    if lab.links.len() > HARD_MAX_LINKS {
        issue(
            &mut issues,
            "too_many_links",
            "/links",
            format!("hard maximum is {HARD_MAX_LINKS}"),
        );
    }

    validate_limits(lab, &mut issues);

    let ids = validate_components(lab, &mut issues);
    let kinds = lab
        .components
        .iter()
        .map(|component| (component.id.as_str(), component.kind))
        .collect::<BTreeMap<_, _>>();

    validate_links(lab, &ids, &kinds, &mut issues);
    validate_authentication_topology(lab, &mut issues);

    ValidationReport::from_issues(issues)
}

fn validate_authentication_topology(lab: &LabSpec, issues: &mut Vec<ValidationIssue>) {
    for (index, component) in lab.components.iter().enumerate() {
        if component.implementation == "keycloak" {
            let primary_databases = lab
                .links
                .iter()
                .filter(|link| {
                    link.from == component.id
                        && link.kind == LinkKind::DatabaseBackend
                        && matches!(
                            link.binding,
                            Some(DependencyBinding::Database {
                                role: crate::DatabaseRole::Primary
                            })
                        )
                })
                .count();
            if primary_databases != 1 {
                issue(
                    issues,
                    "keycloak_primary_database_required",
                    format!("/components/{index}"),
                    "Keycloak requires exactly one primary PostgreSQL dependency",
                );
            }
        }
        if component.implementation != "nutshell" {
            continue;
        }
        let authentication_links = lab
            .links
            .iter()
            .filter(|link| {
                link.from == component.id && link.kind == LinkKind::AuthenticationBackend
            })
            .count();
        if authentication_links > 1 {
            issue(
                issues,
                "duplicate_authentication_provider",
                format!("/components/{index}"),
                "Nutshell accepts at most one authentication provider",
            );
        }
        if authentication_links == 0 {
            continue;
        }
        if component
            .config
            .get("oidc_discovery_url")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|url| !url.is_empty())
        {
            issue(
                issues,
                "ambiguous_oidc_discovery",
                format!("/components/{index}/config/oidc_discovery_url"),
                "linked authentication derives the discovery URL; remove the authored URL",
            );
        }
        if component
            .config
            .get("oidc_client_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|client| client != "cashu-client")
        {
            issue(
                issues,
                "linked_oidc_client_mismatch",
                format!("/components/{index}/config/oidc_client_id"),
                "the in-lab Keycloak contract uses the fixed public client cashu-client",
            );
        }
    }
}

fn validate_links(
    lab: &LabSpec,
    ids: &BTreeSet<&str>,
    kinds: &BTreeMap<&str, ComponentKind>,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut link_ids = BTreeSet::new();
    for (index, link) in lab.links.iter().enumerate() {
        validate_link_identity(index, link, &mut link_ids, issues);
        validate_binding(index, link, issues);
        validate_database_role(index, link, &lab.links[..index], issues);
        if link.from == link.to {
            issue(
                issues,
                "self_link",
                format!("/links/{index}"),
                "link endpoints must be different",
            );
        }
        for (field, endpoint) in [("from", &link.from), ("to", &link.to)] {
            if !ids.contains(endpoint.as_str()) {
                issue(
                    issues,
                    "missing_link_endpoint",
                    format!("/links/{index}/{field}"),
                    format!("component {endpoint:?} does not exist"),
                );
            }
        }
        if let (Some(from), Some(to)) = (
            kinds.get(link.from.as_str()).copied(),
            kinds.get(link.to.as_str()).copied(),
        ) {
            let compatible = match link.kind {
                LinkKind::BitcoinPeer => {
                    from == ComponentKind::Bitcoin && to == ComponentKind::Bitcoin
                }
                LinkKind::LightningPeer => {
                    from == ComponentKind::Lightning && to == ComponentKind::Lightning
                }
                LinkKind::ChainBackend => {
                    matches!(from, ComponentKind::Lightning | ComponentKind::Mint)
                        && to == ComponentKind::Bitcoin
                }
                LinkKind::PaymentBackend => {
                    from == ComponentKind::Mint
                        && match &link.binding {
                            Some(DependencyBinding::Payment {
                                method: PaymentMethod::Onchain,
                                ..
                            }) => to == ComponentKind::Bitcoin,
                            Some(DependencyBinding::Payment { .. }) => {
                                to == ComponentKind::Lightning
                            }
                            _ => matches!(to, ComponentKind::Lightning | ComponentKind::Bitcoin),
                        }
                }
                LinkKind::DatabaseBackend => {
                    matches!(from, ComponentKind::Mint | ComponentKind::IdentityProvider)
                        && to == ComponentKind::Database
                }
                LinkKind::AuthenticationBackend => {
                    from == ComponentKind::Mint && to == ComponentKind::IdentityProvider
                }
                LinkKind::NetworkPath => true,
            };
            if !compatible {
                issue(
                    issues,
                    "incompatible_link_kinds",
                    format!("/links/{index}"),
                    format!("{:?} does not support {:?} -> {:?}", link.kind, from, to),
                );
            }
        }
    }
}

fn validate_database_role(
    index: usize,
    link: &LinkSpec,
    previous: &[LinkSpec],
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(DependencyBinding::Database { role }) = link.binding else {
        return;
    };
    if role == crate::DatabaseRole::Authentication {
        issue(
            issues,
            "unsupported_database_role",
            format!("/links/{index}/binding/role"),
            "authentication databases are reserved for a future CDK auth slice",
        );
        return;
    }
    let duplicated = previous.iter().any(|candidate| {
        candidate.from == link.from
            && matches!(
                candidate.binding,
                Some(DependencyBinding::Database {
                    role: candidate_role
                }) if candidate_role == role
            )
    });
    if duplicated {
        let (code, message) = match role {
            crate::DatabaseRole::Primary => (
                "duplicate_primary_database",
                "a component can bind exactly one primary database",
            ),
            crate::DatabaseRole::Cache => (
                "duplicate_cache_database",
                "a component can bind exactly one cache database",
            ),
            crate::DatabaseRole::Authentication => unreachable!(),
        };
        issue(issues, code, format!("/links/{index}"), message);
    }
}

fn validate_binding(index: usize, link: &LinkSpec, issues: &mut Vec<ValidationIssue>) {
    let path = format!("/links/{index}/binding");
    match (&link.kind, &link.binding) {
        (LinkKind::ChainBackend, Some(DependencyBinding::Chain { .. }))
        | (LinkKind::PaymentBackend, Some(DependencyBinding::Payment { .. }))
        | (LinkKind::DatabaseBackend, Some(DependencyBinding::Database { .. }))
        | (LinkKind::AuthenticationBackend, Some(DependencyBinding::Authentication { .. })) => {}
        (
            LinkKind::ChainBackend
            | LinkKind::PaymentBackend
            | LinkKind::DatabaseBackend
            | LinkKind::AuthenticationBackend,
            None,
        ) => {
            issue(
                issues,
                "missing_dependency_binding",
                path,
                "backend links require a typed binding payload",
            );
        }
        (
            LinkKind::ChainBackend
            | LinkKind::PaymentBackend
            | LinkKind::DatabaseBackend
            | LinkKind::AuthenticationBackend,
            Some(_),
        ) => issue(
            issues,
            "incompatible_dependency_binding",
            path,
            format!("binding payload does not match {:?}", link.kind),
        ),
        (_, Some(_)) => issue(
            issues,
            "unexpected_dependency_binding",
            path,
            "peer and network-path links cannot carry dependency binding payloads",
        ),
        (_, None) => {}
    }
    if let Some(DependencyBinding::Payment { unit, .. }) = &link.binding {
        if !is_unit_identifier(unit) {
            issue(
                issues,
                "invalid_payment_unit",
                format!("/links/{index}/binding/unit"),
                "must be a lowercase unit identifier of 1..=16 ASCII letters, digits, or '-'",
            );
        }
    }
}

fn validate_link_identity<'a>(
    index: usize,
    link: &'a LinkSpec,
    ids: &mut BTreeSet<&'a str>,
    issues: &mut Vec<ValidationIssue>,
) {
    if !is_slug(&link.id) {
        issue(
            issues,
            "invalid_link_id",
            format!("/links/{index}/id"),
            "must be a lowercase kebab-case binding identifier of 1..=63 bytes",
        );
    }
    if !ids.insert(link.id.as_str()) {
        issue(
            issues,
            "duplicate_link_id",
            format!("/links/{index}/id"),
            format!("binding {:?} appears more than once", link.id),
        );
    }
}

fn validate_components<'a>(
    lab: &'a LabSpec,
    issues: &mut Vec<ValidationIssue>,
) -> BTreeSet<&'a str> {
    let mut ids = BTreeSet::new();
    for (index, component) in lab.components.iter().enumerate() {
        let path = format!("/components/{index}/id");
        if !is_slug(&component.id) {
            issue(
                issues,
                "invalid_component_id",
                path.clone(),
                "must be a lowercase kebab-case identifier of 1..=63 bytes",
            );
        }
        if !ids.insert(component.id.as_str()) {
            issue(
                issues,
                "duplicate_component_id",
                path,
                format!("component {:?} appears more than once", component.id),
            );
        }
        if !is_slug(&component.implementation) {
            issue(
                issues,
                "invalid_implementation",
                format!("/components/{index}/implementation"),
                "must be a lowercase kebab-case catalog identifier",
            );
        }
        if !is_config_version_identifier(&component.config_version) {
            issue(
                issues,
                "invalid_config_version",
                format!("/components/{index}/config_version"),
                "must be an implementation-scoped identifier of 1..=128 ASCII alphanumeric, '.', '+', '-', or '/' characters with non-empty path segments",
            );
        }
        let config_bytes = serde_json::to_vec(&component.config).map_or(usize::MAX, |v| v.len());
        if config_bytes > usize::try_from(lab.policy.limits.max_config_bytes).unwrap_or(usize::MAX)
            || config_bytes > HARD_MAX_CONFIG_BYTES
        {
            issue(
                issues,
                "config_too_large",
                format!("/components/{index}/config"),
                "serialized configuration exceeds the configured or hard byte limit",
            );
        }
    }
    ids
}

fn validate_limits(lab: &LabSpec, issues: &mut Vec<ValidationIssue>) {
    let limits = &lab.policy.limits;
    for (field, value, hard_max) in [
        (
            "max_components",
            usize::from(limits.max_components),
            HARD_MAX_COMPONENTS,
        ),
        ("max_links", usize::from(limits.max_links), HARD_MAX_LINKS),
        (
            "max_config_bytes",
            usize::try_from(limits.max_config_bytes).unwrap_or(usize::MAX),
            HARD_MAX_CONFIG_BYTES,
        ),
    ] {
        if value == 0 || value > hard_max {
            issue(
                issues,
                "invalid_limit",
                format!("/policy/limits/{field}"),
                format!("must be in 1..={hard_max}"),
            );
        }
    }
    if lab.components.len() > usize::from(limits.max_components) {
        issue(
            issues,
            "component_limit_exceeded",
            "/components",
            "component count exceeds policy limit",
        );
    }
    if lab.links.len() > usize::from(limits.max_links) {
        issue(
            issues,
            "link_limit_exceeded",
            "/links",
            "link count exceeds policy limit",
        );
    }
}

fn issue(
    issues: &mut Vec<ValidationIssue>,
    code: &str,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    issues.push(ValidationIssue {
        code: code.into(),
        path: path.into(),
        message: message.into(),
    });
}

fn is_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    let mut previous_dash = false;
    for &byte in bytes {
        let is_dash = byte == b'-';
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || is_dash)
            || (is_dash && previous_dash)
        {
            return false;
        }
        previous_dash = is_dash;
    }
    true
}

fn is_config_version_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
        })
}

fn is_unit_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{ComponentKind, ComponentSpec, ControlClass, LabPolicy, LinkKind, LinkSpec};

    use super::*;

    fn valid_lab() -> LabSpec {
        LabSpec {
            api_version: API_VERSION.into(),
            name: "cdk-lightning-lab".into(),
            components: vec![
                ComponentSpec {
                    id: "chain".into(),
                    kind: ComponentKind::Bitcoin,
                    implementation: "bitcoin-core".into(),
                    version: Some("30.0".into()),
                    config_version: "bitcoin-core/30/v1".into(),
                    control: ControlClass::Laboratory,
                    config: BTreeMap::new(),
                },
                ComponentSpec {
                    id: "mint-lnd".into(),
                    kind: ComponentKind::Lightning,
                    implementation: "lnd".into(),
                    version: None,
                    config_version: "lnd/0.20/v1".into(),
                    control: ControlClass::Laboratory,
                    config: BTreeMap::new(),
                },
                ComponentSpec {
                    id: "target".into(),
                    kind: ComponentKind::Mint,
                    implementation: "cdk".into(),
                    version: None,
                    config_version: "cdk-mintd/0.17/v1".into(),
                    control: ControlClass::Target,
                    config: BTreeMap::new(),
                },
            ],
            links: vec![
                LinkSpec {
                    id: "lnd-chain".into(),
                    kind: LinkKind::ChainBackend,
                    from: "mint-lnd".into(),
                    to: "chain".into(),
                    binding: Some(DependencyBinding::Chain {
                        network: crate::BitcoinNetwork::Regtest,
                    }),
                },
                LinkSpec {
                    id: "mint-lightning".into(),
                    kind: LinkKind::PaymentBackend,
                    from: "target".into(),
                    to: "mint-lnd".into(),
                    binding: Some(DependencyBinding::Payment {
                        method: PaymentMethod::Bolt11,
                        unit: "sat".into(),
                    }),
                },
            ],
            policy: LabPolicy::default(),
        }
    }

    #[test]
    fn valid_bitcoin_lnd_cdk_lab_passes() {
        assert_eq!(
            validate_lab(&valid_lab()),
            ValidationReport::from_issues(vec![])
        );
    }

    #[test]
    fn duplicate_component_and_missing_endpoint_are_stable_issues() {
        let mut lab = valid_lab();
        lab.components[1].id = "chain".into();
        lab.links[1].to = "absent".into();
        let report = validate_lab(&lab);
        assert!(!report.valid);
        assert_eq!(
            report
                .issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "duplicate_component_id",
                "missing_link_endpoint",
                "missing_link_endpoint"
            ]
        );
    }

    #[test]
    fn binding_id_is_required_valid_and_unique() {
        let mut lab = valid_lab();
        lab.links[0].id = "Not Valid".into();
        lab.links[1].id = "Not Valid".into();
        let report = validate_lab(&lab);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "invalid_link_id" && issue.path == "/links/0/id")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "duplicate_link_id" && issue.path == "/links/1/id")
        );
    }

    #[test]
    fn backend_bindings_are_typed_and_payment_endpoints_follow_the_method() {
        let mut lab = valid_lab();
        lab.links[0].binding = None;
        lab.links[1].binding = Some(DependencyBinding::Chain {
            network: crate::BitcoinNetwork::Regtest,
        });
        let report = validate_lab(&lab);
        assert!(report.issues.iter().any(|issue| {
            issue.code == "missing_dependency_binding" && issue.path == "/links/0/binding"
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.code == "incompatible_dependency_binding" && issue.path == "/links/1/binding"
        }));

        let mut onchain = valid_lab();
        onchain.links[1].binding = Some(DependencyBinding::Payment {
            method: PaymentMethod::Onchain,
            unit: "SAT".into(),
        });
        let report = validate_lab(&onchain);
        assert!(report.issues.iter().any(|issue| {
            issue.code == "invalid_payment_unit" && issue.path == "/links/1/binding/unit"
        }));
        assert!(
            report.issues.iter().any(|issue| {
                issue.code == "incompatible_link_kinds" && issue.path == "/links/1"
            })
        );
    }

    #[test]
    fn keycloak_requires_primary_storage_and_linked_nutshell_refuses_oidc_overrides() {
        let mut lab = valid_lab();
        lab.components.extend([
            ComponentSpec {
                id: "identity-db".into(),
                kind: ComponentKind::Database,
                implementation: "postgresql".into(),
                version: None,
                config_version: "postgresql/17/v1".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            },
            ComponentSpec {
                id: "identity".into(),
                kind: ComponentKind::IdentityProvider,
                implementation: "keycloak".into(),
                version: None,
                config_version: "keycloak/25/v1".into(),
                control: ControlClass::Laboratory,
                config: BTreeMap::new(),
            },
        ]);
        lab.components[2].implementation = "nutshell".into();
        lab.components[2].config_version = "nutshell-mint/0.20/v1".into();
        lab.components[2].config.insert(
            "oidc_discovery_url".into(),
            serde_json::json!("https://issuer.example/realm/.well-known/openid-configuration"),
        );
        lab.links.push(LinkSpec {
            id: "mint-authentication".into(),
            kind: LinkKind::AuthenticationBackend,
            from: "target".into(),
            to: "identity".into(),
            binding: Some(DependencyBinding::Authentication {
                protocol: crate::AuthenticationProtocol::Oidc,
            }),
        });
        let report = validate_lab(&lab);
        assert!(report.issues.iter().any(|issue| {
            issue.code == "keycloak_primary_database_required" && issue.path == "/components/4"
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.code == "ambiguous_oidc_discovery"
                && issue.path == "/components/2/config/oidc_discovery_url"
        }));

        lab.components[2].config.clear();
        lab.links.push(LinkSpec {
            id: "identity-database".into(),
            kind: LinkKind::DatabaseBackend,
            from: "identity".into(),
            to: "identity-db".into(),
            binding: Some(DependencyBinding::Database {
                role: crate::DatabaseRole::Primary,
            }),
        });
        assert_eq!(validate_lab(&lab), ValidationReport::from_issues(vec![]));
    }

    #[test]
    fn unsupported_version_and_invalid_names_refuse() {
        let mut lab = valid_lab();
        lab.api_version = "proofstorm/v2".into();
        lab.name = "Not Valid".into();
        lab.components[0].implementation = String::new();
        lab.components[1].config_version = String::new();
        let report = validate_lab(&lab);
        assert_eq!(
            report
                .issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "unsupported_api_version",
                "invalid_lab_name",
                "invalid_implementation",
                "invalid_config_version"
            ]
        );
    }

    #[test]
    fn zero_and_exceeded_limits_refuse() {
        let mut lab = valid_lab();
        lab.policy.limits.max_components = 0;
        lab.policy.limits.max_links = 1;
        let report = validate_lab(&lab);
        assert_eq!(
            report
                .issues
                .iter()
                .map(|issue| issue.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "invalid_limit",
                "component_limit_exceeded",
                "link_limit_exceeded"
            ]
        );
    }
}
