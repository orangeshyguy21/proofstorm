//! Slice 4: MCP-driven create, publish, materialize, readiness, sanitized
//! status, and verified close.
//!
//! Ported from `tests/kubernetes/slice4_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, json as expect, lab};

const INSTANCE: &str = "slice4-instance";
const DRAFT: &str = "slice4";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "slice4-static-lab",
        "components": [
            {
                "id": "chain",
                "kind": "bitcoin",
                "implementation": "bitcoin-core",
                "version": "30.0",
                "config_version": "bitcoin-core/30/v1",
                "control": "laboratory",
                "config": {"txindex": true, "fallback_fee": 0.0002}
            },
            {
                "id": "lightning",
                "kind": "lightning",
                "implementation": "lnd",
                "version": "0.20.0-beta",
                "config_version": "lnd/0.20/v1",
                "control": "laboratory",
                "config": {"alias": "proofstorm-lightning"}
            },
            {
                "id": "mint",
                "kind": "mint",
                "implementation": "cdk",
                "version": "0.18.0",
                "config_version": "cdk-mintd/0.18/v1",
                "control": "target",
                "config": {
                    "name": "Proofstorm Slice 4",
                    "description": "MCP-created static lab"
                }
            }
        ],
        "links": [
            {
                "id": "lightning-chain",
                "kind": "chain_backend",
                "from": "lightning",
                "to": "chain",
                "binding": {"type": "chain", "network": "regtest"}
            },
            {
                "id": "mint-bolt11",
                "kind": "payment_backend",
                "from": "mint",
                "to": "lightning",
                "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}
            }
        ],
        "policy": {
            "allow": [],
            "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}
        }
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session("slice4-live", "designer", LIFECYCLE_CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({
            "draft_id": DRAFT,
            "lab": lab_document(),
            "idempotency_key": "create-slice4"
        }),
    )?;

    let published = client.call(
        "proofstorm_lab_publish",
        json!({
            "draft_id": DRAFT,
            "expected_version": 1,
            "idempotency_key": "publish-slice4",
            "include_revision": true
        }),
    )?;

    for entry in expect::array(&published, "/lock/entries")? {
        let image = expect::string(entry, "/image")?;
        if !image.contains("@sha256:") {
            bail!("published lock contains an unpinned image: {image}");
        }
    }

    client.call(
        "proofstorm_lab_materialize",
        json!({
            "instance_id": INSTANCE,
            "revision_digest": expect::string(&published, "/digest")?,
            "idempotency_key": "materialize-slice4"
        }),
    )?;

    let ready = lab::wait_ready(&mut client, INSTANCE)?;

    let components = client.call(
        "proofstorm_lab_component_status_list",
        json!({"instance_id": INSTANCE, "limit": 50}),
    )?;
    let mut ready_ids = Vec::new();
    for component in expect::array(&components, "/components")? {
        if expect::boolean(component, "/ready")? {
            ready_ids.push(expect::string(component, "/id")?.to_string());
        }
    }
    ready_ids.sort();
    if ready_ids != ["chain", "lightning", "mint"] {
        bail!("sanitized topology is not ready: {components}");
    }

    let encoded = serde_json::to_string(&ready)?;
    if encoded.contains("macaroon") || encoded.contains("proofstorm-regtest-only") {
        bail!("sanitized status leaked a credential");
    }

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    let closed = lab::wait_closed(&mut client, INSTANCE)?;

    if !expect::boolean(&closed, "/teardown_receipt/verified_absent")? {
        bail!("teardown receipt did not record verified absence: {closed}");
    }
    expect::string(&closed, "/teardown_receipt/inventory_digest")?;

    println!("Slice 4 MCP materialization, readiness, and verified close acceptance passed");
    Ok(())
}
