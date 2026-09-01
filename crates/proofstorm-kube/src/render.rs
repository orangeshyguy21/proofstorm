use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        core::v1::{
            LimitRange, LimitRangeItem, LimitRangeSpec, Namespace, ResourceQuota,
            ResourceQuotaSpec, ServiceAccount,
        },
        networking::v1::{NetworkPolicy, NetworkPolicySpec},
        rbac::v1::{Role, RoleBinding, RoleRef, Subject},
    },
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
};

pub const INSTANCE_LABEL: &str = "proofstorm.dev/instance";
const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";

#[derive(Debug, Clone)]
pub struct RenderedSecuritySpine {
    pub namespace: Namespace,
    pub quota: ResourceQuota,
    pub limits: LimitRange,
    pub default_deny: NetworkPolicy,
    pub service_account: ServiceAccount,
    pub role: Role,
    pub role_binding: RoleBinding,
}

#[must_use]
pub fn instance_namespace(instance_key: &str) -> String {
    format!("proofstorm-{instance_key}")
}

#[must_use]
pub fn render_security_spine(instance_key: &str) -> RenderedSecuritySpine {
    let namespace_name = instance_namespace(instance_key);
    let labels = BTreeMap::from([
        (INSTANCE_LABEL.to_owned(), instance_key.to_owned()),
        (MANAGED_BY_LABEL.to_owned(), "proofstormd".to_owned()),
    ]);
    let namespaced_metadata = |name: &str| ObjectMeta {
        name: Some(name.to_owned()),
        namespace: Some(namespace_name.clone()),
        labels: Some(labels.clone()),
        ..ObjectMeta::default()
    };

    let namespace = restricted_namespace(instance_key, &namespace_name);

    let quota = instance_quota(namespaced_metadata("proofstorm-instance-quota"));

    let limits = LimitRange {
        metadata: namespaced_metadata("proofstorm-container-limits"),
        spec: Some(LimitRangeSpec {
            limits: vec![LimitRangeItem {
                type_: "Container".to_owned(),
                default: Some(BTreeMap::from([
                    ("cpu".to_owned(), Quantity("500m".to_owned())),
                    ("memory".to_owned(), Quantity("512Mi".to_owned())),
                ])),
                default_request: Some(BTreeMap::from([
                    ("cpu".to_owned(), Quantity("100m".to_owned())),
                    ("memory".to_owned(), Quantity("128Mi".to_owned())),
                ])),
                ..LimitRangeItem::default()
            }],
        }),
    };

    let default_deny = NetworkPolicy {
        metadata: namespaced_metadata("default-deny-all"),
        spec: Some(NetworkPolicySpec {
            pod_selector: Option::default(),
            policy_types: Some(vec!["Ingress".to_owned(), "Egress".to_owned()]),
            ingress: Some(vec![]),
            egress: Some(vec![]),
        }),
    };

    let service_account = ServiceAccount {
        metadata: namespaced_metadata("proofstorm-workload"),
        automount_service_account_token: Some(false),
        ..ServiceAccount::default()
    };
    let role = Role {
        metadata: namespaced_metadata("proofstorm-workload"),
        rules: Some(vec![]),
    };
    let role_binding = RoleBinding {
        metadata: namespaced_metadata("proofstorm-workload"),
        role_ref: RoleRef {
            api_group: "rbac.authorization.k8s.io".to_owned(),
            kind: "Role".to_owned(),
            name: "proofstorm-workload".to_owned(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_owned(),
            name: "proofstorm-workload".to_owned(),
            namespace: Some(namespace_name),
            api_group: None,
        }]),
    };

    RenderedSecuritySpine {
        namespace,
        quota,
        limits,
        default_deny,
        service_account,
        role,
        role_binding,
    }
}

fn restricted_namespace(instance_key: &str, namespace_name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(namespace_name.to_owned()),
            labels: Some(BTreeMap::from([
                (INSTANCE_LABEL.to_owned(), instance_key.to_owned()),
                (MANAGED_BY_LABEL.to_owned(), "proofstormd".to_owned()),
                (
                    "pod-security.kubernetes.io/enforce".to_owned(),
                    "restricted".to_owned(),
                ),
                (
                    "pod-security.kubernetes.io/enforce-version".to_owned(),
                    "latest".to_owned(),
                ),
                (
                    "pod-security.kubernetes.io/audit".to_owned(),
                    "restricted".to_owned(),
                ),
                (
                    "pod-security.kubernetes.io/warn".to_owned(),
                    "restricted".to_owned(),
                ),
            ])),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

fn instance_quota(metadata: ObjectMeta) -> ResourceQuota {
    ResourceQuota {
        metadata,
        spec: Some(ResourceQuotaSpec {
            hard: Some(BTreeMap::from([
                ("requests.cpu".to_owned(), Quantity("2".to_owned())),
                ("requests.memory".to_owned(), Quantity("4Gi".to_owned())),
                ("limits.cpu".to_owned(), Quantity("8".to_owned())),
                ("limits.memory".to_owned(), Quantity("8Gi".to_owned())),
                ("pods".to_owned(), Quantity("40".to_owned())),
                (
                    "persistentvolumeclaims".to_owned(),
                    Quantity("12".to_owned()),
                ),
            ])),
            scopes: None,
            scope_selector: None,
        }),
        ..ResourceQuota::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_identity_is_stable_across_reconciliation() {
        assert_eq!(
            instance_namespace("01abcxyz7890"),
            "proofstorm-01abcxyz7890"
        );
        assert_eq!(
            render_security_spine("01abcxyz7890")
                .namespace
                .metadata
                .name
                .as_deref(),
            Some("proofstorm-01abcxyz7890")
        );
    }

    #[test]
    fn rendered_namespace_enforces_restricted_pod_security() {
        let rendered = render_security_spine("01abcxyz7890");
        let labels = rendered.namespace.metadata.labels.expect("labels");
        assert_eq!(
            labels
                .get("pod-security.kubernetes.io/enforce")
                .map(String::as_str),
            Some("restricted")
        );
        assert_eq!(
            rendered.service_account.automount_service_account_token,
            Some(false)
        );
        assert!(rendered.role.rules.expect("rules").is_empty());
    }

    #[test]
    fn rendered_network_policy_denies_both_directions() {
        let rendered = render_security_spine("01abcxyz7890");
        let spec = rendered.default_deny.spec.expect("network policy spec");
        assert_eq!(
            spec.policy_types,
            Some(vec!["Ingress".into(), "Egress".into()])
        );
        assert_eq!(spec.ingress, Some(vec![]));
        assert_eq!(spec.egress, Some(vec![]));
    }
}
