//! MCP fixture for live agent/provider schema compatibility checks.
//!
//! Advertises the complete production tool catalog with an in-memory store.
//! Catalog reads work; runtime operations fail because no cluster is attached.
//! Point an agent's MCP command at this example, then ask it to call `catalog_list`.
use proofstorm_mcp::ProofstormMcp;
use proofstorm_store::Store;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Store::memory()?;
    proofstorm_app::developer::configure(&store, "schema-smoke", "agent")?;
    let service = ProofstormMcp::new(store, "schema-smoke", "agent")?;
    assert_eq!(
        service.tool_names().len(),
        proofstorm_core::mcp::TOOLS.len()
    );
    service
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
