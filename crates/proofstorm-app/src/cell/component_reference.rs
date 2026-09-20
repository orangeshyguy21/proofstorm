//! Resolve locked components with actionable diagnostics.
use crate::{Error, ErrorKind};
use proofstorm_core::{ComponentKind, PublishedRevision};

fn invalid_request(message: String, details: Option<serde_json::Value>) -> Error {
    Error {
        kind: ErrorKind::Invalid,
        message,
        details,
    }
}

pub(super) fn component_image_any(
    revision: &PublishedRevision,
    id: &str,
    kind: ComponentKind,
) -> Result<String, Error> {
    let valid_component_ids = || valid_component_ids(revision, kind);
    let Some(component) = revision
        .cell
        .components
        .iter()
        .find(|component| component.id == id)
    else {
        let valid_component_ids = valid_component_ids();
        return Err(invalid_request(
            format!(
                "component {id:?} is not in this revision; expected {}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_id_unknown",
                "requested_id": id,
                "expected_kind": kind,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    };
    if component.kind != kind {
        let valid_component_ids = valid_component_ids();
        return Err(invalid_request(
            format!(
                "component {id:?} is {}; expected {}; valid component IDs: {valid_component_ids:?}",
                component_kind_name(component.kind),
                component_kind_name(kind)
            ),
            Some(serde_json::json!({
                "code": "component_kind_mismatch",
                "requested_id": id,
                "actual_kind": component.kind,
                "expected_kind": kind,
                "valid_component_ids": valid_component_ids,
            })),
        ));
    }
    revision
        .lock
        .entries
        .iter()
        .find(|entry| entry.component_id == id && entry.catalog_id == component.implementation)
        .map(|entry| entry.image.clone())
        .ok_or_else(|| revision_integrity_error(id))
}

fn valid_component_ids(revision: &PublishedRevision, kind: ComponentKind) -> Vec<String> {
    let mut ids = revision
        .cell
        .components
        .iter()
        .filter(|component| component.kind == kind)
        .map(|component| component.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

const fn component_kind_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Bitcoin => "bitcoin",
        ComponentKind::Lightning => "lightning",
        ComponentKind::Mint => "mint",
        ComponentKind::Database => "database",
        ComponentKind::IdentityProvider => "identity_provider",
        ComponentKind::Wallet => "wallet",
        ComponentKind::Attacker => "workspace",
        ComponentKind::Proxy => "proxy",
        ComponentKind::Oracle => "oracle",
    }
}

fn revision_integrity_error(component_id: &str) -> Error {
    Error::failure(
        format!(
            "published revision has no matching immutable lock entry for component {component_id:?}"
        ),
        Some(serde_json::json!({
            "code": "revision_integrity_error",
            "component_id": component_id,
        })),
    )
}
