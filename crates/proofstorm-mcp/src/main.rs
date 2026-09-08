use anyhow::Context;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = configured_service().await?;
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

async fn configured_service() -> anyhow::Result<ProofstormMcp> {
    let toolset = std::env::var("PROOFSTORM_TOOLSET")
        .unwrap_or_else(|_| "developer".into())
        .parse::<ProofstormToolset>()
        .map_err(anyhow::Error::msg)?;
    let environment = proofstorm_app::config::Environment::resolve(
        |key| std::env::var(key).ok(),
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
    if let Some(parent) = environment.database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&environment.database)?;
    let workspace = environment.workspace.clone();
    let principal = environment.principal.clone();
    if let Ok(encoded) = std::env::var("PROOFSTORM_CAPABILITIES") {
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
            "identity {principal:?} has no configured grants in {workspace:?}; supply operator-owned PROOFSTORM_CAPABILITIES or configure this identity with proofstorm init --principal {principal}"
        );
    }
    let service = ProofstormMcp::new(store.clone(), workspace.clone(), principal.clone())?
        .with_toolset(toolset);
    if environment.mode == proofstorm_app::config::Mode::Offline {
        return Ok(service.offline());
    }
    let runtime = environment.runtime().await?;
    let _recovery =
        proofstorm_app::updates::start_recovery(runtime.clone(), store, workspace, principal);
    Ok(service.with_runtime(runtime))
}
