//! Live gates own disposable installations; no implicit developer-cluster fallback.
use anyhow::Result;
use clap::Parser;
use proofstorm_acceptance::{gates, runner};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(about = "Run live gates in a new, owned Proofstorm installation")]
struct Arguments {
    /// Opt-in O1 pilot model, in `OpenCode` provider/model form.
    #[arg(long)]
    benchmark_model: Option<String>,
    /// Headless `OpenCode` executable for the opt-in pilot.
    #[arg(long, default_value = "opencode")]
    benchmark_opencode: PathBuf,
    /// Rescore retained O1 evidence without contacting a model or runtime.
    #[arg(long)]
    benchmark_grade: Option<PathBuf>,
    #[arg(long, hide = true)]
    benchmark_proxy: Option<PathBuf>,
    /// Catalog-derived qualification plan for a single isolated case.
    #[arg(long, requires = "qualification_case")]
    qualification_plan: Option<PathBuf>,
    #[arg(long, requires = "qualification_plan")]
    qualification_case: Option<String>,
    /// Verified checkout artifact source. Its runtime is never used or modified.
    #[arg(long, conflicts_with = "bundle")]
    checkout_home: Option<PathBuf>,
    /// Verified unpacked release bundle (not a source checkout).
    #[arg(long)]
    bundle: Option<PathBuf>,
    /// Permit an explicitly selected development bundle.
    #[arg(long)]
    allow_development: bool,
    /// New directory for installation state, logs and report; retained after cleanup.
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Retry owned runtime cleanup using a previous run's retained receipt.
    #[arg(long)]
    cleanup: Option<PathBuf>,
    /// Repository fixtures for gates that need them.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Maximum seconds for each gate (setup has its own bounded operations).
    #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..=14400))]
    timeout: u64,
    #[arg(long)]
    list: bool,
    /// Parent-owned worker home; not an existing-installation test mode.
    #[arg(long, hide = true)]
    worker_home: Option<PathBuf>,
    /// Named gates; defaults to the small Bitcoin smoke test.
    gates: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Arguments::parse();
    if let Some(path) = args.benchmark_proxy {
        return proofstorm_acceptance::benchmark::proxy::serve(&path);
    }
    if let Some(path) = args.benchmark_grade {
        println!("{}", proofstorm_acceptance::benchmark::regrade(&path)?);
        return Ok(());
    }
    if args.list {
        for name in gates::NAMES {
            println!("{name}");
        }
        return Ok(());
    }
    if let Some(work) = args.cleanup {
        let result = runner::cleanup(&work);
        if work.join("benchmark-task.json").exists() {
            println!("{}", proofstorm_acceptance::benchmark::finalize(&work)?);
        }
        return result;
    }
    let selection = runner::Selection {
        benchmark_model: args.benchmark_model,
        benchmark_opencode: args.benchmark_opencode,
        qualification: args.qualification_plan.zip(args.qualification_case),
        checkout_home: args.checkout_home,
        bundle: args.bundle,
        allow_development: args.allow_development,
    };
    let root = args.root.unwrap_or(std::env::current_dir()?);
    let names = if args.gates.is_empty() {
        vec!["smoke".into()]
    } else {
        args.gates
    };
    runner::validate_gates(&names)?;
    let benchmark = names.iter().any(|n| n == "benchmark-o1");
    if benchmark {
        anyhow::ensure!(
            names.len() == 1 && selection.benchmark_model.is_some(),
            "benchmark-o1 requires --benchmark-model and its own run"
        );
        anyhow::ensure!(
            args.work_dir.is_some() || args.worker_home.is_some(),
            "benchmark requires --work-dir to retain every attempt"
        );
    }
    if let Some(home) = args.worker_home {
        anyhow::ensure!(names.len() == 1, "worker requires exactly one gate");
        return runner::worker(&selection, &root, &home, &names[0]);
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("register SIGTERM");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        eprintln!(
            "Interrupted. Finishing the current setup operation, then cleaning up the owned runtime..."
        );
        signal.store(true, Ordering::SeqCst);
    });
    tokio::task::spawn_blocking(move || {
        let result = runner::run(
            &selection,
            &root,
            args.work_dir.as_deref(),
            &names,
            args.timeout,
            &cancelled,
        );
        if benchmark
            && let Some(work) = &args.work_dir
            && work.join("acceptance.json").exists()
        {
            let score = proofstorm_acceptance::benchmark::finalize(work)?;
            println!("{}", serde_json::to_string_pretty(&score)?);
        }
        result
    })
    .await?
}
