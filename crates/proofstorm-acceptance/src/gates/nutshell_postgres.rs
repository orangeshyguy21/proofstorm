//! Nutshell 0.21.0 + PostgreSQL: secret stability across a controller restart,
//! database persistence across a workload restart, and verified teardown.
//!
//! Ported from `tests/kubernetes/nutshell_postgres_mcp_client.py`.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect, postgres};

const INSTANCE: &str = "nutshell-postgres-instance";
const MARKER: &str = "nutshell-persistent";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-postgres-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-nutshell-postgres"}},
            {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "cell", "config": {"storage_size": "2Gi"}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell PostgreSQL", "description": "Secret-backed persistence acceptance", "mint_quote_ttl_seconds": 701, "melt_quote_ttl_seconds": 131}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "mint-database", "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": "primary", "database": "nutshell_mint"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("nutshell-postgres-live", "designer")?;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":cell_document(),"request_id":"create-nutshell-postgres"}),
    )?;
    crate::cell::review(&mut client, &preview)?;
    crate::cell::apply(&mut client, &preview)?;

    let ready = cell::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let public_config =
        context
            .kubectl
            .get_json(&["get", "configmap/mint-config", "-n", namespace])?;
    let data = expect::object(&public_config, "/data")?;
    if data.contains_key("MINT_DATABASE") || data.contains_key("MINT_PRIVATE_KEY") {
        bail!("public Nutshell configuration contains private database or mint credentials");
    }
    if data.values().any(|value| {
        value
            .as_str()
            .is_some_and(|text| text.contains("postgresql://"))
    }) {
        bail!("public Nutshell configuration contains a PostgreSQL URL");
    }

    let database_secret_args = [
        "get",
        "secret/database-credentials",
        "-n",
        namespace,
        "-o",
        "json",
    ];
    let mint_secret_args = [
        "get",
        "secret/mint-credentials",
        "-n",
        namespace,
        "-o",
        "json",
    ];
    let database_digest = context.kubectl.digest(&database_secret_args)?;
    let mint_digest = context.kubectl.digest(&mint_secret_args)?;

    let database_secret =
        context
            .kubectl
            .get_json(&["get", "secret/database-credentials", "-n", namespace])?;
    let mut database_keys: Vec<&str> = expect::object(&database_secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    database_keys.sort_unstable();
    if database_keys != ["POSTGRES_DB", "POSTGRES_PASSWORD", "POSTGRES_USER"] {
        bail!("generated PostgreSQL Secret has an unexpected key contract: {database_keys:?}");
    }

    let mint_secret =
        context
            .kubectl
            .get_json(&["get", "secret/mint-credentials", "-n", namespace])?;
    let mut mint_keys: Vec<&str> = expect::object(&mint_secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    mint_keys.sort_unstable();
    if mint_keys != ["MINT_PRIVATE_KEY", "PROOFSTORM_SECRET_KIND"] {
        bail!("generated Nutshell Secret has an unexpected key contract: {mint_keys:?}");
    }

    let rendered = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["/opt/proofstorm/driver", "nutshell", "postgres-settings"],
    )?;
    let settings: Value = serde_json::from_str(rendered.trim())?;
    let expected = json!({
        "version": "0.21.0",
        "name": "Proofstorm Nutshell PostgreSQL",
        "database_host": "database",
        "database_name": "nutshell_mint",
        "private_key_length": 64
    });
    if settings != expected {
        bail!("live Nutshell PostgreSQL settings differ: {settings}");
    }

    postgres::seed_sentinel(true, &context.kubectl, namespace, MARKER)?;
    let tables = postgres::schema_table_count(&context.kubectl, namespace, "nutshell_mint")?;
    if tables < 2 {
        bail!("Nutshell did not initialize its PostgreSQL schema: {tables} tables");
    }

    let management_args = [
        "get",
        "secret/mint-management-tls",
        "-n",
        namespace,
        "-o",
        "jsonpath={.data}",
    ];
    let management_digest = context.kubectl.digest(&management_args)?;
    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if context.kubectl.digest(&management_args)? != management_digest {
        bail!("controller restart rotated management TLS credentials");
    }
    if context.kubectl.digest(&database_secret_args)? != database_digest {
        bail!("controller restart rotated the PostgreSQL Secret");
    }
    if context.kubectl.digest(&mint_secret_args)? != mint_digest {
        bail!("controller restart rotated the Nutshell private key");
    }

    postgres::restart_database(true, &context.kubectl, namespace)?;
    context
        .kubectl
        .rollout_restart(namespace, "deployment/mint")?;
    postgres::verify_sentinel(true, &context.kubectl, namespace, MARKER)?;

    cell::wait_phase(&mut client, INSTANCE, "ready", 80, Duration::from_secs(3))?;

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(&mut client, INSTANCE, "closed", 80, Duration::from_secs(3))?;

    println!(
        "Nutshell 0.21.0 + PostgreSQL secret stability, database persistence, mint restart, readiness, and teardown passed"
    );
    Ok(())
}
