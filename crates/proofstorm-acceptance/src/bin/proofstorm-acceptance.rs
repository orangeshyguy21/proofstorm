#![allow(clippy::doc_markdown)]
//! Live acceptance gate runner.
//!
//! Replaces the per-gate shell wrapper plus Python client pair. Each gate is a
//! subcommand so the Makefile can invoke one directly and read its exit code.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use proofstorm_acceptance::{GateContext, Kubectl, doctor, gates};

#[derive(Parser)]
#[command(
    name = "proofstorm-acceptance",
    about = "Run one live Proofstorm acceptance gate against the lab cluster"
)]
struct Arguments {
    /// Isolated installation for images, images-check, or cluster-schema only.
    #[arg(long, env = "PROOFSTORM_HOME")]
    home: Option<PathBuf>,
    /// Gate to run, matching its `make e2e-<gate>` target.
    #[arg(required_unless_present = "list")]
    gate: Option<String>,
    /// Repository root. Defaults to the current directory.
    #[arg(long)]
    root: Option<PathBuf>,
    /// MCP server binary. Defaults to `<root>/target/debug/proofstorm-mcp`.
    #[arg(long)]
    mcp_binary: Option<PathBuf>,
    /// List the available gates and exit.
    #[arg(long)]
    list: bool,
    /// OpenCode MCP configuration the `doctor` check reads.
    #[arg(long, default_value = "examples/opencode/proofstorm-only.json")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    if arguments.list {
        for name in gates::NAMES {
            println!("{name}");
        }
        return Ok(());
    }

    let root = match arguments.root {
        Some(path) => path,
        None => std::env::current_dir().context("resolve repository root")?,
    };
    let gate = arguments.gate.context("no gate given")?;
    if let Some(home) = &arguments.home {
        let installation = proofstorm_app::installation::Installation::load(home)?;
        let kubectl = Kubectl::for_installation(&root, &installation);
        let registry =
            proofstorm_acceptance::images::RegistryTarget::for_installation(&installation);
        return match gate.as_str() {
            "images" => proofstorm_acceptance::images::provision_for(&registry),
            "images-check" => proofstorm_acceptance::images::verify_for(&kubectl, &registry),
            "cluster-schema" => doctor::cluster_schema(&kubectl),
            _ => anyhow::bail!(
                "gate {gate} is not installation-aware; refusing to fall back to the development cluster"
            ),
        };
    }
    match gate.as_str() {
        "images" => proofstorm_acceptance::images::provision(),
        "images-check" => proofstorm_acceptance::images::verify(&Kubectl::pinned(&root)),
        "doctor" => {
            let binary = arguments
                .mcp_binary
                .unwrap_or_else(|| root.join("target/release/proofstorm-mcp"));
            doctor::run(&binary, &root.join(&arguments.config))
        }
        "cluster-schema" => doctor::cluster_schema(&Kubectl::pinned(&root)),
        _ => {
            let context = GateContext::new(&root, arguments.mcp_binary)?;
            gates::run(&gate, &context)
        }
    }
}
