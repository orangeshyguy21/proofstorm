//! Opt-in developer campaign runner; installed preview packaging is separate.
use anyhow::{Context, Result};
use clap::Parser;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(about = "Run a frozen local model campaign; never retries a model launch")]
struct Args {
    #[arg(long, required_unless_present_any = ["tasks", "opencode_profile_hash"], conflicts_with_all = ["tasks", "opencode_profile_hash"])]
    plan: Option<PathBuf>,
    /// Continue a stopped campaign, rechecking all retained receipts first.
    #[arg(long, requires = "plan")]
    resume: bool,
    /// Print current task contract hashes without running a model or Docker.
    #[arg(long)]
    tasks: bool,
    /// Hash a private `OpenCode` provider profile and its local plugin files.
    #[arg(long, conflicts_with = "tasks")]
    opencode_profile_hash: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(path) = &args.opencode_profile_hash {
        println!(
            "{}",
            proofstorm_acceptance::campaign::opencode_profile_hash(path)?
        );
        return Ok(());
    }
    if args.tasks {
        println!(
            "{}",
            serde_json::to_string_pretty(&proofstorm_acceptance::campaign::contracts())?
        );
        return Ok(());
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("register SIGTERM");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        signal.store(true, Ordering::SeqCst);
    });
    let path = args.plan.context("plan required")?;
    tokio::task::spawn_blocking(move || {
        proofstorm_acceptance::campaign::run(&path, args.resume, &cancelled)
    })
    .await?
}
