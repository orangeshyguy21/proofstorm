//! The explicit local developer grant preset, shared by init and first setup.
use anyhow::Result;
use proofstorm_core::Capability;
use proofstorm_store::{Store, Workspace};

pub const PRESET: &str = "proofstorm/developer/v1";
pub const CAPABILITIES: [Capability; 16] = [
    Capability::CatalogRead,
    Capability::CellCreate,
    Capability::CellEdit,
    Capability::CellRead,
    Capability::CellPublish,
    Capability::CellMaterialize,
    Capability::CellStatus,
    Capability::CellClose,
    Capability::CellConnect,
    Capability::ExperimentCreate,
    Capability::ExperimentRead,
    Capability::ExperimentClose,
    Capability::CellOperate,
    Capability::ComponentExecLive,
    Capability::ArtifactRead,
    Capability::ActionCancel,
];

/// Explicitly replaces this principal's grants; never call this on setup retries.
pub fn configure(store: &Store, workspace: &str, principal: &str) -> Result<()> {
    store.put_workspace(&Workspace {
        id: workspace.into(),
        name: workspace.into(),
    })?;
    store.put_principal(principal)?;
    store.replace_grants(workspace, principal, CAPABILITIES)?;
    Ok(())
}
