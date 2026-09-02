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
        .unwrap_or_else(|_| "all".into())
        .parse::<ProofstormToolset>()
        .map_err(anyhow::Error::msg)?;
    let Ok(database_path) = std::env::var("PROOFSTORM_DB") else {
        return Ok(ProofstormMcp::default().with_toolset(toolset));
    };
    let workspace = std::env::var("PROOFSTORM_WORKSPACE")
        .context("PROOFSTORM_WORKSPACE is required with PROOFSTORM_DB")?;
    let principal = std::env::var("PROOFSTORM_PRINCIPAL")
        .context("PROOFSTORM_PRINCIPAL is required with PROOFSTORM_DB")?;
    let encoded_capabilities = std::env::var("PROOFSTORM_CAPABILITIES")
        .context("PROOFSTORM_CAPABILITIES is required with PROOFSTORM_DB")?;
    let capabilities = encoded_capabilities
        .split(',')
        .filter(|value| !value.is_empty())
        .map(|value| {
            serde_json::from_value::<Capability>(serde_json::Value::String(value.to_owned()))
                .with_context(|| format!("invalid Proofstorm capability {value:?}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let store = Store::open(database_path)?;
    store.put_workspace(&Workspace {
        id: workspace.clone(),
        name: workspace.clone(),
    })?;
    store.put_principal(&principal)?;
    store.replace_grants(&workspace, &principal, capabilities)?;
    let service = ProofstormMcp::new(store, workspace, principal)?.with_toolset(toolset);
    let Ok(control_namespace) = std::env::var("PROOFSTORM_CONTROL_NAMESPACE") else {
        return Ok(service);
    };
    let client = kube::Client::try_default()
        .await
        .context("connect to Kubernetes for Proofstorm materialization")?;
    Ok(service.with_kubernetes(client, control_namespace))
}
