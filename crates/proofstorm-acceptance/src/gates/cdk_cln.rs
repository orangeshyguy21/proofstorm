//! CDK 0.18.0 + Core Lightning materialization, database-backed configuration,
//! native socket configuration, readiness, and verified teardown.
//!
//! Ported from `tests/kubernetes/cdk_cln_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, json as expect, lab};

const INSTANCE: &str = "cdk-cln-instance";
const DRAFT: &str = "cdk-cln";
const IMAGE: &str = "docker.io/cashubtc/mintd@sha256:fd938da187fb9fce82627ced6d419e675dbd6db5f0d50dc6930b1f6e18c359f0";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-cln-live-lab",
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
                "id": "mint-cln",
                "kind": "lightning",
                "implementation": "cln",
                "version": "26.06.7",
                "config_version": "cln/26.06/v1",
                "control": "laboratory",
                "config": {"alias": "proofstorm-mint-cln"}
            },
            {
                "id": "mint",
                "kind": "mint",
                "implementation": "cdk",
                "version": "0.18.0",
                "config_version": "cdk-mintd/0.18/v1",
                "control": "target",
                "config": {
                    "name": "Proofstorm CDK CLN",
                    "description": "Native CDK and CLN lab"
                }
            }
        ],
        "links": [
            {
                "id": "mint-cln-chain",
                "kind": "chain_backend",
                "from": "mint-cln",
                "to": "chain",
                "binding": {"type": "chain", "network": "regtest"}
            },
            {
                "id": "mint-cln-bolt11",
                "kind": "payment_backend",
                "from": "mint",
                "to": "mint-cln",
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
    let mut client = context.session("cdk-cln-live", "designer", LIFECYCLE_CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({
            "draft_id": DRAFT,
            "lab": lab_document(),
            "idempotency_key": "create-cdk-cln"
        }),
    )?;

    let published = client.call(
        "proofstorm_lab_publish",
        json!({
            "draft_id": DRAFT,
            "expected_version": 1,
            "idempotency_key": "publish-cdk-cln",
            "include_revision": true
        }),
    )?;

    let entry = lab::lock_entry(&published, "cdk")?;
    expect::equals(entry, "/version", &Value::from("0.18.0"))?;
    expect::equals(entry, "/image", &Value::from(IMAGE))?;

    client.call(
        "proofstorm_lab_materialize",
        json!({
            "instance_id": INSTANCE,
            "revision_digest": expect::string(&published, "/digest")?,
            "idempotency_key": "materialize-cdk-cln"
        }),
    )?;

    let ready = lab::wait_ready(&mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in [
        "[payment_backend]\nbackend = \"cln\"",
        "rpc_path = \"/cln/regtest/lightning-rpc\"",
        "bolt12 = false",
    ] {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }
    if config.contains("[lnd]") {
        bail!("CLN lab rendered an LND stanza: {config}");
    }

    let version =
        context
            .kubectl
            .exec(namespace, "deployment/mint", &["cdk-mintd", "--version"])?;
    if !version.contains("0.18.0") {
        bail!("live mint reports the wrong version: {version:?}");
    }

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_closed(&mut client, INSTANCE)?;

    println!(
        "CDK 0.18.0 + CLN MCP materialization, database-backed configuration, native socket configuration, readiness, and teardown passed"
    );
    Ok(())
}
