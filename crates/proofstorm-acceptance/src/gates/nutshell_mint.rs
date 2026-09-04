//! Nutshell 0.20.3 + LND materialization, typed configuration, generated key,
//! readiness, and verified teardown.
//!
//! Ported from `tests/kubernetes/nutshell_mint_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, json as expect, lab};

/// The driver runs inside the mint's own image, which ships the `cashu` library.
const SETTINGS_DRIVER: &str = include_str!("../../drivers/nutshell_settings.py");

const INSTANCE: &str = "nutshell-mint-instance";
const DRAFT: &str = "nutshell-mint";
const IMAGE: &str = "docker.io/cashubtc/nutshell@sha256:f039b0e61f64d67c7212f5472eb5d021c3703cd9e72170aa924906ce6bd1f2ed";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-mint-live-lab",
        "components": [
            {
                "id": "chain",
                "kind": "bitcoin",
                "implementation": "bitcoin-core",
                "version": "30.0",
                "config_version": "bitcoin-core/30/v1",
                "control": "laboratory",
                "config": {}
            },
            {
                "id": "lightning",
                "kind": "lightning",
                "implementation": "lnd",
                "version": "0.20.0-beta",
                "config_version": "lnd/0.20/v1",
                "control": "laboratory",
                "config": {"alias": "proofstorm-nutshell-lnd"}
            },
            {
                "id": "mint",
                "kind": "mint",
                "implementation": "nutshell",
                "version": "0.20.3",
                "config_version": "nutshell-mint/0.20/v1",
                "control": "target",
                "config": {
                    "name": "Proofstorm Nutshell Native",
                    "description": "Native Nutshell and LND lab",
                    "input_fee_ppk": 123,
                    "mint_quote_ttl_seconds": 321,
                    "melt_quote_ttl_seconds": 123,
                    "max_mint_sat": 400_000,
                    "max_melt_sat": 300_000,
                    "max_balance_sat": 9_000_000,
                    "global_rate_limit_per_minute": 77,
                    "transaction_rate_limit_per_minute": 33,
                    "lightning_fee_percent": 0.5,
                    "lightning_reserve_fee_min_sat": 7
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
                "id": "mint-lightning-bolt11",
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

fn expected_settings() -> Value {
    json!({
        "version": "0.20.3",
        "name": "Proofstorm Nutshell Native",
        "description": "Native Nutshell and LND lab",
        "input_fee_ppk": 123,
        "mint_quote_ttl": 321,
        "melt_quote_ttl": 123,
        "max_mint_sat": 400_000,
        "max_melt_sat": 300_000,
        "max_balance_sat": 9_000_000,
        "global_rate_limit": 77,
        "transaction_rate_limit": 33,
        "lightning_fee_percent": 0.5,
        "lightning_reserve_fee_min": 7000,
        "backend": "LndRestWallet",
        "lnd_endpoint": "https://lightning:8080",
        "database": "/app/data",
        "private_key_length": 64
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session("nutshell-mint-live", "designer", LIFECYCLE_CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({
            "draft_id": DRAFT,
            "lab": lab_document(),
            "idempotency_key": "create-nutshell-mint"
        }),
    )?;

    let published = client.call(
        "proofstorm_lab_publish",
        json!({
            "draft_id": DRAFT,
            "expected_version": 1,
            "idempotency_key": "publish-nutshell-mint",
            "include_revision": true
        }),
    )?;

    let entry = lab::lock_entry(&published, "nutshell")?;
    expect::equals(entry, "/version", &Value::from("0.20.3"))?;
    expect::equals(
        entry,
        "/config_version",
        &Value::from("nutshell-mint/0.20/v1"),
    )?;
    expect::equals(entry, "/image", &Value::from(IMAGE))?;

    client.call(
        "proofstorm_lab_materialize",
        json!({
            "instance_id": INSTANCE,
            "revision_digest": expect::string(&published, "/digest")?,
            "idempotency_key": "materialize-nutshell-mint"
        }),
    )?;

    let ready = lab::wait_ready(&mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let rendered = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["python3", "-c", SETTINGS_DRIVER],
    )?;
    let settings: Value = serde_json::from_str(rendered.trim())?;
    let expected = expected_settings();
    if settings != expected {
        bail!("live Nutshell settings differ: expected={expected} actual={settings}");
    }

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_closed(&mut client, INSTANCE)?;

    println!(
        "Nutshell 0.20.3 + LND MCP materialization, typed configuration, generated key, readiness, and teardown passed"
    );
    Ok(())
}
