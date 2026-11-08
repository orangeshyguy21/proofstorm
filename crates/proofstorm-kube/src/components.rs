//! Each component's observed state lives in its own `ProofstormComponent`
//! object, written only when the component's state changes. The cell status
//! carries a summary and a protocol freshness heartbeat instead of every
//! component, so it stays small and isn't rewritten on every probe.
//!
//! Readers rebuild today's in-memory shape with
//! [`assemble_component_statuses`]: `status.components` filled from the objects,
//! with protocol evidence kept fresh by the heartbeat.
use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use proofstorm_core::{CellSpec, ComponentStatus};

use crate::{
    COMPONENT_LABEL, CellPhase, INSTANCE_LABEL, ProofstormCell, ProofstormComponent,
    ProofstormComponentSpec, ProtocolFreshness,
};

/// The component object's name: one per cell instance and component.
#[must_use]
pub fn component_object_name(instance_key: &str, component_id: &str) -> String {
    format!("{instance_key}-{component_id}")
}

/// The component object for `status`, owned by its cell when `owner` is given.
#[must_use]
pub fn component_object(
    instance_key: &str,
    namespace: &str,
    status: &ComponentStatus,
    owner: Option<OwnerReference>,
) -> ProofstormComponent {
    ProofstormComponent {
        metadata: ObjectMeta {
            name: Some(component_object_name(instance_key, &status.id)),
            namespace: Some(namespace.to_owned()),
            labels: Some(BTreeMap::from([
                (INSTANCE_LABEL.to_owned(), instance_key.to_owned()),
                (COMPONENT_LABEL.to_owned(), status.id.clone()),
            ])),
            owner_references: owner.map(|owner| vec![owner]),
            ..ObjectMeta::default()
        },
        spec: ProofstormComponentSpec {
            instance_key: instance_key.to_owned(),
            component_id: status.id.clone(),
        },
        status: Some(status.clone()),
    }
}

/// Whether `a` and `b` differ only in when they were last probed, so a
/// component object is rewritten only when its state changes.
#[must_use]
pub fn same_material(a: &ComponentStatus, b: &ComponentStatus) -> bool {
    // Destructured so a new field can't be left out by accident.
    let ComponentStatus {
        id,
        kind,
        observed_revision_digest,
        observed_rollout_digest,
        conditions,
        ready,
        protocol_observation,
        service,
        ports,
    } = a;
    *id == b.id
        && *kind == b.kind
        && *observed_revision_digest == b.observed_revision_digest
        && *observed_rollout_digest == b.observed_rollout_digest
        && *conditions == b.conditions
        && *ready == b.ready
        // An observation holds only probe times, which the cell's heartbeat carries.
        && protocol_observation.is_some() == b.protocol_observation.is_some()
        && *service == b.service
        && *ports == b.ports
}

/// The window every probed component was confirmed in: the oldest observation
/// and the earliest expiry. `None` when no component has protocol evidence.
#[must_use]
pub fn protocol_freshness(components: &[ComponentStatus]) -> Option<ProtocolFreshness> {
    components
        .iter()
        .filter_map(|component| component.protocol_observation.as_ref())
        .fold(None, |window: Option<ProtocolFreshness>, observation| {
            Some(ProtocolFreshness {
                observed_at_unix: window.map_or(observation.observed_at_unix, |w| {
                    w.observed_at_unix.min(observation.observed_at_unix)
                }),
                expires_at_unix: window.map_or(observation.expires_at_unix, |w| {
                    w.expires_at_unix.min(observation.expires_at_unix)
                }),
            })
        })
}

