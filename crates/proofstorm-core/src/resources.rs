//! User-owned CPU and memory caps.
//!
//! A container's resources come from the first level that sets each value:
//! the component's own `resources` (main container only), then the adapter's
//! built-in values, then the cell's `policy.resources.defaults`, then
//! Proofstorm's defaults. A limit of `none` removes that limit. Proofstorm sets
//! no CPU limit by default: a cap only slightly below real use throttled the
//! controller in every scheduling period and made command starts about ten
//! times slower.
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::validation::{ValidationIssue, issue};

/// The value that removes a limit.
pub const NO_LIMIT: &str = "none";
/// Proofstorm's CPU request for a container that sets none.
pub const DEFAULT_CPU_REQUEST: &str = "100m";
/// Proofstorm's memory request for a container that sets none.
pub const DEFAULT_MEMORY_REQUEST: &str = "128Mi";
/// Proofstorm's memory limit for a container that sets none. Memory stays
/// capped by default because running out takes the whole host down.
pub const DEFAULT_MEMORY_LIMIT: &str = "512Mi";

// Agent tool schemas carry these types several times, inside a fixed discovery
// budget, so their schemas are inline and terse. Validation gives full errors.
fn amount_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "object", "additionalProperties": false,
        "properties": {"request": {"type": "string"}, "limit": {"type": "string"}}})
}

fn quantity_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "string"})
}

fn count_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "integer", "minimum": 1})
}

// A reserved amount and an upper bound for one resource.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct ResourceAmount {
    /// Reserved, e.g. `100m`, `128Mi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// Cap, e.g. `2`, `1Gi`; `none` removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
}

// CPU and memory for a container. Omitted values come from the next level.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "amount_schema")]
    pub cpu: Option<ResourceAmount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "amount_schema")]
    pub memory: Option<ResourceAmount>,
}

impl Resources {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

// Totals every pod in the cell may reserve together.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct CellQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "quantity_schema")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "quantity_schema")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "count_schema")]
    pub pods: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "count_schema")]
    pub volumes: Option<u64>,
}

// The cell's resource policy. Everything is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct ResourcePolicy {
    #[serde(default, skip_serializing_if = "Resources::is_empty")]
    pub defaults: Resources,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<CellQuota>,
}

impl ResourcePolicy {
    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Proofstorm's defaults overlaid by the cell's.
    #[must_use]
    pub fn container_defaults(&self) -> Effective {
        let builtin = Effective {
            cpu_request: Some(DEFAULT_CPU_REQUEST.into()),
            cpu_limit: None,
            memory_request: Some(DEFAULT_MEMORY_REQUEST.into()),
            memory_limit: Some(DEFAULT_MEMORY_LIMIT.into()),
        };
        builtin.overlaid(&self.defaults)
    }
}

/// What one container ends up with. `None` means the value is absent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effective {
    pub cpu_request: Option<String>,
    pub cpu_limit: Option<String>,
    pub memory_request: Option<String>,
    pub memory_limit: Option<String>,
}

impl Effective {
    /// Values `resources` sets replace these; `none` removes a limit.
    #[must_use]
    pub fn overlaid(mut self, resources: &Resources) -> Self {
        for (amount, request, limit) in [
            (&resources.cpu, &mut self.cpu_request, &mut self.cpu_limit),
            (
                &resources.memory,
                &mut self.memory_request,
                &mut self.memory_limit,
            ),
        ] {
            let Some(amount) = amount else { continue };
            if let Some(value) = &amount.request {
                *request = Some(value.clone());
            }
            if let Some(value) = &amount.limit {
                *limit = (value != NO_LIMIT).then(|| value.clone());
            }
        }
        self
    }

    /// Fill each absent value from `fallback`.
    #[must_use]
    pub fn or(self, fallback: &Self) -> Self {
        Self {
            cpu_request: self.cpu_request.or_else(|| fallback.cpu_request.clone()),
            cpu_limit: self.cpu_limit.or_else(|| fallback.cpu_limit.clone()),
            memory_request: self
                .memory_request
                .or_else(|| fallback.memory_request.clone()),
            memory_limit: self.memory_limit.or_else(|| fallback.memory_limit.clone()),
        }
    }

