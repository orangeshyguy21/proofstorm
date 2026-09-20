use super::*;
use crate::{API_VERSION, CellLimits, ComponentKind, ControlClass, LinkKind, validate_cell};
use std::collections::BTreeMap;

fn component(id: &str) -> ComponentSpec {
    ComponentSpec {
        id: id.into(),
        kind: ComponentKind::Bitcoin,
        implementation: "bitcoin-core".into(),
        version: Some("31.1".into()),
        config_version: "bitcoin-core/31/v1".into(),
        control: ControlClass::Cell,
        config: BTreeMap::new(),
    }
}

fn link(id: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: id.into(),
        kind: LinkKind::BitcoinPeer,
        from: "a".into(),
        to: to.into(),
        binding: None,
    }
}

fn cell() -> CellSpec {
    CellSpec {
        api_version: API_VERSION.into(),
        name: "patched-cell".into(),
        components: vec![component("z"), component("a")],
        links: vec![link("old", "z")],
        policy: CellPolicy::default(),
    }
}

#[test]
fn ordered_batch_allows_incomplete_intermediate_topology_and_sorts_the_result() {
    let mut updated = component("a");
    updated.config.insert("txindex".into(), false.into());
    let policy = CellPolicy {
        limits: CellLimits {
            max_components: Some(2),
            ..Default::default()
        },
        ..Default::default()
    };
    let patched = apply_cell_patch(
        cell(),
        vec![
            CellPatch::RemoveComponent { id: "z".into() },
            CellPatch::RemoveLink { id: "old".into() },
            CellPatch::AddLink {
                link: link("z-link", "y"),
            },
            CellPatch::AddComponent {
                component: component("y"),
            },
            CellPatch::UpdateComponent {
                component: updated.clone(),
            },
            CellPatch::AddLink {
                link: link("a-link", "y"),
            },
            CellPatch::SetPolicy {
                policy: policy.clone(),
            },
        ],
    )
    .unwrap();
    assert_eq!(patched.components, vec![updated, component("y")]);
    assert_eq!(
        patched.links,
        vec![link("a-link", "y"), link("z-link", "y")]
    );
    assert_eq!(patched.policy, policy);
    assert!(validate_cell(&patched).valid);
}

#[test]
fn identities_are_checked_in_order_including_duplicate_links_with_different_endpoints() {
    for (change, error) in [
        (
            CellPatch::AddComponent {
                component: component("a"),
            },
            "Component \"a\" already exists",
        ),
        (
            CellPatch::UpdateComponent {
                component: component("missing"),
            },
            "Component \"missing\" is absent",
        ),
        (
            CellPatch::RemoveComponent {
                id: "missing".into(),
            },
            "Component \"missing\" is absent",
        ),
        (
            CellPatch::AddLink {
                link: link("old", "missing"),
            },
            "Link \"old\" already exists",
        ),
        (
            CellPatch::RemoveLink {
                id: "missing".into(),
            },
            "Link \"missing\" is absent",
        ),
    ] {
        // A valid first operation must not hide a later failure or return a partial cell.
        let patch = vec![
            CellPatch::SetPolicy {
                policy: CellPolicy::default(),
            },
            change,
        ];
        assert_eq!(apply_cell_patch(cell(), patch).unwrap_err(), error);
    }
    assert_eq!(
        apply_cell_patch(
            cell(),
            vec![
                CellPatch::RemoveLink { id: "old".into() },
                CellPatch::RemoveLink { id: "old".into() },
            ]
        )
        .unwrap_err(),
        "Link \"old\" is absent"
    );
}

#[test]
fn batch_bounds_are_inclusive() {
    let change = CellPatch::SetPolicy {
        policy: CellPolicy::default(),
    };
    for count in [0, 101] {
        assert_eq!(
            apply_cell_patch(cell(), vec![change.clone(); count]).unwrap_err(),
            "patch must contain 1..=100 operations"
        );
    }
    for count in [1, 100] {
        assert!(apply_cell_patch(cell(), vec![change.clone(); count]).is_ok());
    }
}
