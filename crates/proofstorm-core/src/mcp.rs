//! The single public MCP contract. Permissions and runtime availability are independent facts.
use crate::Capability;
use std::collections::BTreeSet;

pub struct PublicTool {
    pub name: &'static str,
    pub capabilities: &'static [Capability],
    pub requires_runtime: bool,
}

macro_rules! tool {
    ($name:literal, $runtime:literal, [$($cap:ident),*]) => {
        PublicTool { name: $name, requires_runtime: $runtime, capabilities: &[$(Capability::$cap),*] }
    };
}

pub const TOOLS: &[PublicTool] = &[
    tool!("catalog_list", false, [CatalogRead]),
    tool!("catalog_entry_read", false, [CatalogRead]),
    tool!("catalog_config_schema_read", false, [CatalogRead]),
    tool!(
        "candidate_build",
        true,
        [CandidateBuild, CandidateRead, CatalogRead]
    ),
    tool!("candidate_wait", true, [CandidateRead]),
    tool!("candidate_list", false, [CandidateRead]),
    tool!("candidate_cancel", true, [CandidateCancel, CandidateRead]),
    tool!(
        "cell_plan",
        false,
        [
            CatalogRead,
            CellCreate,
            CellRead,
            CellPublish,
            CellStatus,
            CellEdit
        ]
    ),
    tool!(
        "cell_up",
        true,
        [
            CellCreate,
            CellRead,
            CellPublish,
            CellMaterialize,
            CellStatus,
            CatalogRead,
            ExperimentRead,
            CellOperate,
            CellEdit
        ]
    ),
    tool!("cell_inspect", true, [CellStatus, ExperimentRead]),
    tool!("cell_read", false, [CellRead]),
    tool!("cell_search", false, [CellRead]),
    tool!("cell_component_status_list", true, [CellStatus]),
    tool!("cell_inventory_list", true, [CellStatus]),
    tool!("cell_wait", true, [CellStatus]),
    tool!(
        "cell_remove",
        true,
        [
            CellStatus,
            CellClose,
            ExperimentRead,
            ExperimentClose,
            ArtifactRead,
            ActionCancel
        ]
    ),
    tool!(
        "environment_read",
        true,
        [CellRead, CellStatus, ExperimentRead]
    ),
    tool!("session_list", false, [ExperimentRead]),
    tool!("run_start", false, [ExperimentCreate]),
    tool!("run_read", false, [ExperimentRead]),
    tool!(
        "run_finish",
        true,
        [ExperimentClose, ExperimentRead, ArtifactRead]
    ),
    tool!("cell_exec", true, [ComponentExecLive]),
    tool!("workspace_task", true, [ComponentExecLive, ArtifactRead]),
    tool!("workspace_file", true, [ComponentExecLive, ArtifactRead]),
    tool!(
        "workspace_capture",
        false,
        [ComponentExecLive, ArtifactRead, ExperimentRead]
    ),
    tool!("component_forensics", true, [ComponentForensics]),
    tool!("component_logs", true, [ComponentLogs]),
    tool!("component_start", true, [ComponentControl]),
    tool!("component_stop", true, [ComponentControl]),
    tool!("component_restart", true, [ComponentControl]),
    tool!("network_capabilities", false, [CatalogRead]),
    tool!("network_partition", true, [NetworkPartition]),
    tool!("network_heal", true, [NetworkHeal]),
    tool!("network_probe", true, [OracleRun]),
    tool!("private_transfer", true, [ComponentExecLive]),
    tool!(
        "private_access_issue",
        true,
        [CellOperate, ComponentExecLive]
    ),
    tool!("private_access_read", false, [ExperimentRead]),
    tool!("private_access_revoke", true, [ExperimentRead]),
    tool!(
        "cell_sync",
        true,
        [CellStatus, ArtifactRead, ExperimentRead]
    ),
    tool!(
        "activity_search",
        false,
        [CellStatus, ExperimentRead, ArtifactRead]
    ),
    tool!("operation_read", false, [ArtifactRead]),
    tool!("operation_status", true, [ArtifactRead]),
    tool!("operation_wait", true, [ArtifactRead]),
    tool!("operation_cancel", true, [ActionCancel]),
    tool!("evidence_export", false, [ExperimentRead, ArtifactRead]),
    tool!(
        "evidence_section_read",
        false,
        [ExperimentRead, ArtifactRead]
    ),
    tool!("wallet_balance", true, [WalletControl]),
];

#[must_use]
pub fn tool(name: &str) -> Option<&'static PublicTool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Fresh default actors can use the complete contract. Reconnection never calls this to grant access.
#[must_use]
pub fn default_capabilities() -> BTreeSet<Capability> {
    TOOLS
        .iter()
        .flat_map(|tool| tool.capabilities.iter().copied())
        .collect()
}
