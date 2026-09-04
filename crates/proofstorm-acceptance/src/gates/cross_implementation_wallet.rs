//! CDK and Redis-backed Nutshell run the identical wallet workflow: NUT-20
//! interoperability, Redis cache use, secret stability across a controller
//! restart, ephemeral cache loss across a cache restart, recovery, and
//! verified teardown.
//!
//! Ported from `tests/kubernetes/cross_implementation_wallet_mcp_client.py`.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{EXPERIMENT_CAPABILITIES, GateContext, gate::CONTROL_NAMESPACE, json as expect, lab};

const CACHE_DRIVER: &str = include_str!("../../drivers/nutshell_redis_settings.py");

const INSTANCE: &str = "cross-mint-wallet-instance";
const EXPERIMENT: &str = "cross-mint-experiment";
const LEASE: &str = "cross-mint-lease";
const DRAFT: &str = "cross-mint-wallet";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cross-mint-wallet-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cross-mint"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cross-payer"}},
            {"id": "cache", "kind": "database", "implementation": "redis", "version": "8.10.1", "config_version": "redis/8.10/v1", "control": "laboratory", "config": {"maxmemory_mb": 64}},
            {"id": "cdk-mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK Cross-Parity", "description": "Cross-implementation wallet acceptance"}},
            {"id": "nutshell-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell Cross-Parity", "description": "Cross-implementation wallet acceptance", "redis_cache_ttl_seconds": 900}},
            {"id": "cdk-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
            {"id": "nutshell-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}
        ],
        "links": [
            {"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "cdk-bolt11", "kind": "payment_backend", "from": "cdk-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "nutshell-bolt11", "kind": "payment_backend", "from": "nutshell-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "nutshell-cache", "kind": "database_backend", "from": "nutshell-mint", "to": "cache", "binding": {"type": "database", "role": "cache"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

fn redis(context: &GateContext, namespace: &str, command: &str) -> Result<String> {
    let script = format!("redis-cli --no-auth-warning -a \"$REDIS_PASSWORD\" {command}");
    context
        .kubectl
        .exec(namespace, "deployment/cache", &["sh", "-c", &script])
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session(
        "cross-mint-wallet-live",
        "experiment-agent",
        EXPERIMENT_CAPABILITIES,
    )?;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-cross-mint-wallet"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-cross-mint-wallet", "include_revision": true}),
    )?;

    for (component, catalog_id, version, config_version) in [
        ("cache", "redis", "8.10.1", "redis/8.10/v1"),
        ("cdk-mint", "cdk", "0.18.0", "cdk-mintd/0.18/v1"),
        (
            "nutshell-mint",
            "nutshell",
            "0.20.3",
            "nutshell-mint/0.20/v1",
        ),
    ] {
        let entry = expect::array(&published, "/lock/entries")?
            .iter()
            .find(|entry| entry.get("component_id").and_then(Value::as_str) == Some(component))
            .ok_or_else(|| anyhow::anyhow!("no lock entry for {component}"))?;
        if expect::string(entry, "/catalog_id")? != catalog_id
            || expect::string(entry, "/version")? != version
            || expect::string(entry, "/config_version")? != config_version
            || !expect::string(entry, "/image")?.contains("@sha256:")
        {
            bail!("unexpected pinned lock for {component}: {entry}");
        }
    }

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-cross-mint-wallet"}),
    )?;
    let ready = lab::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let components = client.call(
        "proofstorm_lab_component_status_list",
        json!({"instance_id": INSTANCE, "limit": 50}),
    )?;
    let mut actual: Vec<&str> = expect::array(&components, "/components")?
        .iter()
        .filter(|component| component.get("ready").and_then(Value::as_bool) == Some(true))
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    actual.sort_unstable();
    let mut wanted = [
        "cache",
        "cdk-mint",
        "cdk-wallet",
        "chain",
        "mint-lnd",
        "nutshell-mint",
        "nutshell-wallet",
        "payer-lnd",
    ];
    wanted.sort_unstable();
    if actual != wanted {
        bail!("cross-implementation topology is not fully ready: {components}");
    }

    let public_config =
        context
            .kubectl
            .get_json(&["get", "configmap/nutshell-mint-config", "-n", namespace])?;
    for (key, value) in [
        ("MINT_REDIS_CACHE_ENABLED", "TRUE"),
        ("MINT_REDIS_CACHE_TTL", "900"),
        ("MINT_REDIS_CACHE_CLUSTER", "FALSE"),
    ] {
        expect::equals(&public_config, &format!("/data/{key}"), &Value::from(value))?;
    }
    let data = expect::object(&public_config, "/data")?;
    if data.contains_key("MINT_REDIS_CACHE_URL")
        || data
            .values()
            .any(|value| value.as_str().is_some_and(|text| text.contains("redis://")))
    {
        bail!("public Nutshell configuration contains the private Redis URL");
    }

    let cache_secret_args = [
        "get",
        "secret/cache-credentials",
        "-n",
        namespace,
        "-o",
        "json",
    ];
    let cache_secret_digest = context.kubectl.digest(&cache_secret_args)?;
    let cache_secret =
        context
            .kubectl
            .get_json(&["get", "secret/cache-credentials", "-n", namespace])?;
    let mut keys: Vec<&str> = expect::object(&cache_secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    if keys != ["PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"] {
        bail!("generated Redis Secret has an unexpected key contract: {keys:?}");
    }

    let rendered = context.kubectl.exec(
        namespace,
        "deployment/nutshell-mint",
        &["python3", "-c", CACHE_DRIVER],
    )?;
    let cache_settings: Value = serde_json::from_str(rendered.trim())?;
    let expected_cache = json!({
        "enabled": true,
        "host": "cache",
        "password_length": 64,
        "ttl": 900,
        "cluster": false
    });
    if cache_settings != expected_cache {
        bail!("live Nutshell Redis settings differ: {cache_settings}");
    }

    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": EXPERIMENT, "instance_id": INSTANCE, "idempotency_key": "create-cross-mint-experiment"}),
    )?;
    client.call(
        "proofstorm_lease_acquire",
        json!({"experiment_id": EXPERIMENT, "lease_id": LEASE, "duration_seconds": 1200, "max_actions": 12, "idempotency_key": "acquire-cross-mint-lease"}),
    )?;

    client.call(
        "proofstorm_liquidity_bootstrap",
        json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "lease_id": LEASE,
            "operation_id": "cross-mint-bootstrap", "chain": "chain",
            "mint_lightning": "mint-lnd", "payer_lightning": "payer-lnd",
            "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 5_000_000,
            "idempotency_key": "bootstrap-cross-mint"
        }),
    )?;
    let bootstrap = lab::wait_operation(&mut client, "cross-mint-bootstrap", 160)?;
    if !expect::boolean(lab::artifact_content(&bootstrap)?, "/ready")? {
        bail!("liquidity bootstrap artifact is invalid: {bootstrap}");
    }

    for (implementation, mint, wallet) in [
        ("cdk", "cdk-mint", "cdk-wallet"),
        ("nutshell", "nutshell-mint", "nutshell-wallet"),
    ] {
        let prefix = format!("{implementation}-wallet");
        let common = json!({
            "instance_id": INSTANCE, "experiment_id": EXPERIMENT, "lease_id": LEASE,
            "wallet": wallet, "mint": mint
        });
        let merge = |extra: Value| -> Value {
            let mut base = common.clone();
            if let (Some(target), Value::Object(source)) = (base.as_object_mut(), extra) {
                for (key, value) in source {
                    target.insert(key, value);
                }
            }
            base
        };

        client.call(
            "proofstorm_wallet_initialize",
            merge(json!({"operation_id": format!("{prefix}-initialize"), "idempotency_key": format!("{prefix}-initialize")})),
        )?;
        let initialized = lab::wait_operation(&mut client, &format!("{prefix}-initialize"), 160)?;
        if !expect::boolean(lab::artifact_content(&initialized)?, "/initialized")? {
            bail!("{implementation} wallet initialization failed: {initialized}");
        }

        client.call(
            "proofstorm_wallet_balance",
            merge(json!({"operation_id": format!("{prefix}-balance"), "idempotency_key": format!("{prefix}-balance")})),
        )?;
        let balance = lab::wait_operation(&mut client, &format!("{prefix}-balance"), 160)?;
        if expect::integer(lab::artifact_content(&balance)?, "/balance_sat")? != 0 {
            bail!("{implementation} wallet did not start empty: {balance}");
        }

        client.call(
            "proofstorm_wallet_fund",
            merge(json!({"operation_id": format!("{prefix}-fund"), "payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": format!("{prefix}-fund")})),
        )?;
        let funded = lab::wait_operation(&mut client, &format!("{prefix}-fund"), 160)?;
        let fund_content = lab::artifact_content(&funded)?;
        if expect::integer(fund_content, "/funded_sat")? != 1000
            || expect::integer(fund_content, "/balance_sat")? != 1000
        {
            bail!("{implementation} wallet funding failed: {funded}");
        }

        let baseline_id = format!("{prefix}-balance-before-round-trip");
        client.call(
            "proofstorm_wallet_balance",
            merge(json!({"operation_id": baseline_id, "idempotency_key": format!("{prefix}-balance-before-round-trip")})),
        )?;
        let baseline = lab::wait_operation(
            &mut client,
            &format!("{prefix}-balance-before-round-trip"),
            160,
        )?;
        if expect::integer(lab::artifact_content(&baseline)?, "/balance_sat")? != 1000 {
            bail!("{implementation} wallet baseline is invalid: {baseline}");
        }

        client.call(
            "proofstorm_wallet_round_trip",
            merge(json!({"operation_id": format!("{prefix}-round-trip"), "payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": format!("{prefix}-round-trip")})),
        )?;
        let round_trip = lab::wait_operation(&mut client, &format!("{prefix}-round-trip"), 160)?;
        let round_content = lab::artifact_content(&round_trip)?;
        if expect::boolean(round_content, "/inflation")?
            || expect::integer(round_content, "/minted_sat")? != 1000
        {
            bail!("{implementation} wallet round trip failed: {round_trip}");
        }

        client.call(
            "proofstorm_conservation_oracle",
            merge(json!({
                "operation_id": format!("{prefix}-conservation"),
                "baseline_operation_id": format!("{prefix}-balance-before-round-trip"),
                "treatment_operation_id": format!("{prefix}-round-trip"),
                "idempotency_key": format!("{prefix}-conservation")
            })),
        )?;
        let oracle = lab::wait_operation(&mut client, &format!("{prefix}-conservation"), 160)?;
        if !expect::boolean(lab::artifact_content(&oracle)?, "/conserved")? {
            bail!("{implementation} conservation check failed: {oracle}");
        }
    }

    let cache_size: u64 = redis(context, namespace, "dbsize")?.trim().parse()?;
    if cache_size < 1 {
        bail!("Nutshell wallet workflow did not populate Redis");
    }
    redis(
        context,
        namespace,
        "set proofstorm:restart-canary present >/dev/null",
    )?;

    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if context.kubectl.digest(&cache_secret_args)? != cache_secret_digest {
        bail!("controller restart rotated the Redis credentials");
    }

    context
        .kubectl
        .rollout_restart(namespace, "deployment/cache")?;
    let canary = redis(context, namespace, "exists proofstorm:restart-canary")?;
    if canary.trim() != "0" {
        bail!("ephemeral Redis cache survived restart unexpectedly: {canary}");
    }

    context
        .kubectl
        .rollout_restart(namespace, "deployment/nutshell-mint")?;
    lab::wait_phase(&mut client, INSTANCE, "ready", 80, Duration::from_secs(3))?;

    client.call(
        "proofstorm_lease_release",
        json!({"lease_id": LEASE, "idempotency_key": "release-cross-mint-lease"}),
    )?;
    let closed_experiment = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": EXPERIMENT, "idempotency_key": "close-cross-mint-experiment"}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(&mut client, INSTANCE, "closed", 80, Duration::from_secs(3))?;

    println!(
        "CDK 0.18.0 and Redis-backed Nutshell 0.20.3 passed the same wallet workflow; NUT-20 interoperability, cache use, secret stability, ephemeral restart, recovery, and teardown passed"
    );
    Ok(())
}
