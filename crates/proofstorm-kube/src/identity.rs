//! A cell's identity in its metadata, so hosts can enumerate and check cells
//! with metadata-only reads. The spec stays the controller's source of truth;
//! these copies are written once, when the cell is created.
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

use crate::{INSTANCE_LABEL, ProofstormCell};

/// The cell's workspace id. An annotation: ids aren't guaranteed label-safe.
pub const WORKSPACE_ID_ANNOTATION: &str = "proofstorm.dev/workspace-id";
/// The cell's instance id. An annotation: draft cells use plain names.
pub const INSTANCE_ID_ANNOTATION: &str = "proofstorm.dev/instance-id";

/// Who a cell is, as its metadata records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellIdentity {
    pub workspace_id: String,
    pub instance_id: String,
    pub instance_key: String,
}

/// Copy the cell's identity from its spec into its labels and annotations.
pub fn identify_cell(cell: &mut ProofstormCell) {
    let spec = cell.spec.clone();
    cell.metadata
        .labels
        .get_or_insert_default()
        .insert(INSTANCE_LABEL.to_owned(), spec.instance_key);
    let annotations = cell.metadata.annotations.get_or_insert_default();
    annotations.insert(WORKSPACE_ID_ANNOTATION.to_owned(), spec.workspace_id);
    annotations.insert(INSTANCE_ID_ANNOTATION.to_owned(), spec.instance_id);
}

/// The identity a cell's metadata records, or `None` when any part is missing.
#[must_use]
pub fn cell_identity(metadata: &ObjectMeta) -> Option<CellIdentity> {
    let annotations = metadata.annotations.as_ref()?;
    Some(CellIdentity {
        workspace_id: annotations.get(WORKSPACE_ID_ANNOTATION)?.clone(),
        instance_id: annotations.get(INSTANCE_ID_ANNOTATION)?.clone(),
        instance_key: metadata.labels.as_ref()?.get(INSTANCE_LABEL)?.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProofstormCellSpec;

    #[test]
    fn identity_round_trips_through_metadata_and_needs_every_part() {
        let mut cell = ProofstormCell::new(
            "cell-ikey",
            ProofstormCellSpec {
                workspace_id: "local workspace".into(),
                instance_id: "demo".into(),
                instance_key: "ikey".into(),
                revision_digest: "sha256:revision".into(),
            },
        );
        assert_eq!(cell_identity(&cell.metadata), None);
        identify_cell(&mut cell);
        assert_eq!(
            cell_identity(&cell.metadata),
            Some(CellIdentity {
                workspace_id: "local workspace".into(),
                instance_id: "demo".into(),
                instance_key: "ikey".into(),
            })
        );
        cell.metadata.labels = None;
        assert_eq!(cell_identity(&cell.metadata), None);
    }
}
