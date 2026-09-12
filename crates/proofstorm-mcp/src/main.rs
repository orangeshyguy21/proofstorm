use anyhow::Context;
use clap::Parser;
use futures::{StreamExt, future};
use proofstorm_core::Capability;
use proofstorm_mcp::{ProofstormMcp, ProofstormToolset};
use proofstorm_store::{Store, Workspace};
use rmcp::{
    RoleServer, ServiceExt,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::async_rw::JsonRpcMessageCodec,
};
use tokio::io::{stdin, stdout};
use tokio_util::codec::{FramedRead, FramedWrite};

const MAX_MCP_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Parser)]
#[command(name = "proofstorm-mcp", version)]
struct Args {
    /// Report embedded release contents and exit; do not start the MCP transport.
    #[arg(long)]
    release_info: bool,
    /// Select an initialized isolated installation independent of working directory.
    #[arg(long, env = "PROOFSTORM_HOME")]
    home: Option<std::path::PathBuf>,
    /// Explicit kubeconfig; never fall back to the user's configuration.
    #[arg(long, env = "PROOFSTORM_KUBECONFIG")]
    kubeconfig: Option<std::path::PathBuf>,
    /// Managed project actor; ignores ambient Proofstorm configuration and never grants permissions.
    #[arg(long, requires = "home")]
    attachment: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.release_info {
        println!(
            "{}",
            serde_json::to_string_pretty(&proofstorm_app::release::describe())?
        );
        return Ok(());
    }
    let service = configured_service(args).await?;
    let requests = FramedRead::new(
        stdin(),
        JsonRpcMessageCodec::<RxJsonRpcMessage<RoleServer>>::new_with_max_length(
            MAX_MCP_FRAME_BYTES,
        ),
    )
    .take_while(|result| future::ready(result.is_ok()))
    .filter_map(|result| future::ready(result.ok()));
    let replies = FramedWrite::new(
        stdout(),
        JsonRpcMessageCodec::<TxJsonRpcMessage<RoleServer>>::new(),
    );
    let server = service.serve((replies, requests)).await?;
    server.waiting().await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "keep managed and manual startup policy together for auditability"
)]
async fn configured_service(args: Args) -> anyhow::Result<ProofstormMcp> {
    if let Some(home) = &args.home {
        proofstorm_app::artifacts::check_checkout(home)?;
    }
    let attached = args.attachment.is_some();
    let toolset = if attached {
        "developer".to_owned()
    } else {
        std::env::var("PROOFSTORM_TOOLSET").unwrap_or_else(|_| "developer".into())
    }
    .parse::<ProofstormToolset>()
    .map_err(anyhow::Error::msg)?;
    let environment = proofstorm_app::config::Environment::resolve(
        |key| match key {
            "PROOFSTORM_HOME" => args
                .home
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "PROOFSTORM_KUBECONFIG" if attached => None,
            "PROOFSTORM_KUBECONFIG" => args
                .kubeconfig
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "PROOFSTORM_PRINCIPAL" if attached => args.attachment.clone(),
            _ if attached => None,
            _ => std::env::var(key).ok(),
        },
        &std::env::current_dir()?,
    )?;
    environment.report();
    if environment.mode == proofstorm_app::config::Mode::Memory {
        let store = Store::memory()?;
        store.put_workspace(&Workspace {
            id: environment.workspace.clone(),
            name: environment.workspace.clone(),
        })?;
        store.put_principal(&environment.principal)?;
        store.replace_grants(
            &environment.workspace,
            &environment.principal,
            [Capability::CatalogRead, Capability::LabValidate],
        )?;
        return Ok(
            ProofstormMcp::new(store, &environment.workspace, &environment.principal)?
                .with_toolset(toolset)
                .offline(),
        );
    }
    if attached {
        anyhow::ensure!(
            std::fs::symlink_metadata(&environment.database)?.is_file(),
            "managed attachment needs an existing installation database"
        );
    } else if let Some(parent) = environment.database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&environment.database)?;
    let workspace = environment.workspace.clone();
    let principal = environment.principal.clone();
    if attached {
        anyhow::ensure!(
            store.actor_preset(&workspace, &principal)?.as_deref()
                == Some(proofstorm_app::developer::PRESET),
            "managed actor is not configured; run proofstorm agent configure with your agent name for this project"
        );
    } else if let Ok(encoded) = std::env::var("PROOFSTORM_CAPABILITIES") {
        let capabilities = encoded
            .split(',')
            .filter(|value| !value.is_empty())
            .map(|value| {
                serde_json::from_value::<Capability>(serde_json::Value::String(value.to_owned()))
                    .with_context(|| format!("invalid Proofstorm capability {value:?}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        store.put_workspace(&Workspace {
            id: workspace.clone(),
            name: workspace.clone(),
        })?;
        store.put_principal(&principal)?;
        store.replace_grants(&workspace, &principal, capabilities)?;
    } else if store.capabilities(&workspace, &principal)?.is_empty() {
        anyhow::bail!(
            "identity {principal:?} has no configured grants in {workspace:?}; supply operator-owned PROOFSTORM_CAPABILITIES or configure this identity with proofstorm dev init --principal {principal}"
        );
    }
    let service = ProofstormMcp::new(store.clone(), workspace.clone(), principal.clone())?
        .with_toolset(toolset);
    if environment.mode == proofstorm_app::config::Mode::Offline {
        return Ok(service.offline());
    }
    if attached {
        proofstorm_app::bootstrap::check_installed_runtime(
            environment
                .installation
                .as_ref()
                .context("managed attachment requires an installation")?,
        )?;
    }
    let runtime = environment.runtime().await?;
    // Managed startup/verification stays passive. Explicit mutations reconcile their
    // own durable intent; CLI-owned recovery remains available for interrupted work.
    let _recovery = (!attached).then(|| {
        proofstorm_app::updates::start_recovery(runtime.clone(), store, workspace, principal)
    });
    Ok(if let Some(installation) = &environment.installation {
        service.with_installation_runtime(runtime, installation)
    } else {
        service.with_runtime(runtime)
    })
}
