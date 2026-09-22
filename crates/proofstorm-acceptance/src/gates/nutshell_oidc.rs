//! Nutshell 0.20.3 + Keycloak 25.0.6: NUT-21 and NUT-22 positive and negative
//! limits, spent-token replay persistence, restart recovery, and teardown.
//!
//! Ported from `tests/kubernetes/nutshell_oidc_mcp_client.py`.
//!
//! The 0.21 family does not currently advertise authenticated support: its
//! upstream auth database cannot issue blind-auth proofs. The planner selects
//! only versions that explicitly declare the authenticated integration.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "nutshell-oidc-instance";
const EXPERIMENT: &str = "nutshell-oidc-experiment";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-oidc-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-nutshell-oidc"}},
            {"id": "identity-db", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "cell", "config": {"database_name": "keycloak", "storage_size": "2Gi"}},
            {"id": "identity", "kind": "identity_provider", "implementation": "keycloak", "version": "25.0.6", "config_version": "keycloak/25/v1", "control": "cell", "config": {"access_token_lifespan_seconds": 600}},
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
    context.qualification_stage("materialize")?;
    let mut client = context.default_session("nutshell-oidc-live", "designer")?;
    let kubectl = &context.kubectl;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":context.document(cell_document())?,"request_id":"create-nutshell-oidc"}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;
    for (catalog_id, version, config_version) in [
        ("nutshell", "0.20.3", "nutshell-mint/0.20/v1"),
        ("keycloak", "25.0.6", "keycloak/25/v1"),
        ("postgresql", "17.11", "postgresql/17/v1"),
    ] {
        let entry = cell::lock_entry(&published, catalog_id)?;
        if expect::string(entry, "/version")? != context.selected_version(catalog_id, version)
            || expect::string(entry, "/config_version")? != config_version
        {
            bail!("unexpected {catalog_id} lock: {entry}");
        }
    }

    crate::cell::apply(&mut client, &preview)?;
    let status = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();

    context.qualification_stage("funding")?;
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

    context.qualification_stage("configuration")?;
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
        "run_start",
        json!({"request_id":"7035","run_id": EXPERIMENT, "name": INSTANCE}),
    )?;

    crate::driver::authentication_conformance(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,

            "request_id": "nutshell-oidc-baseline",
            "mint": "mint",
            "identity_provider": "identity"}),
    )?;
    let baseline = cell::wait_operation(&mut client, "nutshell-oidc-baseline", 60)?;
    let baseline = cell::artifact_content(&baseline)?;
    expect::equals(
        baseline,
        "/contract",
        &Value::from("proofstorm/authentication-conformance/v1"),
    )?;
    expect::equals(baseline, "/mint", &Value::from("mint"))?;
    expect::equals(baseline, "/identity_provider", &Value::from("identity"))?;
    if !expect::boolean(baseline, "/conformant")? {
        client.call("cell_remove", json!({"name": INSTANCE}))?;
        cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
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

    crate::driver::authentication_protected_spend(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,

            "request_id": "nutshell-oidc-protected-spend",
            "mint": "mint",
            "identity_provider": "identity"}),
    )?;
    let protected = cell::wait_operation(&mut client, "nutshell-oidc-protected-spend", 60)?;
    let protected = cell::artifact_content(&protected)?;
    expect::equals(
        protected,
        "/contract",
        &Value::from("proofstorm/authentication-protected-spend/v1"),
    )?;
    if !expect::boolean(protected, "/conformant")?
        || !expect::boolean(protected, "/protected_request")?
    {
        client.call("cell_remove", json!({"name": INSTANCE}))?;
        cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
        bail!("Nutshell OIDC protected spend reported a conformance finding: {protected}");
    }

    kubectl.rollout_restart(&namespace, "deployment/mint")?;

    crate::driver::authentication_replay(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,

            "request_id": "nutshell-oidc-replay",
            "mint": "mint",
            "identity_provider": "identity",
            "source_operation_id": "nutshell-oidc-protected-spend"}),
    )?;
    let replay = cell::wait_operation(&mut client, "nutshell-oidc-replay", 60)?;
    let replay = cell::artifact_content(&replay)?;
    expect::equals(
        replay,
        "/contract",
        &Value::from("proofstorm/authentication-replay/v1"),
    )?;
    if !expect::boolean(replay, "/conformant")? || !expect::boolean(replay, "/protected_request")? {
        client.call("cell_remove", json!({"name": INSTANCE}))?;
        cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
        bail!("Nutshell OIDC replay reported a conformance finding: {replay}");
    }

    cell::wait_phase(&mut client, INSTANCE, "ready", 100, Duration::from_secs(3))?;

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;

    println!(
        "Nutshell 0.20.3 + Keycloak 25.0.6 passed NUT-21/NUT-22 positive and negative limits, replay persistence, restart recovery, and teardown"
    );
    Ok(())
}
