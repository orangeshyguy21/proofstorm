//! Live acceptance gates sharing the installation-aware runner and MCP client.

use anyhow::{Result, bail};

use crate::GateContext;

pub mod agents;
mod authentication;
mod candidate_isolation;
mod candidate_source;
pub mod candidates;
mod candidates_nutshell;
pub mod cashu_double_spend;
pub mod cdk_bdk_stress;
pub mod cdk_cln;
pub mod cdk_ldk;
pub mod cdk_lnd_bdk;
mod cdk_mint_identity;
pub mod cdk_oidc;
pub mod cdk_postgres;
pub mod cdk_wallet;
pub mod cocod_wallet;
pub mod cross_cell_scheduler;
pub mod cross_implementation_wallet;
pub mod dynamic_cell;
pub mod failed_melt;
pub mod gui;
pub mod isolation;
pub mod keycloak;
pub mod ldk_server;
pub mod mint_management;
pub mod native_exec;
pub mod nutshell_cln;
pub mod nutshell_mint;
pub mod nutshell_oidc;
pub mod nutshell_postgres;
pub mod onboarding;
pub mod private_handoff;
pub mod private_transfer;
pub mod progress;
pub mod quote_composition;
pub mod reliable_exec;
pub mod runtime_lifecycle;
pub mod slice2;
pub mod slice4;
pub mod slice5;
pub mod smoke;
pub mod surface;

/// Every gate name the binary accepts, in the plan's port order.
pub const NAMES: &[&str] = &[
    "benchmark-o1",
    "benchmark-oracle",
    "qualification",
    "smoke",
    "runtime-lifecycle",
    "mcp-surface",
    "candidate-build-isolation",
    "candidate-coco",
    "candidate-cdk-cli",
    "candidate-nutshell-wallet",
    "candidate-nutshell",
    "candidate-cdk-modes",
    "candidate-cdk",
    "onboarding",
    "gui",
    "cli-progress",
    "agent-config",
    "agent-clients",
    "installation-isolation",
    "cashu-double-spend",
    "mint-management",
    "dynamic-cell",
    "nutshell-mint",
    "cdk-cln",
    "cdk-wallet",
    "cocod-wallet",
    "private-transfer",
    "private-handoff",
    "cocod-projection",
    "cdk-wallet-fees",
    "slice4",
    "nutshell-cln",
    "nutshell-postgres",
    "cdk-postgres",
    "cross-cell-scheduler",
    "cdk-ldk",
    "ldk-server-processor",
    "cdk-ldk-postgres",
    "cdk-bdk",
    "cdk-bdk-postgres-stress",
    "cdk-bdk-stress",
    "cdk-bdk-postgres",
    "cdk-lnd-bdk",
    "cdk-oidc",
    "cross-implementation-wallet",
    "cdk-mint-identity",
    "native-exec",
    "reliable-exec",
    "slice2",
    "slice5",
    "controller-recovery",
    "network-faults",
    "channel-lifecycle",
    "failed-melt",
    "quote-composition",
    "nutshell-oidc",
    "keycloak",
];

/// Dispatch a gate by the name passed to `just e2e`.
pub fn run(name: &str, context: &GateContext) -> Result<()> {
    match name {
        "benchmark-oracle" => crate::benchmark::reference::run(context),
        "qualification" => crate::qualification::run(context),
        "smoke" => smoke::run(context),
        "runtime-lifecycle" => runtime_lifecycle::run(context),
        "mcp-surface" => surface::run(context),
        "candidate-build-isolation" => candidate_isolation::run(context),
        "candidate-coco" => candidates::run(context, "cocod-wallet"),
        "candidate-cdk-cli" => candidates::run(context, "cdk-cli-wallet"),
        "candidate-nutshell-wallet" => candidates::run(context, "nutshell-wallet"),
        "candidate-nutshell" => candidates::run(context, "nutshell"),
        "candidate-cdk-modes" => candidates::run_cdk_modes(context),
        "candidate-cdk" => candidates::run(context, "cdk"),
        "onboarding" => onboarding::run(context),
        "gui" => gui::run(context),
        "cli-progress" => progress::run(context),
        "agent-config" => agents::run(context, false),
        "agent-clients" => agents::run(context, true),
        "installation-isolation" => isolation::run(context),
        "cashu-double-spend" => cashu_double_spend::run(context),
        "mint-management" => mint_management::run(context),
        "dynamic-cell" => dynamic_cell::run(context),
        "nutshell-mint" => nutshell_mint::run(context),
        "cdk-cln" => cdk_cln::run(context),
        "cdk-wallet" => cdk_wallet::run(context),
        "private-transfer" => cocod_wallet::run_transfer(context),
        "private-handoff" => cocod_wallet::run_handoff(context),
        "cocod-wallet" => cocod_wallet::run(context),
        "cocod-projection" => cocod_wallet::run_projection(context),
        "cdk-wallet-fees" => cdk_wallet::run_with_fee(context, 100),
        "slice4" => slice4::run(context),
        "nutshell-cln" => nutshell_cln::run(context),
        "nutshell-postgres" => nutshell_postgres::run(context),
        "cdk-postgres" => cdk_postgres::run(context),
        "cross-cell-scheduler" => cross_cell_scheduler::run(context),
        "cdk-ldk" => cdk_ldk::run(context, crate::postgres::enabled()),
        "ldk-server-processor" => ldk_server::run(context),
        "cdk-ldk-postgres" => cdk_ldk::run(context, true),
        "cdk-bdk" => cdk_bdk_stress::run(context, false, false),
        "cdk-bdk-stress" => cdk_bdk_stress::run(context, crate::postgres::enabled(), true),
        "cdk-bdk-postgres-stress" => cdk_bdk_stress::run(context, true, true),
        "cdk-bdk-postgres" => cdk_bdk_stress::run(context, true, false),
        "cdk-lnd-bdk" => cdk_lnd_bdk::run(context),
        "cdk-oidc" => cdk_oidc::run(context),
        "cross-implementation-wallet" => cross_implementation_wallet::run(context),
        "cdk-mint-identity" => cdk_mint_identity::run(context),
        "native-exec" => native_exec::run(context),
        "reliable-exec" => reliable_exec::run(context),
        "slice2" => slice2::run(context),
        "slice5" => slice5::run(context, slice5::Scenario::Smoke),
        "controller-recovery" => slice5::run(context, slice5::Scenario::Recovery),
        "network-faults" => slice5::run(context, slice5::Scenario::Network),
        "channel-lifecycle" => slice5::run(context, slice5::Scenario::Channels),
        "failed-melt" => failed_melt::run(context),
        "quote-composition" => quote_composition::run(context),
        "nutshell-oidc" => nutshell_oidc::run(context),
        "keycloak" => keycloak::run(context),
        other => bail!(
            "unknown gate {other}; available gates: {}",
            NAMES.join(", ")
        ),
    }
}
