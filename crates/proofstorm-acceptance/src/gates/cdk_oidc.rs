//! CDK 0.18.1 + Keycloak 25.0.6: NUT-21 and NUT-22 with CDK's upstream default
//! endpoint protection, SQLite and a separate auth database on the mint's
//! shared PostgreSQL server, spent-token replay persistence, restart recovery,
//! and teardown.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "cdk-oidc-instance";
const EXPERIMENT: &str = "cdk-oidc-experiment";
const MINTS: &[&str] = &["mint", "mint-sqlite"];

fn cell_document() -> Value {
    let mut document = json!({
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
    });
    let mut sqlite = document["components"][4].clone();
    sqlite["id"] = json!("mint-sqlite");
    document["components"]
        .as_array_mut()
        .expect("fixture array")
        .push(sqlite);
    document["links"].as_array_mut().expect("fixture array").extend([
        json!({"id": "sqlite-lightning", "kind": "payment_backend", "from": "mint-sqlite", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}),
        json!({"id": "sqlite-identity", "kind": "authentication_backend", "from": "mint-sqlite", "to": "identity", "binding": {"type": "authentication", "protocol": "oidc"}}),
    ]);
    document
}

const CONFIG_FRAGMENTS: &[&str] = &[
    "[auth]\nauth_enabled = true",
    "openid_discovery = \"http://identity:8080/realms/proofstorm/.well-known/openid-configuration\"",
    "openid_client_id = \"cashu-client\"",
    "mint_max_bat = 3",
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
    for mint in MINTS {
        let config = kubectl.exec(
            &namespace,
            &format!("deployment/{mint}"),
            &["cat", "/config/config.toml"],
        )?;
        for fragment in CONFIG_FRAGMENTS {
            if !config.contains(fragment) {
                bail!("{mint} configuration is missing {fragment:?}");
            }
        }
        let postgres_auth =
            config.contains("[auth_database.postgres]\nurl = \"env:CDK_MINTD_AUTH_POSTGRES_URL\"");
        if postgres_auth != (*mint == "mint") {
            bail!("{mint} has the wrong authentication database configuration");
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

    for mint in MINTS {
        context.qualification_stage(if mint == &"mint" {
            "conformance-postgres"
        } else {
            "conformance-sqlite"
        })?;
        let request_id = format!("cdk-oidc-{mint}-baseline");
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
        "deployment/mint-sqlite",
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
        "CDK 0.18.1 + Keycloak 25.0.6 passed NUT-21/NUT-22 with upstream endpoint defaults, SQLite and PostgreSQL auth stores, replay persistence, restart recovery, and teardown"
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
    context.qualification_stage(if mint == "mint" {
        "protected-spend-postgres"
    } else {
        "protected-spend-sqlite"
    })?;
    let spend_id = format!("cdk-oidc-{mint}-protected-spend");
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
    context.qualification_stage(if mint == "mint" {
        "replay-postgres"
    } else {
        "replay-sqlite"
    })?;
    let replay_id = format!("cdk-oidc-{mint}-replay");
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
            if name != "cdk-oidc" {
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
                    link.from == "mint" && link.kind == proofstorm_core::LinkKind::DatabaseBackend
                })
                .collect();
            assert_eq!(databases.len(), 2);
            for role in [DatabaseRole::Primary, DatabaseRole::Authentication] {
                assert!(databases.iter().any(|link| link.to == "database" && matches!(link.binding, Some(proofstorm_core::DependencyBinding::Database { role: actual, .. }) if actual == role)));
            }
            assert!(!cell.links.iter().any(|link| link.from == "mint-sqlite"
                && link.kind == proofstorm_core::LinkKind::DatabaseBackend));
            assert_eq!(
                cell.components
                    .iter()
                    .filter(|component| component.implementation == "cdk")
                    .count(),
                2
            );
        }
        assert_eq!(covered, ["linux/amd64", "linux/arm64"].into());
    }
}
