use std::collections::BTreeMap;

use k8s_openapi::{
    api::{
        core::v1::{
            LimitRange, LimitRangeItem, LimitRangeSpec, Namespace, ResourceQuota,
            ResourceQuotaSpec, ServiceAccount,
        },
        networking::v1::{NetworkPolicy, NetworkPolicySpec},
    },
    apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::ObjectMeta},
};
use proofstorm_core::{CellQuota, ResourcePolicy};

pub const INSTANCE_LABEL: &str = "proofstorm.dev/instance";
const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";

#[derive(Debug, Clone)]
pub struct RenderedSecuritySpine {
    pub namespace: Namespace,
    /// Present only when the cell sets `policy.resources.quota`.
    pub quota: Option<ResourceQuota>,
    pub limits: LimitRange,
    pub default_deny: NetworkPolicy,
    pub service_account: ServiceAccount,
}

#[must_use]
pub fn instance_namespace(instance_key: &str) -> String {
    format!("proofstorm-{instance_key}")
}

/// The namespace, its default requests, an optional quota, deny-all networking
/// and the workload service account. Container limits are rendered per container.
#[must_use]
pub fn render_security_spine(instance_key: &str, policy: &ResourcePolicy) -> RenderedSecuritySpine {
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

    let quota = policy
        .quota
        .as_ref()
        .map(|quota| instance_quota(namespaced_metadata("proofstorm-instance-quota"), quota));
    let defaults = policy.container_defaults();

    let limits = LimitRange {
        metadata: namespaced_metadata("proofstorm-container-limits"),
        spec: Some(LimitRangeSpec {
            limits: vec![LimitRangeItem {
                type_: "Container".to_owned(),
                // Requests only: a default limit here would bind containers that
                // set `none`. Rendered containers carry their limits explicitly.
                default_request: Some(
                    [
                        ("cpu", defaults.cpu_request),
                        ("memory", defaults.memory_request),
                    ]
                    .into_iter()
                    .filter_map(|(key, value)| value.map(|v| (key.to_owned(), Quantity(v))))
                    .collect(),
                ),
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
    RenderedSecuritySpine {
        namespace,
        quota,
        limits,
        default_deny,
        service_account,
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

/// The user's cell totals. Requests only: each container's own limit caps its
/// usage, and a sum of idle limits would refuse cells the plan accepted.
fn instance_quota(metadata: ObjectMeta, quota: &CellQuota) -> ResourceQuota {
    let hard = [
        ("requests.cpu", quota.cpu.clone()),
        ("requests.memory", quota.memory.clone()),
        ("pods", quota.pods.map(|count| count.to_string())),
        (
            "persistentvolumeclaims",
            quota.volumes.map(|count| count.to_string()),
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (key.to_owned(), Quantity(value))))
    .collect();
    ResourceQuota {
        metadata,
        spec: Some(ResourceQuotaSpec {
            hard: Some(hard),
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
            render_security_spine("01abcxyz7890", &ResourcePolicy::default())
                .namespace
                .metadata
                .name
                .as_deref(),
            Some("proofstorm-01abcxyz7890")
        );
    }

    #[test]
    fn rendered_namespace_enforces_restricted_pod_security() {
        let rendered = render_security_spine("01abcxyz7890", &ResourcePolicy::default());
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
    }

    #[test]
    fn rendered_network_policy_denies_both_directions() {
        let rendered = render_security_spine("01abcxyz7890", &ResourcePolicy::default());
        let spec = rendered.default_deny.spec.expect("network policy spec");
        assert_eq!(
            spec.policy_types,
            Some(vec!["Ingress".into(), "Egress".into()])
        );
        assert_eq!(spec.ingress, Some(vec![]));
        assert_eq!(spec.egress, Some(vec![]));
    }

    #[test]
    fn a_cell_has_no_quota_and_no_default_limits_unless_it_sets_them() {
        let rendered = render_security_spine("01abcxyz7890", &ResourcePolicy::default());
        assert!(rendered.quota.is_none());
        let item = &rendered.limits.spec.expect("limit range").limits[0];
        assert!(item.default.is_none());
        assert_eq!(
            item.default_request,
            Some(BTreeMap::from([
                ("cpu".to_owned(), Quantity("100m".to_owned())),
                ("memory".to_owned(), Quantity("128Mi".to_owned())),
            ]))
        );
    }

    #[test]
    fn a_user_quota_bounds_reservations_not_container_limits() {
        let policy: ResourcePolicy = serde_json::from_value(serde_json::json!({
            "quota": {"cpu": "20", "memory": "64Gi", "pods": 500}
        }))
        .expect("policy");
        let hard = render_security_spine("01abcxyz7890", &policy)
            .quota
            .expect("quota")
            .spec
            .expect("quota spec")
            .hard
            .expect("hard quota");
        assert_eq!(
            hard,
            BTreeMap::from([
                ("pods".to_owned(), Quantity("500".to_owned())),
                ("requests.cpu".to_owned(), Quantity("20".to_owned())),
                ("requests.memory".to_owned(), Quantity("64Gi".to_owned())),
            ])
        );
    }
}
