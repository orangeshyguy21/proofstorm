//! One module per live acceptance gate, each ported from its Python client.

use anyhow::{Result, bail};

use crate::GateContext;

pub mod cdk_bdk_stress;
pub mod cdk_cln;
pub mod cdk_ldk;
pub mod cdk_postgres;
pub mod cdk_wallet;
pub mod cocod_wallet;
pub mod cross_implementation_wallet;
pub mod cross_lab_scheduler;
pub mod failed_melt;
pub mod native_exec;
pub mod nutshell_cln;
pub mod nutshell_mint;
pub mod nutshell_oidc;
pub mod nutshell_postgres;
pub mod private_handoff;
pub mod private_transfer;
pub mod quote_composition;
pub mod reliable_exec;
pub mod slice2;
pub mod slice4;
pub mod slice5;

/// Every gate name the binary accepts, in the plan's port order.
pub const NAMES: &[&str] = &[
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
    "cross-lab-scheduler",
    "cdk-ldk",
    "cdk-ldk-postgres",
    "cdk-bdk-stress",
    "cdk-bdk-postgres",
    "cross-implementation-wallet",
    "native-exec",
    "reliable-exec",
    "slice2",
    "slice5",
    "failed-melt",
    "quote-composition",
    "nutshell-oidc",
];

/// Dispatch a gate by the name its Makefile target uses.
pub fn run(name: &str, context: &GateContext) -> Result<()> {
    match name {
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
        "cross-lab-scheduler" => cross_lab_scheduler::run(context),
        "cdk-ldk" => cdk_ldk::run(context, crate::postgres::enabled()),
        "cdk-ldk-postgres" => cdk_ldk::run(context, true),
        "cdk-bdk-stress" => cdk_bdk_stress::run(context, crate::postgres::enabled()),
        "cdk-bdk-postgres" => cdk_bdk_stress::run(context, true),
        "cross-implementation-wallet" => cross_implementation_wallet::run(context),
        "native-exec" => native_exec::run(context),
        "reliable-exec" => reliable_exec::run(context),
        "slice2" => slice2::run(context),
        "slice5" => slice5::run(context),
        "failed-melt" => failed_melt::run(context),
        "quote-composition" => quote_composition::run(context),
        "nutshell-oidc" => nutshell_oidc::run(context),
        other => bail!(
            "unknown gate {other}; available gates: {}",
            NAMES.join(", ")
        ),
    }
}