/// Fill `cell.status.components` from its component objects and apply the
/// cell's freshness heartbeat to their protocol evidence. With the cell's
/// `spec`, components follow its order and objects of components it lacks are
/// ignored; without it, all of the cell's objects are used in id order.
/// Objects of other cells are ignored. A blocked cell presents no component
/// evidence.
///
/// Read the cell before listing its objects: the controller writes changed
/// objects before each heartbeat, so a heartbeat never vouches for evidence
/// a later object no longer has.
pub fn assemble_component_statuses<'a>(
    cell: &mut ProofstormCell,
    spec: Option<&CellSpec>,
    objects: impl IntoIterator<Item = &'a ProofstormComponent>,
) {
    let mut by_id: BTreeMap<&str, &ComponentStatus> = objects
        .into_iter()
        .filter(|object| object.spec.instance_key == cell.spec.instance_key)
        .filter_map(|object| {
            object
                .status
                .as_ref()
                .map(|status| (object.spec.component_id.as_str(), status))
        })
        .collect();
    let mut components: Vec<ComponentStatus> = match spec {
        Some(spec) => spec
            .components
            .iter()
            .filter_map(|component| by_id.remove(component.id.as_str()).cloned())
            .collect(),
        None => by_id.into_values().cloned().collect(),
    };
    let Some(status) = cell.status.as_mut() else {
        return;
    };
    if status.phase == CellPhase::Blocked {
        status.components.clear();
        return;
    }
    if let Some(freshness) = status.protocol_freshness {
        // Both the object's evidence and the heartbeat are lower bounds on
        // how fresh the component's evidence is; keep the later of each.
        for observation in components
            .iter_mut()
            .filter_map(|component| component.protocol_observation.as_mut())
        {
            observation.observed_at_unix =
                observation.observed_at_unix.max(freshness.observed_at_unix);
            observation.expires_at_unix =
                observation.expires_at_unix.max(freshness.expires_at_unix);
        }
    }
    status.components = components;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LoadedCell, ProofstormCellSpec, ProofstormCellStatus};
    use proofstorm_core::{
        API_VERSION, CellPolicy, CellSpec, ComponentKind, ComponentSpec, ControlClass,
        ProtocolObservation, ResolvedLock,
    };

    fn status(id: &str, observed_at_unix: i64) -> ComponentStatus {
        ComponentStatus {
            id: id.into(),
            kind: ComponentKind::Bitcoin,
            observed_revision_digest: "sha256:revision".into(),
            observed_rollout_digest: "sha256:rollout".into(),
            conditions: Vec::new(),
            ready: true,
            protocol_observation: Some(ProtocolObservation {
                observed_at_unix,
                expires_at_unix: observed_at_unix + 30,
                elapsed_micros: 5,
            }),
            service: id.into(),
            ports: BTreeMap::new(),
        }
    }

    fn cell(ids: &[&str]) -> LoadedCell {
        let spec = CellSpec {
            api_version: API_VERSION.into(),
            name: "cell".into(),
            components: ids
                .iter()
                .map(|id| ComponentSpec {
                    resources: None,
                    id: (*id).into(),
                    kind: ComponentKind::Bitcoin,
                    implementation: "bitcoin-core".into(),
                    version: None,
                    config_version: "bitcoin-core/31/v1".into(),
                    control: ControlClass::Cell,
                    config: BTreeMap::new(),
                })
                .collect(),
            links: Vec::new(),
            policy: CellPolicy::default(),
        };
        let lock = ResolvedLock {
            api_version: API_VERSION.into(),
            digest: "sha256:lock".into(),
            entries: Vec::new(),
        };
        let mut resource = ProofstormCell::new(
            "cell",
            ProofstormCellSpec {
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "ikey".into(),
                revision_digest: "sha256:revision".into(),
            },
        );
        resource.status = Some(ProofstormCellStatus::default());
        LoadedCell::from_spec(resource, spec, lock)
    }

    #[test]
    fn without_the_spec_every_object_of_the_cell_is_used_in_id_order() {
        let mut cell = cell(&[]).resource;
        let objects = [
            component_object("ikey", "system", &status("wallet", 100), None),
            component_object("ikey", "system", &status("chain", 100), None),
            component_object("other", "system", &status("mint", 100), None),
        ];
        assemble_component_statuses(&mut cell, None, &objects);
        assert_eq!(
            cell.status
                .unwrap()
                .components
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            ["chain", "wallet"]
        );
    }

    #[test]
    fn a_new_probe_alone_does_not_change_the_material_state() {
        let first = status("chain", 100);
        let reprobed = status("chain", 160);
        assert!(same_material(&first, &reprobed));
        let mut down = reprobed;
        down.ready = false;
        assert!(!same_material(&first, &down));
    }

    #[test]
    fn freshness_is_the_oldest_observation_and_earliest_expiry() {
        assert_eq!(protocol_freshness(&[]), None);
        assert_eq!(
            protocol_freshness(&[status("a", 100), status("b", 90)]),
            Some(ProtocolFreshness {
                observed_at_unix: 90,
                expires_at_unix: 120
            })
        );
    }

    #[test]
    fn assembly_follows_revision_order_ignores_strangers_and_applies_the_heartbeat() {
        let mut loaded = cell(&["chain", "wallet"]);
        loaded.resource.status.as_mut().unwrap().protocol_freshness = Some(ProtocolFreshness {
            observed_at_unix: 500,
            expires_at_unix: 530,
        });
        let objects = [
            component_object("ikey", "system", &status("wallet", 100), None),
            component_object("ikey", "system", &status("chain", 100), None),
            component_object("other", "system", &status("chain", 100), None),
            component_object("ikey", "system", &status("removed", 100), None),
        ];
        loaded.assemble_components(&objects);
        let components = &loaded.status.as_ref().unwrap().components;
        assert_eq!(
            components.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["chain", "wallet"]
        );
        let observation = components[0].protocol_observation.as_ref().unwrap();
        assert_eq!(
            (observation.observed_at_unix, observation.expires_at_unix),
            (500, 530)
        );
        assert_eq!(observation.elapsed_micros, 5);
    }

    #[test]
    fn a_blocked_cell_presents_no_component_evidence() {
        let mut loaded = cell(&["chain"]);
        loaded.resource.status.as_mut().unwrap().phase = CellPhase::Blocked;
        let objects = [component_object(
            "ikey",
            "system",
            &status("chain", 100),
            None,
        )];
        loaded.assemble_components(&objects);
        assert!(loaded.status.as_ref().unwrap().components.is_empty());
    }

    #[test]
    fn an_older_heartbeat_never_ages_newer_evidence() {
        let mut loaded = cell(&["chain"]);
        loaded.resource.status.as_mut().unwrap().protocol_freshness = Some(ProtocolFreshness {
            observed_at_unix: 50,
            expires_at_unix: 80,
        });
        let objects = [component_object(
            "ikey",
            "system",
            &status("chain", 100),
            None,
        )];
        loaded.assemble_components(&objects);
        let observation = loaded.status.as_ref().unwrap().components[0]
            .protocol_observation
            .clone()
            .unwrap();
        assert_eq!(
            (observation.observed_at_unix, observation.expires_at_unix),
            (100, 130)
        );
    }

    #[test]
    fn without_a_newer_heartbeat_stored_evidence_ages_out() {
        let mut loaded = cell(&["chain"]);
        let objects = [component_object(
            "ikey",
            "system",
            &status("chain", 100),
            None,
        )];
        loaded.assemble_components(&objects);
        let observation = loaded.status.as_ref().unwrap().components[0]
            .protocol_observation
            .clone()
            .unwrap();
        assert_eq!(observation.expires_at_unix, 130);
        assert!(!observation.is_fresh(130));
    }
}
