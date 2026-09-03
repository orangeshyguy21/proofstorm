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

use crate::{GateContext, LIFECYCLE_CAPABILITIES, gate::CONTROL_NAMESPACE, json as expect, lab};

/// All three drivers run inside the mint image, which ships `httpx` and the
/// `cashu` wallet library.
const PRE_RESTART_DRIVER: &str = include_str!("../../drivers/oidc_pre_restart.py");
const POST_RESTART_DRIVER: &str = include_str!("../../drivers/oidc_post_restart.py");
const REPLAY_DRIVER: &str = include_str!("../../drivers/oidc_replay.py");

const INSTANCE: &str = "nutshell-oidc-instance";
const DRAFT: &str = "nutshell-oidc";

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

/// Run one in-container driver, feeding it the login payload on stdin.
fn exec_driver(
    context: &GateContext,
    namespace: &str,
    driver: &str,
    payload: &Value,
) -> Result<Value> {
    let output = context.kubectl.exec_stdin(
        namespace,
        "deployment/mint",
        &["python3", "-c", driver],
        &serde_json::to_string(payload)?,
    )?;
    let last = output
        .lines()
        .last()
        .ok_or_else(|| anyhow::anyhow!("driver produced no output"))?;
    serde_json::from_str(last.trim())
        .map_err(|error| anyhow::anyhow!("parse driver output {last:?}: {error}"))
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session("nutshell-oidc-live", "designer", LIFECYCLE_CAPABILITIES)?;
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

    let credentials = kubectl.secret_data(&namespace, "identity-credentials")?;
    let mut keys: Vec<&str> = credentials.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys
        != [
            "KEYCLOAK_ADMIN_PASSWORD",
            "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS",
            "OIDC_TEST_PASSWORD",
            "OIDC_TEST_USERNAME",
            "PROOFSTORM_SECRET_KIND",
            "realm.json",
        ]
    {
        bail!("generated Keycloak Secret has unexpected keys: {keys:?}");
    }

    let realm: Value = serde_json::from_str(
        credentials
            .get("realm.json")
            .ok_or_else(|| anyhow::anyhow!("no realm.json in the generated Secret"))?,
    )?;
    if expect::string(&realm, "/realm")? != "proofstorm"
        || expect::integer(&realm, "/accessTokenLifespan")? != 600
    {
        bail!("generated Keycloak realm does not preserve authored policy");
    }
    if expect::string(&realm, "/clients/0/clientId")? != "cashu-client"
        || !expect::boolean(&realm, "/clients/0/directAccessGrantsEnabled")?
    {
        bail!("generated Keycloak client does not support the acceptance login flow");
    }
    let scopes = expect::array(&realm, "/clients/0/defaultClientScopes")?;
    if !scopes.iter().any(|scope| scope.as_str() == Some("basic")) {
        bail!("generated Keycloak client does not request the standard subject claim");
    }

    let public_config = serde_json::to_string(&mint_config)?;
    for key in ["OIDC_TEST_PASSWORD", "KEYCLOAK_ADMIN_PASSWORD"] {
        let secret = credentials
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("no {key} in the generated Secret"))?;
        if public_config.contains(secret) {
            bail!("public mint configuration contains generated Keycloak credentials");
        }
    }

    let login = json!({
        "username": credentials.get("OIDC_TEST_USERNAME"),
        "password": credentials.get("OIDC_TEST_PASSWORD")
    });

    exec_driver(context, &namespace, PRE_RESTART_DRIVER, &login)?;

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

    exec_driver(context, &namespace, POST_RESTART_DRIVER, &login)?;

    kubectl.rollout_restart(&namespace, "deployment/mint")?;

    exec_driver(context, &namespace, REPLAY_DRIVER, &login)?;

    lab::wait_phase(&mut client, INSTANCE, "ready", 100, Duration::from_secs(3))?;

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;

    println!(
        "Nutshell 0.20.3 + Keycloak 25.0.6 passed NUT-21/NUT-22 positive and negative limits, replay persistence, restart recovery, and teardown"
    );
    Ok(())
}
