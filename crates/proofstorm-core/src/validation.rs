use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{API_VERSION, ComponentKind, LabSpec, LinkKind};

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

    for (index, link) in lab.links.iter().enumerate() {
        if link.from == link.to {
            issue(
                &mut issues,
                "self_link",
                format!("/links/{index}"),
                "link endpoints must be different",
            );
        }
        for (field, endpoint) in [("from", &link.from), ("to", &link.to)] {
            if !ids.contains(endpoint.as_str()) {
                issue(
                    &mut issues,
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
                    from == ComponentKind::Lightning && to == ComponentKind::Bitcoin
                }
                LinkKind::LightningBackend => {
                    from == ComponentKind::Mint && to == ComponentKind::Lightning
                }
                LinkKind::NetworkPath => true,
            };
            if !compatible {
                issue(
                    &mut issues,
                    "incompatible_link_kinds",
                    format!("/links/{index}"),
                    format!("{:?} does not support {:?} -> {:?}", link.kind, from, to),
                );
            }
        }
    }

    ValidationReport::from_issues(issues)
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
        if !is_version_identifier(&component.config_version) {
            issue(
                issues,
                "invalid_config_version",
                format!("/components/{index}/config_version"),
                "must be a non-empty version identifier of at most 64 ASCII alphanumeric, '.', '+', or '-' characters",
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

fn is_version_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
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
                    config_version: "v1alpha1".into(),
                    control: ControlClass::Laboratory,
                    config: BTreeMap::new(),
                },
                ComponentSpec {
                    id: "mint-lnd".into(),
                    kind: ComponentKind::Lightning,
                    implementation: "lnd".into(),
                    version: None,
                    config_version: "v1alpha1".into(),
                    control: ControlClass::Laboratory,
                    config: BTreeMap::new(),
                },
                ComponentSpec {
                    id: "target".into(),
                    kind: ComponentKind::Mint,
                    implementation: "cdk".into(),
                    version: None,
                    config_version: "v1alpha1".into(),
                    control: ControlClass::Target,
                    config: BTreeMap::new(),
                },
            ],
            links: vec![
                LinkSpec {
                    kind: LinkKind::ChainBackend,
                    from: "mint-lnd".into(),
                    to: "chain".into(),
                },
                LinkSpec {
                    kind: LinkKind::LightningBackend,
                    from: "target".into(),
                    to: "mint-lnd".into(),
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
