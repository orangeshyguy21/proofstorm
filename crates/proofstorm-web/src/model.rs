//! Pure presentation helpers, also checked by native tests.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use proofstorm_core::ComponentKind;
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
pub fn position(components: &[ComponentView], id: &str) -> (i32, i32) {
    let Some(component) = components.iter().find(|c| c.id == id) else {
        return (0, 0);
    };
    let group = column(component.kind);
    let row = components
        .iter()
        .filter(|c| group == column(c.kind) && c.id.as_str() < id)
        .count();
    let previous_columns: usize = (0..group)
        .map(|group| {
            components
                .iter()
                .filter(|c| column(c.kind) == group)
                .count()
                .div_ceil(4)
                .max(1)
        })
        .sum();
    (
        40 + i32::try_from(previous_columns + row / 4).unwrap_or(0) * 292,
        40 + i32::try_from(row % 4).unwrap_or(0) * 170,
    )
}

fn column(kind: ComponentKind) -> i32 {
    match kind {
        ComponentKind::Bitcoin | ComponentKind::Database | ComponentKind::IdentityProvider => 0,
        ComponentKind::Lightning | ComponentKind::Proxy => 1,
        ComponentKind::Mint | ComponentKind::Oracle => 2,
        ComponentKind::Wallet | ComponentKind::Attacker => 3,
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
pub fn elapsed_time(timestamp: i64, now: i64) -> String {
    let seconds = now.saturating_sub(timestamp).max(0);
    if seconds == 0 {
        return "just now".into();
    }
    let (amount, unit) = if seconds < 60 {
        (seconds, "second")
    } else if seconds < 3600 {
        (seconds / 60, "minute")
    } else if seconds < 86400 {
        (seconds / 3600, "hour")
    } else {
        (seconds / 86400, "day")
    };
    format!("{amount} {unit}{} ago", if amount == 1 { "" } else { "s" })
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
    use super::*;
    #[test]
    fn height_tracks_the_current_lab_and_can_decrease() {
        let observation = |height, error| proofstorm_view::ComponentBalance {
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
    fn large_layouts_wrap_each_kind_without_overlapping_tiles() {
        let nodes = (0..10)
            .map(|i| ComponentView {
                id: format!("node-{i}"),
                kind: ComponentKind::Lightning,
                implementation: "lnd".into(),
                version: None,
                ready: None,
                conditions: vec![],
                endpoints: vec![],
            })
            .collect::<Vec<_>>();
        let positions = nodes
            .iter()
            .map(|n| position(&nodes, &n.id))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(positions.len(), 10);
        assert!(positions.iter().all(|(_, y)| *y <= 550));
    }
    #[test]
    fn merges_component_pages_without_duplicating_shared_demands() {
        let resource = || ResourceDemand {
            retained_storage: std::collections::BTreeMap::new(),
            workloads: vec![proofstorm_view::WorkloadDemand {
                name: "shared".into(),
                component: None,
                replicas: 1,
                containers: vec![],
            }],
            storage: vec![],
        };
        let mut result = Some(resource());
        merge_resources(&mut result, Some(resource()));
        assert_eq!(result.unwrap().workloads.len(), 1);
    }
    #[test]
    fn layout_is_stable_when_input_order_changes() {
        let node = |id: &str| ComponentView {
            id: id.into(),
            kind: ComponentKind::Bitcoin,
            implementation: "bitcoind".into(),
            version: None,
            ready: None,
            conditions: vec![],
            endpoints: vec![],
        };
        let nodes = vec![node("b"), node("a")];
        assert_eq!(position(&nodes, "a"), (40, 40));
        assert_eq!(
            position(&[nodes[1].clone(), nodes[0].clone()], "b"),
            (40, 210)
        );
    }
}
