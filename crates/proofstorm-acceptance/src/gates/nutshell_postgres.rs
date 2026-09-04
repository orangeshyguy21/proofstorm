//! Nutshell 0.20.3 + PostgreSQL: secret stability across a controller restart,
//! database persistence across a workload restart, and verified teardown.
//!
//! Ported from `tests/kubernetes/nutshell_postgres_mcp_client.py`.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, gate::CONTROL_NAMESPACE, json as expect, lab};

const SETTINGS_DRIVER: &str = include_str!("../../drivers/nutshell_postgres_settings.py");

const INSTANCE: &str = "nutshell-postgres-instance";
const DRAFT: &str = "nutshell-postgres";
const MARKER: &str = "nutshell-persistent";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-postgres-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-nutshell-postgres"}},
            {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "laboratory", "config": {"database_name": "nutshell_mint", "storage_size": "2Gi"}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell PostgreSQL", "description": "Secret-backed persistence acceptance", "mint_quote_ttl_seconds": 701, "melt_quote_ttl_seconds": 131}}
        ],
        "links": [
            {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "mint-database", "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": "primary"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

fn psql(context: &GateContext, namespace: &str, statement: &str) -> Result<String> {
    let script = format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -At -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" -c \"{statement}\""
    );
    context
        .kubectl
        .exec(namespace, "statefulset/database", &["sh", "-c", &script])
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client =
        context.session("nutshell-postgres-live", "designer", LIFECYCLE_CAPABILITIES)?;

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-nutshell-postgres"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-nutshell-postgres"}),
    )?;
    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-nutshell-postgres"}),
    )?;

    let ready = lab::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
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
    if database_keys
        != [
            "DATABASE_URL",
            "POSTGRES_DB",
            "POSTGRES_PASSWORD",
            "POSTGRES_USER",
            "database.toml",
        ]
    {
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
        &["python3", "-c", SETTINGS_DRIVER],
    )?;
    let settings: Value = serde_json::from_str(rendered.trim())?;
    let expected = json!({
        "version": "0.20.3",
        "name": "Proofstorm Nutshell PostgreSQL",
        "database_host": "database",
        "database_name": "nutshell_mint",
        "private_key_length": 64
    });
    if settings != expected {
        bail!("live Nutshell PostgreSQL settings differ: {settings}");
    }

    let seed = format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -v ON_ERROR_STOP=1 -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" \
         -c \"CREATE TABLE IF NOT EXISTS proofstorm_acceptance (id integer primary key, marker text not null);\" \
         -c \"INSERT INTO proofstorm_acceptance VALUES (1, '{MARKER}') ON CONFLICT (id) DO UPDATE SET marker = EXCLUDED.marker;\""
    );
    context
        .kubectl
        .exec(namespace, "statefulset/database", &["sh", "-c", &seed])?;

    let tables: u64 = psql(
        context,
        namespace,
        "SELECT count(*) FROM pg_tables WHERE schemaname = 'public';",
    )?
    .trim()
    .parse()?;
    if tables < 2 {
        bail!("Nutshell did not initialize its PostgreSQL schema: {tables} tables");
    }

    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if context.kubectl.digest(&database_secret_args)? != database_digest {
        bail!("controller restart rotated the PostgreSQL Secret");
    }
    if context.kubectl.digest(&mint_secret_args)? != mint_digest {
        bail!("controller restart rotated the Nutshell private key");
    }

    context
        .kubectl
        .rollout_restart(namespace, "statefulset/database")?;
    context
        .kubectl
        .rollout_restart(namespace, "deployment/mint")?;
    let persisted = psql(
        context,
        namespace,
        "SELECT marker FROM proofstorm_acceptance WHERE id = 1;",
    )?;
    if persisted.trim() != MARKER {
        bail!(
            "PostgreSQL state did not survive restart: {:?}",
            persisted.trim()
        );
    }

    lab::wait_phase(&mut client, INSTANCE, "ready", 80, Duration::from_secs(3))?;

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(&mut client, INSTANCE, "closed", 80, Duration::from_secs(3))?;

    println!(
        "Nutshell 0.20.3 + PostgreSQL secret stability, database persistence, mint restart, readiness, and teardown passed"
    );
    Ok(())
}
