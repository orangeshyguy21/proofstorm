//! Pure presentation helpers, also checked by native tests.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use proofstorm_view::{ComponentView, EnvironmentLab, ResourceDemand};

pub fn label(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
        .replace('_', " ")
}
pub fn lab_name(lab: &EnvironmentLab) -> String {
    lab.handle
        .as_ref()
        .map_or_else(|| lab.id.clone(), |h| h.name.clone())
}
pub fn closed(lab: &EnvironmentLab) -> bool {
    lab.handle
        .as_ref()
        .is_some_and(|h| h.phase == proofstorm_view::LabHandlePhase::Closed)
}
pub fn lab_phase(lab: &EnvironmentLab) -> String {
    if lab.read_error.is_some() {
        "history unavailable".into()
    } else if closed(lab) {
        "closed".into()
    } else {
        lab.runtime
            .phase
            .as_ref()
            .map_or_else(|| label(&lab.runtime.state), label)
    }
}
pub fn health(component: &ComponentView) -> &'static str {
    if component.ready == Some(false)
        && component
            .conditions
            .iter()
            .any(|c| c.reason.blocks_startup())
    {
        return "blocked";
    }
    match component.ready {
        Some(true) => "ready",
        Some(false) => "pending",
        None => "unknown",
    }
}
pub fn merge_resources(target: &mut Option<ResourceDemand>, page: Option<ResourceDemand>) {
    if let Some(page) = page {
        let target = target.get_or_insert_with(|| ResourceDemand {
            retained_storage: std::collections::BTreeMap::new(),
            workloads: vec![],
            storage: vec![],
        });
        target.retained_storage.extend(page.retained_storage);
        for workload in page.workloads {
            if !target.workloads.iter().any(|w| w.name == workload.name) {
                target.workloads.push(workload);
            }
        }
        for storage in page.storage {
            if !target
                .storage
                .iter()
                .any(|s| s.name == storage.name && s.workload == storage.workload)
            {
                target.storage.push(storage);
            }
        }
    }
}

pub fn cpu(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or_else(
            || "—".into(),
            |value| format!("{} cores", cpu_amount(value)),
        )
}
pub fn cpu_amount(millicores: f64) -> String {
    if millicores > 0.0 && millicores < 1.0 {
        "<0.001".into()
    } else if millicores > 0.0 && millicores < 100.0 {
        format!("{:.3}", millicores / 1000.0)
    } else {
        format!("{:.2}", millicores / 1000.0)
    }
}
pub fn cpu_quantity(value: &str) -> String {
    let (number, factor) = [
        ("n", 1e-6),
        ("u", 1e-3),
        ("m", 1.0),
        ("k", 1e6),
        ("K", 1e6),
        ("M", 1e9),
        ("G", 1e12),
        ("T", 1e15),
        ("P", 1e18),
        ("E", 1e21),
    ]
    .into_iter()
    .find_map(|(suffix, factor)| value.strip_suffix(suffix).map(|number| (number, factor)))
    .unwrap_or((value, 1000.0));
    cpu(number.parse::<f64>().ok().map(|number| number * factor))
}
pub const OBSERVATION_MAX_AGE: i64 = 20;
// Kubernetes metrics refresh less often than the lab observations.
pub const METRICS_MAX_AGE: i64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Live,
    Delayed,
    Disconnected,
    Unavailable,
}
impl Freshness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Delayed => "Delayed",
            Self::Disconnected => "Disconnected",
            Self::Unavailable => "Unavailable",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Live => "freshness-live",
            Self::Delayed => "freshness-delayed",
            Self::Disconnected => "freshness-disconnected",
            Self::Unavailable => "freshness-unavailable",
        }
    }
}
pub fn observation_freshness(
    timestamp: i64,
    now: i64,
    failed: bool,
    connected: bool,
    max_age: i64,
) -> Freshness {
    if !connected {
        Freshness::Disconnected
    } else if timestamp <= 0 {
        Freshness::Unavailable
    } else if failed || now.saturating_sub(timestamp) > max_age {
        Freshness::Delayed
    } else {
        Freshness::Live
    }
}
pub fn memory(value: Option<f64>) -> String {
    value.map_or_else(
        || "—".into(),
        |value| {
            if value >= 1_073_741_824.0 {
                format!("{:.2} GiB", value / 1_073_741_824.0)
            } else {
                format!("{:.1} MiB", value / 1_048_576.0)
            }
        },
    )
}
pub fn sat(value: u64) -> String {
    let raw = value.to_string();
    raw.chars()
        .enumerate()
        .fold(String::new(), |mut result, (i, c)| {
            if i > 0 && (raw.len() - i).is_multiple_of(3) {
                result.push(',');
            }
            result.push(c);
            result
        })
}

