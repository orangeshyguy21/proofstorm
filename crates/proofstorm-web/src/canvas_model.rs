//! Stable canvas identities and geometry, independent of browser rendering.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use proofstorm_core::ComponentKind;
use proofstorm_view::{ComponentView, EnvironmentLab};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const WIDTH: f64 = 260.0;
pub const HEIGHT: f64 = 144.0;
pub type Positions = BTreeMap<String, (f64, f64)>;
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Layout {
    pub positions: Positions,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasNode {
    pub id: String,
    pub owner: String,
    pub parent: Option<String>,
    pub name: String,
    pub implementation: String,
    pub kind: ComponentKind,
    pub embedded_count: usize,
}
pub fn embedded_id(parent: &str, resource: &str) -> String {
    format!(
        "embedded:{}",
        serde_json::to_string(&(parent, resource)).unwrap_or_default()
    )
}
pub fn nodes(lab: &EnvironmentLab) -> Vec<CanvasNode> {
    lab.components
        .items
        .iter()
        .flat_map(|component| {
            let embedded = component
                .details
                .as_ref()
                .map(|d| d.embedded.as_slice())
                .unwrap_or_default();
            let mut result = vec![CanvasNode {
                id: component.id.clone(),
                owner: component.id.clone(),
                parent: None,
                name: component.id.clone(),
                implementation: component.implementation.clone(),
                kind: component.kind,
                embedded_count: embedded.len(),
            }];
            result.extend(embedded.iter().map(|resource| CanvasNode {
                id: embedded_id(&component.id, &resource.id),
                owner: component.id.clone(),
                parent: Some(component.id.clone()),
                name: resource.name.clone(),
                implementation: resource.id.clone(),
                kind: resource.kind,
                embedded_count: 0,
            }));
            result
        })
        .collect()
}
pub fn selected_owner<'a>(lab: &'a EnvironmentLab, id: &str) -> Option<&'a ComponentView> {
    lab.components.items.iter().find(|c| {
        c.id == id
            || c.details
                .as_ref()
                .is_some_and(|d| d.embedded.iter().any(|e| embedded_id(&c.id, &e.id) == id))
    })
}
pub fn group_height(node: &CanvasNode) -> f64 {
    HEIGHT
        + f64::from(u32::try_from(node.embedded_count).unwrap_or(0)) * 108.0
        + if node.embedded_count > 0 { 20.0 } else { 0.0 }
}
pub fn world_position(node: &CanvasNode, positions: &Positions) -> (f64, f64) {
    let local = positions.get(&node.id).copied().unwrap_or_default();
    node.parent.as_ref().map_or(local, |parent| {
        let parent = positions.get(parent).copied().unwrap_or_default();
        (parent.0 + local.0, parent.1 + local.1)
    })
}
pub fn ensure_positions(nodes: &[CanvasNode], positions: &mut Positions) {
    positions
        .retain(|_, p| p.0.is_finite() && p.1.is_finite() && p.0.abs() < 1e7 && p.1.abs() < 1e7);
    for node in nodes.iter().filter(|n| n.parent.is_none()) {
        if positions.contains_key(&node.id) {
            continue;
        }
        let column = match node.kind {
            ComponentKind::Bitcoin | ComponentKind::IdentityProvider | ComponentKind::Database => 0,
            ComponentKind::Lightning | ComponentKind::Proxy => 1,
            ComponentKind::Mint | ComponentKind::Oracle => 2,
            ComponentKind::Wallet | ComponentKind::Attacker => 3,
        };
        let x = 40.0 + f64::from(column) * 340.0;
        let mut y = 40.0;
        while nodes.iter().filter(|n| n.parent.is_none()).any(|other| {
            positions.get(&other.id).is_some_and(|p| {
                (p.0 - x).abs() < WIDTH + 48.0
                    && y < p.1 + group_height(other) + 44.0
                    && y + group_height(node) + 44.0 > p.1
            })
        }) {
            y += 60.0;
        }
        positions.insert(node.id.clone(), (x, y));
    }
    let mut counts = BTreeMap::<String, u32>::new();
    for node in nodes.iter().filter(|n| n.parent.is_some()) {
        let parent = node.parent.as_ref().unwrap();
        let index = counts.entry(parent.clone()).or_default();
        positions
            .entry(node.id.clone())
            .or_insert((14.0, 164.0 + f64::from(*index) * 108.0));
        *index += 1;
    }
}
pub fn move_node(
    node: &CanvasNode,
    nodes: &[CanvasNode],
    positions: &mut Positions,
    delta: (f64, f64),
) {
    let previous = positions.get(&node.id).copied().unwrap_or_default();
    let mut next = (previous.0 + delta.0, previous.1 + delta.1);
    if let Some(parent) = node
        .parent
        .as_ref()
        .and_then(|id| nodes.iter().find(|n| &n.id == id))
    {
        next.0 = next.0.clamp(0.0, 28.0);
        next.1 = next.1.clamp(154.0, group_height(parent) - 88.0);
    }
    positions.insert(node.id.clone(), next);
}
pub fn bounds(nodes: &[CanvasNode], positions: &Positions) -> (f64, f64, f64, f64) {
    let roots = nodes
        .iter()
        .filter(|n| n.parent.is_none())
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return (0.0, 0.0, 900.0, 500.0);
    }
    let x = roots
        .iter()
        .map(|n| world_position(n, positions).0 - 36.0)
        .fold(f64::INFINITY, f64::min);
    let y = roots
        .iter()
        .map(|n| world_position(n, positions).1 - 36.0)
        .fold(f64::INFINITY, f64::min);
    let right = roots
        .iter()
        .map(|n| world_position(n, positions).0 + WIDTH + 36.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = roots
        .iter()
        .map(|n| world_position(n, positions).1 + group_height(n) + 36.0)
        .fold(f64::NEG_INFINITY, f64::max);
    (x, y, (right - x).max(500.0), (bottom - y).max(320.0))
}
pub fn appearance(kind: ComponentKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        ComponentKind::Bitcoin => (
            "bitcoin",
            "Bitcoin",
            "M4 7 12 3 20 7 12 11Z M4 7v10l8 4 8-4V7 M12 11v10",
        ),
        ComponentKind::Lightning => ("lightning", "Lightning", "m13 2-9 12h7l-1 8 10-12h-7Z"),
        ComponentKind::Mint => (
            "mint",
            "Mint",
            "M4 6c0-4 16-4 16 0s-16 4-16 0v6c0 4 16 4 16 0V6 M4 12v6c0 4 16 4 16 0v-6",
        ),
        ComponentKind::Wallet => (
            "wallet",
            "Wallet",
            "M3 6h18v15H3Z M3 6V3h15v3 M15 11h6v5h-6Z",
        ),
        ComponentKind::IdentityProvider => (
            "identity",
            "Identity / OIDC",
            "M12 2 3 6v6c0 5 9 10 9 10s9-5 9-10V6Z m-4 10 3 3 5-6",
        ),
        ComponentKind::Database => (
            "database",
            "Database",
            "M4 5c0-4 16-4 16 0s-16 4-16 0v14c0 4 16 4 16 0V5 M4 12c0 4 16 4 16 0",
        ),
        ComponentKind::Proxy => (
            "proxy",
            "Proxy",
            "M2 7h18m-4-4 4 4-4 4 M22 17H4m4-4-4 4 4 4",
        ),
        ComponentKind::Oracle => (
            "oracle",
            "Oracle",
            "M2 12s4-8 10-8 10 8 10 8-4 8-10 8S2 12 2 12Z M9 12a3 3 0 1 0 6 0a3 3 0 1 0-6 0",
        ),
        ComponentKind::Attacker => ("attacker", "Attacker", "m12 2 10 19H2Z M12 8v6 M12 17v1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, kind: ComponentKind) -> CanvasNode {
        CanvasNode {
            id: id.into(),
            owner: id.into(),
            parent: None,
            name: id.into(),
            implementation: id.into(),
            kind,
            embedded_count: 0,
        }
    }
    #[test]
    fn saved_positions_survive_reordering_and_new_components() {
        let mut items = vec![
            node("chain", ComponentKind::Bitcoin),
            node("oidc", ComponentKind::IdentityProvider),
        ];
        let mut positions = Positions::new();
        ensure_positions(&items, &mut positions);
        move_node(&items[1], &items, &mut positions, (-400.0, 180.0));
        let before = positions.clone();
        let encoded = serde_json::to_string(&Layout { positions }).unwrap();
        let mut restored = serde_json::from_str::<Layout>(&encoded).unwrap().positions;
        items.reverse();
        items.push(node("database", ComponentKind::Database));
        ensure_positions(&items, &mut restored);
        assert_eq!(restored["oidc"], before["oidc"]);
        assert_eq!(restored["chain"], before["chain"]);
        assert_eq!(restored.len(), 3);
        let b = bounds(&items, &restored);
        assert!(b.0 < 0.0);
    }
    #[test]
    fn parent_moves_children_and_embedded_moves_remain_inside_group() {
        let mut parent = node("mint", ComponentKind::Mint);
        parent.embedded_count = 1;
        let mut child = node(&embedded_id("mint", "ldk-node"), ComponentKind::Lightning);
        child.parent = Some("mint".into());
        let items = vec![parent, child];
        let mut p = Positions::new();
        ensure_positions(&items, &mut p);
        let before = world_position(&items[1], &p);
        move_node(&items[0], &items, &mut p, (100.0, -50.0));
        assert_eq!(
            world_position(&items[1], &p),
            (before.0 + 100.0, before.1 - 50.0)
        );
        move_node(&items[1], &items, &mut p, (10000.0, -10000.0));
        assert_eq!(p[&items[1].id], (28.0, 154.0));
        assert_ne!(embedded_id("a::b", "c"), embedded_id("a", "b::c"));
    }
    #[test]
    fn invalid_saved_coordinates_are_replaced() {
        let items = vec![node("oidc", ComponentKind::IdentityProvider)];
        let mut p = Positions::from([("oidc".into(), (f64::NAN, 0.0))]);
        ensure_positions(&items, &mut p);
        assert!(p["oidc"].0.is_finite());
        assert_eq!(appearance(items[0].kind).1, "Identity / OIDC");
    }
}
