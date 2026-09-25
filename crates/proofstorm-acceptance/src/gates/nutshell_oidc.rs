//! Nutshell 0.21.0 + Keycloak 25.0.6: NUT-21 and NUT-22 with Nutshell's upstream default
//! endpoint protection and SQLite primary storage. Auth stores use SQLite and
//! PostgreSQL (shared with Keycloak), spent-token replay persistence, restart recovery,
//! and teardown.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "nutshell-oidc-instance";
const EXPERIMENT: &str = "nutshell-oidc-experiment";
const MINTS: &[&str] = &["mint", "mint-pg"];

fn cell_document() -> Value {
    let mut document = json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-oidc-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-nutshell-oidc"}},
            {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "cell", "config": {"storage_size": "2Gi"}},
            {"id": "identity", "kind": "identity_provider", "implementation": "keycloak", "version": "25.0.6", "config_version": "keycloak/25/v1", "control": "cell", "config": {"access_token_lifespan_seconds": 600}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Authenticated Nutshell", "description": "Live NUT-21 and NUT-22 acceptance", "auth_max_blind_tokens": 3, "auth_rate_limit_per_minute": 2}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-lightning", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "identity-database", "kind": "database_backend", "from": "identity", "to": "database", "binding": {"type": "database", "role": "primary"}},
            {"id": "mint-identity", "kind": "authentication_backend", "from": "mint", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    });
    let mut postgres = document["components"][4].clone();
    postgres["id"] = json!("mint-pg");
    document["components"]
        .as_array_mut()
        .expect("fixture array")
        .push(postgres);
    document["links"].as_array_mut().expect("fixture array").extend([
        json!({"id": "postgres-lightning", "kind": "payment_backend", "from": "mint-pg", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}),
        json!({"id": "postgres-identity", "kind": "authentication_backend", "from": "mint-pg", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}}),
    ]);
    document["links"].as_array_mut().expect("fixture links").push(json!({"id":"mint-auth-database","kind":"database_backend","from":"mint-pg","to":"database","binding":{"type":"database","role":"authentication"}}));
    document
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

fn finding(client: &mut crate::McpClient, what: &str, result: &Value) -> Result<()> {
    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
    bail!("Nutshell OIDC {what} reported a conformance finding: {result}");
}

