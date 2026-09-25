//! CDK 0.18.1 + Keycloak 25.0.6: NUT-21 and NUT-22 with CDK's upstream default
//! endpoint protection, the auth store on a separate database of the mint's
//! shared PostgreSQL server, spent-token replay persistence, restart recovery,
//! and teardown.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "cdk-oidc-instance";
const EXPERIMENT: &str = "cdk-oidc-experiment";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-oidc-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cdk-oidc"}},
            {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "cell", "config": {"storage_size": "2Gi"}},
            {"id": "identity", "kind": "identity_provider", "implementation": "keycloak", "version": "25.0.6", "config_version": "keycloak/25/v1", "control": "cell", "config": {"access_token_lifespan_seconds": 600}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm Authenticated CDK", "description": "Live NUT-21 and NUT-22 acceptance", "auth_max_blind_tokens": 3}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-lightning", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "mint-database", "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": "primary"}},
            {"id": "mint-auth-database", "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": "authentication"}},
            {"id": "identity-database", "kind": "database_backend", "from": "identity", "to": "database", "binding": {"type": "database", "role": "primary"}},
            {"id": "mint-identity", "kind": "authentication_backend", "from": "mint", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

const CONFIG_FRAGMENTS: &[&str] = &[
    "[auth]\nauth_enabled = true",
    "openid_discovery = \"http://identity:8080/realms/proofstorm/.well-known/openid-configuration\"",
    "openid_client_id = \"cashu-client\"",
    "mint_max_bat = 3",
    "[auth_database.postgres]\nurl = \"env:CDK_MINTD_AUTH_POSTGRES_URL\"",
];

fn finding(client: &mut crate::McpClient, what: &str, result: &Value) -> Result<()> {
    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
    bail!("CDK OIDC {what} reported a conformance finding: {result}");
}

pub fn run(context: &GateContext) -> Result<()> {
    context.qualification_stage("materialize")?;
    let mut client = context.default_session("cdk-oidc-live", "designer")?;
    let kubectl = &context.kubectl;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":context.document(cell_document())?,"request_id":"create-cdk-oidc"}),
    )?;
    let published = cell::review(&mut client, &preview)?;
    for (catalog_id, version, config_version) in [
        ("cdk", "0.18.1", "cdk-mintd/0.18/v1"),
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

    cell::apply(&mut client, &preview)?;
    let status = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&status, "/instance_namespace")?.to_string();

    context.qualification_stage("configuration")?;
    let config = kubectl.exec(
        &namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in CONFIG_FRAGMENTS {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }
    let databases = kubectl.exec(
        &namespace,
        "statefulset/database",
        &[
            "sh",
            "-c",
            "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -At -U proofstorm -d postgres -c \"SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY 1\"",
        ],
    )?;
    for database in ["identity_primary", "mint_authentication", "mint_primary"] {
        if !databases.lines().any(|line| line.trim() == database) {
            bail!("shared PostgreSQL server is missing {database}: {databases}");
        }
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
        "secret/database-credentials",
        "-n",
        &namespace,
        "-o",
        "json",
    ];
    let identity_digest = kubectl.digest(&identity_args)?;
    let database_digest = kubectl.digest(&database_args)?;

    client.call(
        "run_start",
        json!({"request_id":"7036","run_id": EXPERIMENT, "name": INSTANCE}),
    )?;

    context.qualification_stage("conformance")?;
    crate::driver::authentication_conformance(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,
            "request_id": "cdk-oidc-baseline",
            "mint": "mint",
            "identity_provider": "identity"}),
    )?;
    let baseline = cell::wait_operation(&mut client, "cdk-oidc-baseline", 60)?;
    let baseline = cell::artifact_content(&baseline)?;
    expect::equals(
        baseline,
        "/contract",
        &Value::from("proofstorm/authentication-conformance/v1"),
    )?;
    if !expect::boolean(baseline, "/conformant")? {
        return finding(&mut client, "baseline", baseline);
    }

    kubectl.rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if kubectl.digest(&identity_args)? != identity_digest {
        bail!("controller restart rotated the Keycloak Secret");
    }
    if kubectl.digest(&database_args)? != database_digest {
        bail!("controller restart rotated the PostgreSQL Secret");
    }
    for target in [
        "statefulset/database",
        "deployment/identity",
        "deployment/mint",
    ] {
        kubectl.rollout_restart(&namespace, target)?;
    }

    context.qualification_stage("protected-spend")?;
    crate::driver::authentication_protected_spend(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,
            "request_id": "cdk-oidc-protected-spend",
            "mint": "mint",
            "identity_provider": "identity"}),
    )?;
    let protected = cell::wait_operation(&mut client, "cdk-oidc-protected-spend", 60)?;
    let protected = cell::artifact_content(&protected)?;
    if !expect::boolean(protected, "/conformant")?
        || !expect::boolean(protected, "/protected_request")?
    {
        return finding(&mut client, "protected spend", protected);
    }

    // The spent BAT lives in the PostgreSQL auth database; it must stay spent
    // across a mint restart.
    kubectl.rollout_restart(&namespace, "deployment/mint")?;

    context.qualification_stage("replay")?;
    crate::driver::authentication_replay(
        context,
        &mut client,
        json!({
            "name": INSTANCE,
            "run_id": EXPERIMENT,
            "request_id": "cdk-oidc-replay",
            "mint": "mint",
            "identity_provider": "identity",
            "source_operation_id": "cdk-oidc-protected-spend"}),
    )?;
    let replay = cell::wait_operation(&mut client, "cdk-oidc-replay", 60)?;
    let replay = cell::artifact_content(&replay)?;
    if !expect::boolean(replay, "/conformant")? || !expect::boolean(replay, "/protected_request")? {
        return finding(&mut client, "replay", replay);
    }

    cell::wait_phase(&mut client, INSTANCE, "ready", 100, Duration::from_secs(3))?;
    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
    println!(
        "CDK 0.18.1 + Keycloak 25.0.6 passed NUT-21/NUT-22 with upstream endpoint defaults, PostgreSQL auth store, replay persistence, restart recovery, and teardown"
    );
    Ok(())
}
