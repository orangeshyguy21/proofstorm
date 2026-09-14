//! The explicit local developer grant preset, shared by init and first setup.
use anyhow::Result;
use proofstorm_core::Capability;
use proofstorm_store::{Store, Workspace};

pub const PRESET: &str = "proofstorm/default/v1";

#[must_use]
pub fn capabilities() -> Vec<Capability> {
    proofstorm_core::mcp::default_capabilities()
        .into_iter()
        .collect()
}

/// Explicitly replaces this principal's grants; never call this on setup retries.
pub fn configure(store: &Store, workspace: &str, principal: &str) -> Result<()> {
    store.put_workspace(&Workspace {
        id: workspace.into(),
        name: workspace.into(),
    })?;
    store.put_principal(principal)?;
    store.replace_grants(workspace, principal, capabilities())?;
    Ok(())
}
