//! Nutshell 0.20.3 + Keycloak 25.0.6: NUT-21 and NUT-22 positive and negative
//! limits, spent-token replay persistence, restart recovery, and teardown.
//!
//! Ported from `tests/kubernetes/nutshell_oidc_mcp_client.py`.
//!
//! **This gate is expected to fail against Nutshell 0.20.3.** The blind auth
//! token mint returns 400 because the mint's auth database is missing the
//! `mint_quote` column on its `promises` table. That is an upstream defect,
//! already filed, and must not be worked around here. The port is therefore
//! faithful but unverified; its first green run will be the one that follows
//! the upstream fix.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, gate::CONTROL_NAMESPACE, json as expect, lab};

const INSTANCE: &str = "nutshell-oidc-instance";
const DRAFT: &str = "nutshell-oidc";
const EXPERIMENT: &str = "nutshell-oidc-experiment";
const LEASE: &str = "nutshell-oidc-lease";
const CAPABILITIES: &[&str] = &[
    "catalog.read",
    "lab.read",
    "lab.create",
    "lab.validate",
    "lab.publish",
    "lab.materialize",
    "lab.status",
    "lab.close",
    "experiment.create",
    "experiment.read",
    "experiment.close",
    "lease.acquire",
    "lease.release",
    "authentication.test",
    "artifact.read",
];

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-oidc-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-nutshell-oidc"}},
            {"id": "identity-db", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "laboratory", "config": {"database_name": "keycloak", "storage_size": "2Gi"}},
            {"id": "identity", "kind": "identity_provider", "implementation": "keycloak", "version": "25.0.6", "config_version": "keycloak/25/v1", "control": "laboratory", "config": {"access_token_lifespan_seconds": 600}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Authenticated Nutshell", "description": "Live NUT-21 and NUT-22 acceptance", "auth_rate_limit_per_minute": 2, "auth_max_blind_tokens": 3}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-lightning", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "identity-database", "kind": "database_backend", "from": "identity", "to": "identity-db", "binding": {"type": "database", "role": "primary"}},
            {"id": "mint-identity", "kind": "authentication_backend", "from": "mint", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

fn bitcoin(context: &GateContext, namespace: &str, arguments: &[&str]) -> Result<String> {
    let mut argv = vec![
        "bitcoin-cli",
        "-regtest",
        "-rpcuser=proofstorm",
        "-rpcpassword=proofstorm-regtest-only",
    ];
    argv.extend_from_slice(arguments);
    context.kubectl.exec(namespace, "statefulset/chain", &argv)
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session("nutshell-oidc-live", "designer", CAPABILITIES)?;
    let kubectl = &context.kubectl;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-nutshell-oidc"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-nutshell-oidc", "include_revision": true}),
    )?;
    for (catalog_id, version, config_version) in [
        ("nutshell", "0.20.3", "nutshell-mint/0.20/v1"),
        ("keycloak", "25.0.6", "keycloak/25/v1"),
        ("postgresql", "17.11", "postgresql/17/v1"),
    ] {
        let entry = lab::lock_entry(&published, catalog_id)?;
        if expect::string(entry, "/version")? != version
            || expect::string(entry, "/config_version")? != config_version
        {
            bail!("unexpected {catalog_id} lock: {entry}");
        }
    }

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-nutshell-oidc"}),
    )?;
    let status = lab::wait_phase(&mut client, INSTANCE, "ready", 240, Duration::from_secs(3))?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();

    bitcoin(context, &namespace, &["createwallet", "default"])?;
    let miner = bitcoin(
        context,
        &namespace,
        &["-rpcwallet=default", "getnewaddress"],
    )?;
    bitcoin(
        context,
        &namespace,
        &["-rpcwallet=default", "generatetoaddress", "101", &miner],
    )?;

    let mut synced = false;
    for _ in 0..60 {
        let raw = kubectl.exec(
            &namespace,
            "statefulset/lightning",
            &[
                "lncli",
                "--lnddir=/home/lnd/.lnd",
                "--network=regtest",
                "getinfo",
            ],
        )?;
        let info: Value = serde_json::from_str(&raw)?;
        if info.get("synced_to_chain").and_then(Value::as_bool) == Some(true)
            && info
                .get("block_height")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                >= 101
        {
            synced = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !synced {
        bail!("LND did not synchronize to the acceptance chain");
    }

    let mint_config = kubectl.get_json(&["get", "configmap/mint-config", "-n", &namespace])?;
    for (key, wanted) in [
        ("MINT_REQUIRE_AUTH", "TRUE"),
        ("MINT_AUTH_OICD_CLIENT_ID", "cashu-client"),
        (
            "MINT_AUTH_OICD_DISCOVERY_URL",
            "http://identity:8080/realms/proofstorm/.well-known/openid-configuration",
        ),
        ("MINT_AUTH_RATE_LIMIT_PER_MINUTE", "2"),
        ("MINT_AUTH_MAX_BLIND_TOKENS", "3"),
        ("MINT_AUTH_DATABASE", "/app/data"),
    ] {
        expect::equals(&mint_config, &format!("/data/{key}"), &Value::from(wanted))?;
    }

    let identity_args = [
        "get",
        "secret/identity-credentials",
        "-n",
        &namespace,
        "-o",
        "json",
    ];
    let database_args = [
        "get",
        "secret/identity-db-credentials",
        "-n",
        &namespace,
        "-o",
        "json",
    ];
    let identity_digest = kubectl.digest(&identity_args)?;
    let database_digest = kubectl.digest(&database_args)?;

    client.call(
        "proofstorm_experiment_create",
        json!({"experiment_id": EXPERIMENT, "instance_id": INSTANCE, "idempotency_key": "create-nutshell-oidc-experiment"}),
    )?;
    client.call(
        "proofstorm_lease_acquire",
        json!({"experiment_id": EXPERIMENT, "lease_id": LEASE, "duration_seconds": 1200, "max_actions": 3, "idempotency_key": "acquire-nutshell-oidc-lease"}),
    )?;
    client.call(
        "proofstorm_authentication_conformance",
        json!({
            "instance_id": INSTANCE,
            "experiment_id": EXPERIMENT,
            "lease_id": LEASE,
            "operation_id": "nutshell-oidc-baseline",
            "mint": "mint",
            "identity_provider": "identity",
            "idempotency_key": "nutshell-oidc-baseline"
        }),
    )?;
    let baseline = lab::wait_operation(&mut client, "nutshell-oidc-baseline", 60)?;
    let baseline = lab::artifact_content(&baseline)?;
    expect::equals(
        baseline,
        "/contract",
        &Value::from("proofstorm/authentication-conformance/v1"),
    )?;
    expect::equals(baseline, "/mint", &Value::from("mint"))?;
    expect::equals(baseline, "/identity_provider", &Value::from("identity"))?;
    if !expect::boolean(baseline, "/conformant")? {
        client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
        lab::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
        bail!("Nutshell OIDC baseline reported a conformance finding: {baseline}");
    }

    kubectl.rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if kubectl.digest(&identity_args)? != identity_digest {
        bail!("controller restart rotated the Keycloak Secret");
    }
    if kubectl.digest(&database_args)? != database_digest {
        bail!("controller restart rotated the Keycloak PostgreSQL Secret");
    }

    for target in [
        "statefulset/identity-db",
        "deployment/identity",
        "deployment/mint",
    ] {
        kubectl.rollout_restart(&namespace, target)?;
    }

    client.call(
        "proofstorm_authentication_protected_spend",
        json!({
            "instance_id": INSTANCE,
            "experiment_id": EXPERIMENT,
            "lease_id": LEASE,
            "operation_id": "nutshell-oidc-protected-spend",
            "mint": "mint",
            "identity_provider": "identity",
            "idempotency_key": "nutshell-oidc-protected-spend"
        }),
    )?;
    let protected = lab::wait_operation(&mut client, "nutshell-oidc-protected-spend", 60)?;
    let protected = lab::artifact_content(&protected)?;
    expect::equals(
        protected,
        "/contract",
        &Value::from("proofstorm/authentication-protected-spend/v1"),
    )?;
    if !expect::boolean(protected, "/conformant")?
        || !expect::boolean(protected, "/protected_request")?
    {
        client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
        lab::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
        bail!("Nutshell OIDC protected spend reported a conformance finding: {protected}");
    }

    kubectl.rollout_restart(&namespace, "deployment/mint")?;

    client.call(
        "proofstorm_authentication_replay",
        json!({
            "instance_id": INSTANCE,
            "experiment_id": EXPERIMENT,
            "lease_id": LEASE,
            "operation_id": "nutshell-oidc-replay",
            "mint": "mint",
            "identity_provider": "identity",
            "source_operation_id": "nutshell-oidc-protected-spend",
            "idempotency_key": "nutshell-oidc-replay"
        }),
    )?;
    let replay = lab::wait_operation(&mut client, "nutshell-oidc-replay", 60)?;
    let replay = lab::artifact_content(&replay)?;
    expect::equals(
        replay,
        "/contract",
        &Value::from("proofstorm/authentication-replay/v1"),
    )?;
    if !expect::boolean(replay, "/conformant")? || !expect::boolean(replay, "/protected_request")? {
        client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
        lab::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
        bail!("Nutshell OIDC replay reported a conformance finding: {replay}");
    }

    lab::wait_phase(&mut client, INSTANCE, "ready", 100, Duration::from_secs(3))?;

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;

    println!(
        "Nutshell 0.20.3 + Keycloak 25.0.6 passed NUT-21/NUT-22 positive and negative limits, replay persistence, restart recovery, and teardown"
    );
    Ok(())
}
