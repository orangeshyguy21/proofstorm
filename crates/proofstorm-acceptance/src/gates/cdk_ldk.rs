//! CDK 0.18.0 embedded LDK: BOLT12 offer quoting through the mint's own HTTP
//! API, an inbound CLN peer connection to the embedded node, and, on the
//! PostgreSQL variant, quote survival across database and mint restarts.
//!
//! Ported from `tests/kubernetes/cdk_ldk_mcp_client.py`. Serves both the
//! `cdk-ldk` and `cdk-ldk-postgres` targets; `PROOFSTORM_STORAGE` selects which.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, http, json as expect, lab, postgres};

const INSTANCE: &str = "cdk-ldk-instance";
const DRAFT: &str = "cdk-ldk";
const DATABASE: &str = "proofstorm_ldk";
const MARKER: &str = "ldk-persistent";
const IMAGE: &str = "docker.io/cashubtc/mintd@sha256:2b0e9ff0430710b5c3df93cfaccdea01ffa2efc6d66c50daca4730f0c542d9be";

fn lab_document(postgres_enabled: bool) -> Value {
    let mut lab = json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-ldk-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {"id": "peer", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "laboratory", "config": {"alias": "proofstorm-ldk-introduction-peer"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk-ldk", "version": "0.18.0", "config_version": "cdk-mintd-ldk/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK LDK", "description": "Native CDK embedded-LDK BOLT12 lab"}}
        ],
        "links": [
            {"id": "peer-chain", "kind": "chain_backend", "from": "peer", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    });
    postgres::augment_lab(postgres_enabled, &mut lab, DATABASE);
    lab
}

/// Pull the embedded node's public key out of the mint's bounded startup logs.
fn ldk_node_id(logs: &str) -> Option<&str> {
    let marker = "Created node ";
    let start = logs.find(marker)? + marker.len();
    let candidate = logs.get(start..start + 66)?;
    candidate
        .chars()
        .all(|character| character.is_ascii_hexdigit())
        .then_some(candidate)
}

pub fn run(context: &GateContext, postgres_enabled: bool) -> Result<()> {
    let mut client = context.session("cdk-ldk-live", "designer", LIFECYCLE_CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(postgres_enabled), "idempotency_key": "create-cdk-ldk"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-cdk-ldk", "include_revision": true}),
    )?;
    let entry = lab::lock_entry(&published, "cdk-ldk")?;
    expect::equals(entry, "/version", &Value::from("0.18.0"))?;
    expect::equals(entry, "/image", &Value::from(IMAGE))?;

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-cdk-ldk"}),
    )?;
    let ready = lab::wait_ready(&mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in [
        "[payment_backend]\nbackend = \"ldk-node\"",
        "chain_source_type = \"bitcoinrpc\"",
        "bitcoind_rpc_host = \"chain\"",
        "ldk_node_host = \"0.0.0.0\"",
        "ldk_node_port = 9735",
    ] {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }
    if config.contains("[lnd]") || config.contains("[cln]") {
        bail!("embedded-LDK lab rendered an external Lightning stanza");
    }
    postgres::assert_materialized(
        postgres_enabled,
        &context.kubectl,
        namespace,
        &config,
        DATABASE,
    )?;

    let version =
        context
            .kubectl
            .exec(namespace, "deployment/mint", &["cdk-mintd", "--version"])?;
    if !version.contains("0.18.0") {
        bail!("live mint reports the wrong version: {version:?}");
    }

    let logs = context
        .kubectl
        .run(&["logs", "deployment/mint", "-n", namespace])?;
    let node_id = ldk_node_id(&logs).ok_or_else(|| {
        anyhow::anyhow!("could not discover embedded LDK node identity from bounded startup logs")
    })?;
    context.kubectl.exec(
        namespace,
        "statefulset/peer",
        &[
            "lightning-cli",
            "--lightning-dir=/home/cln/.lightning",
            "--network=regtest",
            "connect",
            &format!("{node_id}@mint:9735"),
        ],
    )?;
    sleep(Duration::from_secs(2));

    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let info = http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    if !serde_json::to_string(&info)?
        .to_lowercase()
        .contains("bolt12")
    {
        bail!("live mint does not advertise BOLT12: {info}");
    }

    let quote = http::post_json(
        &forward.url("/v1/mint/quote/bolt12"),
        &json!({
            "amount": 100,
            "unit": "sat",
            "description": "Proofstorm BOLT12 acceptance",
            "pubkey": "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        }),
    )?;
    if !expect::string(&quote, "/request")?
        .to_lowercase()
        .starts_with("lno")
    {
        bail!("BOLT12 quote did not return an offer: {quote}");
    }
    if expect::string(&quote, "/unit")? != "sat" || expect::integer(&quote, "/amount")? != 100 {
        bail!("BOLT12 quote returned unexpected terms: {quote}");
    }

    if postgres_enabled {
        let quote_id = expect::string(&quote, "/quote")?.to_string();
        postgres::seed_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;
        postgres::restart_database(postgres_enabled, &context.kubectl, namespace)?;
        context
            .kubectl
            .rollout_restart(namespace, "deployment/mint")?;
        postgres::verify_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;

        let recovered = http::get_json_retrying(
            &mut forward,
            &format!("/v1/mint/quote/bolt12/{quote_id}"),
            30,
        )
        .map_err(|error| {
            anyhow::anyhow!("BOLT12 quote did not survive PostgreSQL and mint restarts: {error}")
        })?;
        if expect::string(&recovered, "/quote")? != quote_id {
            bail!("recovered BOLT12 quote changed identity: {recovered}");
        }
    }

    drop(forward);

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_closed(&mut client, INSTANCE)?;

    if postgres_enabled {
        println!("CDK embedded LDK + PostgreSQL MCP BOLT12 persistence and teardown passed");
    } else {
        println!(
            "CDK 0.18.0 embedded-LDK MCP materialization, database-backed configuration, BOLT12 quote, readiness, and teardown passed"
        );
    }
    Ok(())
}