    /// Kubernetes rejects a request above its limit, so lower it to the limit.
    #[must_use]
    pub fn requests_within_limits(mut self) -> Self {
        for (request, limit) in [
            (&mut self.cpu_request, &self.cpu_limit),
            (&mut self.memory_request, &self.memory_limit),
        ] {
            if let (Some(wanted), Some(cap)) = (request.as_ref(), limit.as_ref())
                && quantity_value(wanted)
                    .zip(quantity_value(cap))
                    .is_some_and(|(wanted, cap)| wanted > cap)
            {
                *request = Some(cap.clone());
            }
        }
        self
    }
}

/// The value of a Kubernetes quantity such as `250m`, `2`, `0.5`, `128Mi` or `1G`.
#[must_use]
pub fn quantity_value(text: &str) -> Option<f64> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    if number.is_empty() || number.starts_with('.') || number.ends_with('.') {
        return None;
    }
    let number: f64 = number.parse().ok()?;
    let scale = match suffix {
        "" => 1.0,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024.0_f64.powi(4),
        "Pi" => 1024.0_f64.powi(5),
        "Ei" => 1024.0_f64.powi(6),
        _ => return None,
    };
    Some(number * scale)
}

fn check_amount(amount: Option<&ResourceAmount>, path: &str, issues: &mut Vec<ValidationIssue>) {
    let Some(amount) = amount else { return };
    let request = amount.request.as_deref();
    let limit = amount.limit.as_deref().filter(|limit| *limit != NO_LIMIT);
    if request == Some(NO_LIMIT) {
        issue(
            issues,
            "invalid_resource_quantity",
            format!("{path}/request"),
            "only a limit can be none; omit the request to use the default",
        );
    }
    for (field, value) in [
        ("request", request.filter(|request| *request != NO_LIMIT)),
        ("limit", limit),
    ] {
        if let Some(value) = value
            && !quantity_value(value).is_some_and(|value| value > 0.0)
        {
            issue(
                issues,
                "invalid_resource_quantity",
                format!("{path}/{field}"),
                "must be a positive Kubernetes quantity such as 250m, 2, 128Mi or 1Gi",
            );
        }
    }
    if let (Some(request), Some(limit)) = (
        request.and_then(quantity_value),
        limit.and_then(quantity_value),
    ) && request > limit
    {
        issue(
            issues,
            "resource_request_above_limit",
            path,
            "request must not exceed limit",
        );
    }
}

fn check_resources(resources: &Resources, path: &str, issues: &mut Vec<ValidationIssue>) {
    check_amount(resources.cpu.as_ref(), &format!("{path}/cpu"), issues);
    check_amount(resources.memory.as_ref(), &format!("{path}/memory"), issues);
}

