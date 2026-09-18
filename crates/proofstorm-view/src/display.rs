//! Human-readable labels; wire identifiers remain stable.
use proofstorm_core::{OperationKind, OperationPhase};

#[must_use]
pub const fn action_title(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::NodeStart => "Start node",
        OperationKind::NodeStop => "Stop node",
        OperationKind::NodeRestart => "Restart node",
        OperationKind::ComponentStart => "Start component",
        OperationKind::ComponentStop => "Stop component",
        OperationKind::ComponentRestart => "Restart component",
        OperationKind::BootstrapLiquidity => "Fund channels",
        OperationKind::PeerConnect => "Connect peers",
        OperationKind::PeerDisconnect => "Disconnect peers",
        OperationKind::ChannelOpen => "Open channel",
        OperationKind::ChannelPolicySet => "Update channel policy",
        OperationKind::ChannelClose => "Close channel",
        OperationKind::ChannelForceClose => "Force-close channel",
        OperationKind::ChannelRebalance => "Rebalance channel",
        OperationKind::NetworkPartition => "Partition network",
        OperationKind::NetworkDelay => "Add network delay",
        OperationKind::NetworkLoss => "Add packet loss",
        OperationKind::NetworkHeal => "Restore network",
        OperationKind::WalletInitialize => "Initialize wallet",
        OperationKind::WalletBalance => "Check wallet balance",
        OperationKind::WalletFund => "Fund wallet",
        OperationKind::WalletInvoice => "Create invoice",
        OperationKind::WalletPay => "Pay invoice",
        OperationKind::WalletQuoteClaim => "Claim wallet quote",
        OperationKind::WalletMeltQuoteRefresh => "Refresh payment quote",
        OperationKind::WalletRoundTrip => "Test wallet round trip",
        OperationKind::ConservationOracle => "Check fund conservation",
        OperationKind::ReachabilityOracle => "Check connectivity",
        OperationKind::ComponentForensics => "Inspect component files",
        OperationKind::ComponentExecLive => "Run command",
        OperationKind::PrivateTransfer => "Transfer private data",
        OperationKind::ComponentLogs => "Read component logs",
        OperationKind::AuthenticationConformance => "Test authentication",
        OperationKind::AuthenticationProtectedSpend => "Test authenticated spend",
        OperationKind::AuthenticationReplay => "Test authentication replay",
    }
}
#[must_use]
pub const fn outcome_title(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Pending => "Pending",
        OperationPhase::Running => "Running",
        OperationPhase::Succeeded => "Completed",
        OperationPhase::Failed => "Failed",
        OperationPhase::Cancelled => "Cancelled",
    }
}

/// Display names for MCP discovery and interfaces that refer to exact tool IDs.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one explicit title table covers the public MCP tool IDs"
)]
pub fn tool_title(name: &str) -> Option<&'static str> {
    Some(match name {
        "catalog_list" => "Browse components",
        "catalog_entry_read" => "View component specification",
        "catalog_config_schema_read" => "View configuration fields",
        "candidate_build" => "Build candidate",
        "candidate_wait" => "Wait for build",
        "candidate_list" => "List builds",
        "candidate_cancel" => "Cancel build",
        "cell_plan" => "Plan cell",
        "cell_up" => "Start cell",
        "cell_inspect" => "Inspect cell",
        "cell_read" => "View cell configuration",
        "cell_search" => "Search cell configuration",
        "cell_component_status_list" => "List component statuses",
        "cell_inventory_list" => "List cell resources",
        "cell_wait" => "Wait for cell",
        "cell_remove" => "Remove cell",
        "environment_read" => "View environment",
        "session_list" => "List sessions",
        "run_start" => "Start evidence run",
        "run_read" => "Read evidence run",
        "run_finish" => "Finish evidence run",
        "cell_exec" => "Cell exec",
        "workspace_task" => "Workspace task",
        "workspace_file" => "Workspace file",
        "workspace_upload" => "Upload workspace file",
        "workspace_capture" => "Capture workspace evidence",
        "component_forensics" => action_title(OperationKind::ComponentForensics),
        "component_logs" => action_title(OperationKind::ComponentLogs),
        "component_start" => action_title(OperationKind::ComponentStart),
        "component_stop" => action_title(OperationKind::ComponentStop),
        "component_restart" => action_title(OperationKind::ComponentRestart),
        "network_capabilities" => "View network controls",
        "network_partition" => action_title(OperationKind::NetworkPartition),
        "network_heal" => action_title(OperationKind::NetworkHeal),
        "network_probe" => "Probe reachability",
        "private_transfer" => action_title(OperationKind::PrivateTransfer),
        "private_access_issue" => "Grant private access",
        "private_access_read" => "View private access",
        "private_access_revoke" => "Revoke private access",
        "cell_sync" => "Collect results",
        "activity_search" => "Search recorded activity",
        "operation_read" => "Read recorded result",
        "operation_status" => "Check operation status",
        "operation_wait" => "Wait for operation",
        "operation_cancel" => "Cancel operation",
        "evidence_export" => "Export evidence",
        "evidence_section_read" => "View evidence section",
        "wallet_balance" => action_title(OperationKind::WalletBalance),
        _ => return None,
    })
}