pub fn run(context: &GateContext) -> Result<()> {
    context.qualification_stage("materialize")?;
    let mut client = context.default_session("nutshell-oidc-live", "designer")?;
    let kubectl = &context.kubectl;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":context.document(cell_document())?,"request_id":"create-nutshell-oidc"}),
    )?;
    let published = cell::review(&mut client, &preview)?;
    for (catalog_id, version, config_version) in [
        ("nutshell", "0.21.0", "nutshell-mint/0.20/v1"),
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
    for mint in MINTS {
        let config =
            kubectl.get_json(&["get", &format!("configmap/{mint}-config"), "-n", &namespace])?;
        for (key, wanted) in [
            ("MINT_REQUIRE_AUTH", "TRUE"),
            ("MINT_AUTH_OICD_CLIENT_ID", "cashu-client"),
            (
                "MINT_AUTH_OICD_DISCOVERY_URL",
                "http://identity:8080/realms/proofstorm/.well-known/openid-configuration",
            ),
            ("MINT_AUTH_RATE_LIMIT_PER_MINUTE", "2"),
            ("MINT_AUTH_MAX_BLIND_TOKENS", "3"),
        ] {
            expect::equals(&config, &format!("/data/{key}"), &json!(wanted))?;
        }
        if *mint == "mint" {
            expect::equals(&config, "/data/MINT_AUTH_DATABASE", &json!("/app/data"))?;
        } else if !config["data"]["MINT_AUTH_DATABASE"].is_null() {
            bail!("PostgreSQL auth URL must come from the credential-backed environment");
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
    for database in ["identity_primary", "mint_pg_authentication"] {
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

    for mint in MINTS {
        context.qualification_stage(if mint == &"mint-pg" {
            "conformance-postgres"
        } else {
            "conformance-sqlite"
        })?;
        let request_id = format!("nutshell-oidc-{mint}-baseline");
        crate::driver::authentication_conformance(
            context,
            &mut client,
            json!({"name": INSTANCE, "run_id": EXPERIMENT,
                "request_id": request_id, "mint": mint, "identity_provider": "identity"}),
        )?;
        let operation = wait_auth_operation(context, &mut client, &namespace, &request_id)?;
        let baseline = cell::artifact_content(&operation)?;
        expect::equals(
            baseline,
            "/contract",
            &json!("proofstorm/authentication-conformance/v1"),
        )?;
        expect::equals(baseline, "/mint", &json!(mint))?;
        if !expect::boolean(baseline, "/conformant")? {
            return finding(&mut client, &format!("{mint} baseline"), baseline);
        }
    }

    context.qualification_stage("restart")?;
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
        "deployment/mint-pg",
    ] {
        kubectl.rollout_restart(&namespace, target)?;
    }

    let restarted_at = super::authentication::now()?;
    for mint in MINTS {
        super::authentication::wait_protocol_ready(&mut client, INSTANCE, mint, restarted_at)?;
        verify_spend_and_replay(context, &mut client, &namespace, mint)?;
    }

    context.qualification_stage("teardown")?;
    cell::wait_phase(&mut client, INSTANCE, "ready", 100, Duration::from_secs(3))?;
    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(&mut client, INSTANCE, "closed", 100, Duration::from_secs(3))?;
    println!(
        "Nutshell 0.21.0 + Keycloak 25.0.6 passed NUT-21/NUT-22 with upstream endpoint defaults, SQLite and PostgreSQL auth stores, replay persistence, restart recovery, and teardown"
    );
    Ok(())
}

fn wait_auth_operation(
    context: &GateContext,
    client: &mut crate::McpClient,
    namespace: &str,
    operation: &str,
) -> Result<Value> {
    let result = cell::wait_operation(client, operation, 60);
    if result.is_err() {
        // Keep termination diagnostics in the run's private directory before
        // the owned runner tears down the failed cell. Never print pod contents.
        if let Ok(pods) = context.kubectl.get_json(&["get", "pods", "-n", namespace]) {
            context.record(&format!("{operation}-failed-pods.json"), &pods)?;
        }
    }
    result
}

fn verify_spend_and_replay(
    context: &GateContext,
    client: &mut crate::McpClient,
    namespace: &str,
    mint: &str,
) -> Result<()> {
    context.qualification_stage(if mint == "mint-pg" {
        "protected-spend-postgres"
    } else {
        "protected-spend-sqlite"
    })?;
    let spend_id = format!("nutshell-oidc-{mint}-protected-spend");
    crate::driver::authentication_protected_spend(
        context,
        client,
        json!({"name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": spend_id, "mint": mint, "identity_provider": "identity"}),
    )?;
    let operation = wait_auth_operation(context, client, namespace, &spend_id)?;
    let protected = cell::artifact_content(&operation)?;
    if !expect::boolean(protected, "/conformant")?
        || !expect::boolean(protected, "/protected_request")?
    {
        return finding(client, &format!("{mint} protected spend"), protected);
    }

    // Both auth stores must keep the BAT spent after replacing the mint process.
    context
        .kubectl
        .rollout_restart(namespace, &format!("deployment/{mint}"))?;
    super::authentication::wait_protocol_ready(
        client,
        INSTANCE,
        mint,
        super::authentication::now()?,
    )?;
    context.qualification_stage(if mint == "mint-pg" {
        "replay-postgres"
    } else {
        "replay-sqlite"
    })?;
    let replay_id = format!("nutshell-oidc-{mint}-replay");
    crate::driver::authentication_replay(
        context,
        client,
        json!({"name": INSTANCE, "run_id": EXPERIMENT,
            "request_id": replay_id, "mint": mint, "identity_provider": "identity",
            "source_operation_id": spend_id}),
    )?;
    let operation = wait_auth_operation(context, client, namespace, &replay_id)?;
    let replay = cell::artifact_content(&operation)?;
    if !expect::boolean(replay, "/conformant")? || !expect::boolean(replay, "/protected_request")? {
        return finding(client, &format!("{mint} replay"), replay);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{CellSpec, DatabaseRole, resolve_lock, validate_cell};
    use proofstorm_qualification::{Identity, Mode, Scenario};

    #[test]
    fn auth_fixture_qualifies_both_stores_on_every_planned_platform() {
        let plan = proofstorm_qualification::plan(
            Identity {
                revision: "a".repeat(40),
                run_id: "0".into(),
                attempt: 1,
            },
            Mode::Compatibility,
        )
        .unwrap();
        let mut covered = std::collections::BTreeSet::new();
        for case in &plan.cases {
            let Scenario::Gate { name, versions } = &case.scenario else {
                continue;
            };
            if name != "nutshell-oidc" {
                continue;
            }
            covered.insert(case.platform.as_str());
            let mut fixture = cell_document();
            let observer = crate::qualification::Observer::new(case.clone());
            observer.document(&mut fixture).unwrap();
            observer.finish().unwrap();
            let cell: CellSpec = serde_json::from_value(fixture).unwrap();
            let validation = validate_cell(&cell);
            assert!(validation.valid, "{:?}", validation.issues);
            let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
            for entry in resolve_lock(&cell, &catalog).unwrap().entries {
                assert_eq!(entry.version, versions[&entry.catalog_id]);
            }
            let databases: Vec<_> = cell
                .links
                .iter()
                .filter(|link| {
                    link.from == "mint-pg"
                        && link.kind == proofstorm_core::LinkKind::DatabaseBackend
                })
                .collect();
            assert_eq!(databases.len(), 1);
            assert!(matches!(
                databases[0].binding,
                Some(proofstorm_core::DependencyBinding::Database {
                    role: DatabaseRole::Authentication,
                    ..
                })
            ));
            assert_eq!(databases[0].to, "database");
            assert!(!cell.links.iter().any(|link| link.from == "mint"
                && link.kind == proofstorm_core::LinkKind::DatabaseBackend));
            assert_eq!(
                cell.components
                    .iter()
                    .filter(|component| component.implementation == "nutshell")
                    .count(),
                2
            );
        }
        assert_eq!(covered, ["linux/amd64", "linux/arm64"].into());
    }
}
