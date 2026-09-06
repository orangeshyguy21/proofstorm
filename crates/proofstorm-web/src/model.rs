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
    if closed(lab) {
        "closed".into()
    } else {
        lab.runtime
            .phase
            .as_ref()
            .map_or_else(|| label(&lab.runtime.state), label)
    }
}
pub fn archived(lab: &EnvironmentLab) -> bool {
    lab.handle
        .as_ref()
        .is_some_and(|h| h.phase == proofstorm_view::LabHandlePhase::Closed)
        || matches!(
            lab.runtime.state,
            proofstorm_view::ObservationState::Missing
        )
}
pub fn health(component: &ComponentView) -> &'static str {
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
    let column = column(component.kind);
    let row = components
        .iter()
        .filter(|c| column == self::column(c.kind) && c.id.as_str() < id)
        .count();
    (
        55 + column * 265,
        65 + i32::try_from(row).unwrap_or(0) * 125,
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
            workloads: vec![],
            storage: vec![],
        });
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges_component_pages_without_duplicating_shared_demands() {
        let resource = || ResourceDemand {
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
        assert_eq!(position(&nodes, "a"), (55, 65));
        assert_eq!(
            position(&[nodes[1].clone(), nodes[0].clone()], "b"),
            (55, 190)
        );
    }
}