pub fn block_height(lab: &proofstorm_view::LabUsage) -> Option<u64> {
    lab.balances
        .iter()
        .filter(|b| b.error.is_none())
        .filter_map(|b| b.block_height)
        .max()
}

pub fn process_group(process: &proofstorm_view::ProcessUsage) -> String {
    process.component.clone().unwrap_or_else(|| {
        if process.container.starts_with("probe-")
            || process.pod.starts_with("proofstorm-protocol-prober")
        {
            "Probes".into()
        } else if process.pod.starts_with("op-") {
            "Action jobs".into()
        } else {
            "Shared services".into()
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn freshness_changes_only_at_status_boundaries() {
        for age in 0..=super::OBSERVATION_MAX_AGE {
            assert_eq!(
                super::observation_freshness(
                    100,
                    100 + age,
                    false,
                    true,
                    super::OBSERVATION_MAX_AGE
                ),
                super::Freshness::Live
            );
        }
        assert_eq!(
            super::observation_freshness(100, 121, false, true, 20),
            super::Freshness::Delayed
        );
        assert_eq!(
            super::observation_freshness(121, 121, false, true, 20),
            super::Freshness::Live
        );
        assert_eq!(
            super::observation_freshness(130, 121, false, true, 20),
            super::Freshness::Live
        );
    }
    #[test]
    fn failed_and_missing_observations_cannot_report_live() {
        assert_eq!(
            super::observation_freshness(100, 100, true, true, 20),
            super::Freshness::Delayed
        );
        assert_eq!(
            super::observation_freshness(0, 100, false, true, 20),
            super::Freshness::Unavailable
        );
        assert_eq!(
            super::observation_freshness(0, 100, true, true, 20),
            super::Freshness::Unavailable
        );
        assert_eq!(
            super::observation_freshness(100, 100, false, false, 20),
            super::Freshness::Disconnected
        );
        assert_eq!(
            super::observation_freshness(100, 100, true, false, 20),
            super::Freshness::Disconnected
        );
    }
    #[test]
    fn process_metrics_allow_for_their_slower_sampling_cycle() {
        assert_eq!(
            super::observation_freshness(100, 160, false, true, super::METRICS_MAX_AGE),
            super::Freshness::Live
        );
        assert_eq!(
            super::observation_freshness(100, 161, false, true, super::METRICS_MAX_AGE),
            super::Freshness::Delayed
        );
    }

    use super::*;
    #[test]
    fn height_tracks_the_current_lab_and_can_decrease() {
        let observation = |height, error| proofstorm_view::ComponentBalance {
            rollout_digest: None,
            lightning: None,
            holdings: None,
            component: "chain".into(),
            observed_at_unix: 1,
            error,
            amounts: vec![],
            block_height: height,
        };
        let mut lab = proofstorm_view::LabUsage {
            balances: vec![
                observation(Some(12), None),
                observation(Some(15), None),
                observation(Some(99), Some("stale".into())),
            ],
            ..Default::default()
        };
        assert_eq!(block_height(&lab), Some(15));
        lab.balances = vec![observation(Some(0), None)];
        assert_eq!(block_height(&lab), Some(0));
        lab.balances.clear();
        assert_eq!(block_height(&lab), None);
    }
    #[test]
    fn merges_component_pages_without_duplicating_shared_demands() {
        let resource = || ResourceDemand {
            retained_storage: std::collections::BTreeMap::new(),
            workloads: vec![proofstorm_view::WorkloadDemand {
                name: "shared".into(),
                component: None,
                kind: "Deployment".into(),
                replicas: Some(1),
                replica_policy: proofstorm_view::ReplicaPolicy::Fixed,
                observation: None,
                containers: vec![],
            }],
            storage: vec![],
        };
        let mut result = Some(resource());
        merge_resources(&mut result, Some(resource()));
        assert_eq!(result.unwrap().workloads.len(), 1);
    }
}