pub(crate) fn validate(
    policy: &ResourcePolicy,
    components: &[(usize, Option<&Resources>)],
    issues: &mut Vec<ValidationIssue>,
) {
    check_resources(&policy.defaults, "/policy/resources/defaults", issues);
    if let Some(quota) = &policy.quota {
        for (field, value) in [("cpu", &quota.cpu), ("memory", &quota.memory)] {
            if let Some(value) = value
                && !quantity_value(value).is_some_and(|value| value > 0.0)
            {
                issue(
                    issues,
                    "invalid_resource_quantity",
                    format!("/policy/resources/quota/{field}"),
                    "must be a positive Kubernetes quantity such as 20 or 64Gi",
                );
            }
        }
        for (field, value) in [("pods", quota.pods), ("volumes", quota.volumes)] {
            if value == Some(0) {
                issue(
                    issues,
                    "invalid_limit",
                    format!("/policy/resources/quota/{field}"),
                    "must be positive when supplied; omit this field for no quota on it",
                );
            }
        }
    }
    for (index, resources) in components {
        if let Some(resources) = resources {
            check_resources(resources, &format!("/components/{index}/resources"), issues);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(request: Option<&str>, limit: Option<&str>) -> ResourceAmount {
        ResourceAmount {
            request: request.map(Into::into),
            limit: limit.map(Into::into),
        }
    }

    #[test]
    fn defaults_reserve_a_little_and_cap_only_memory() {
        let defaults = ResourcePolicy::default().container_defaults();
        assert_eq!(defaults.cpu_request.as_deref(), Some("100m"));
        assert_eq!(defaults.cpu_limit, None);
        assert_eq!(defaults.memory_request.as_deref(), Some("128Mi"));
        assert_eq!(defaults.memory_limit.as_deref(), Some("512Mi"));
    }

    #[test]
    fn each_level_overrides_only_what_it_sets_and_none_removes_a_limit() {
        let policy = ResourcePolicy {
            defaults: Resources {
                cpu: Some(amount(None, Some("4"))),
                memory: Some(amount(Some("256Mi"), None)),
            },
            quota: None,
        };
        let cell = policy.container_defaults();
        assert_eq!(cell.cpu_request.as_deref(), Some("100m"));
        assert_eq!(cell.cpu_limit.as_deref(), Some("4"));
        assert_eq!(cell.memory_request.as_deref(), Some("256Mi"));
        assert_eq!(cell.memory_limit.as_deref(), Some("512Mi"));

        let component = Resources {
            cpu: Some(amount(None, Some(NO_LIMIT))),
            memory: Some(amount(None, Some("2Gi"))),
        };
        let main = cell.overlaid(&component);
        assert_eq!(main.cpu_limit, None);
        assert_eq!(main.memory_limit.as_deref(), Some("2Gi"));
        assert_eq!(main.memory_request.as_deref(), Some("256Mi"));
    }

    #[test]
    fn adapter_values_win_over_cell_defaults_only_where_they_are_set() {
        let adapter = Effective {
            memory_request: Some("512Mi".into()),
            memory_limit: Some("2Gi".into()),
            ..Effective::default()
        };
        let merged = adapter.or(&ResourcePolicy::default().container_defaults());
        assert_eq!(merged.cpu_request.as_deref(), Some("100m"));
        assert_eq!(merged.memory_request.as_deref(), Some("512Mi"));
        assert_eq!(merged.memory_limit.as_deref(), Some("2Gi"));
    }

    #[test]
    fn a_request_above_a_lowered_limit_follows_the_limit() {
        let merged = Effective {
            memory_request: Some("512Mi".into()),
            memory_limit: Some("256Mi".into()),
            cpu_request: Some("1".into()),
            cpu_limit: Some("1500m".into()),
        }
        .requests_within_limits();
        assert_eq!(merged.memory_request.as_deref(), Some("256Mi"));
        assert_eq!(merged.cpu_request.as_deref(), Some("1"));
    }

    #[test]
    fn quantities_parse_the_kubernetes_suffixes() {
        assert_eq!(quantity_value("250m"), Some(0.25));
        assert_eq!(quantity_value("2"), Some(2.0));
        assert_eq!(quantity_value("0.5"), Some(0.5));
        assert_eq!(quantity_value("128Mi"), Some(134_217_728.0));
        assert_eq!(quantity_value("1G"), Some(1e9));
        for bad in ["", "Mi", "1.", ".5", "1mi", "-1", "1 Gi", "none"] {
            assert_eq!(quantity_value(bad), None, "{bad}");
        }
    }

    #[test]
    fn validation_reports_each_bad_value_by_path() {
        let policy = ResourcePolicy {
            defaults: Resources {
                cpu: Some(amount(Some("2"), Some("1"))),
                memory: Some(amount(Some(NO_LIMIT), Some("lots"))),
            },
            quota: Some(CellQuota {
                cpu: Some("0".into()),
                pods: Some(0),
                ..CellQuota::default()
            }),
        };
        let component = Resources {
            cpu: Some(amount(None, Some(NO_LIMIT))),
            memory: Some(amount(Some("1Gi"), Some("512Mi"))),
        };
        let mut issues = Vec::new();
        validate(&policy, &[(3, Some(&component))], &mut issues);
        let found: Vec<_> = issues
            .iter()
            .map(|issue| (issue.code.as_str(), issue.path.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "resource_request_above_limit",
                    "/policy/resources/defaults/cpu"
                ),
                (
                    "invalid_resource_quantity",
                    "/policy/resources/defaults/memory/request"
                ),
                (
                    "invalid_resource_quantity",
                    "/policy/resources/defaults/memory/limit"
                ),
                ("invalid_resource_quantity", "/policy/resources/quota/cpu"),
                ("invalid_limit", "/policy/resources/quota/pods"),
                (
                    "resource_request_above_limit",
                    "/components/3/resources/memory"
                ),
            ]
        );
    }
}
