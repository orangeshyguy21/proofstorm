//! CDK 0.18.0 + PostgreSQL: catalog-pinned image, secret-backed URL kept out of
//! every public object, schema initialization, Secret stability across a
//! controller restart, and persistence across a workload restart.
//!
//! Ported from `tests/kubernetes/cdk_postgres_mcp_client.py`.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, gate::CONTROL_NAMESPACE, json as expect, lab};

const INSTANCE: &str = "cdk-postgres-instance";
const DRAFT: &str = "cdk-postgres";
const MARKER: &str = "persistent";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-postgres-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-postgres-lnd"}},
            {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "laboratory", "config": {"database_name": "proofstorm_mint", "storage_size": "2Gi"}},
            {"id": "mint", "kind": "mint", "implementation": "cdk", "version": "0.18.0", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK PostgreSQL", "description": "Secret-backed PostgreSQL persistence acceptance", "mint_quote_ttl_seconds": 601, "melt_quote_ttl_seconds": 121}}
        ],
        "links": [
            {"id": "lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
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
    let mut client = context.session("cdk-postgres-live", "designer", LIFECYCLE_CAPABILITIES)?;

    let catalog = client.call(
        "proofstorm_catalog_list",
        json!({"implementations": ["cdk", "postgresql"]}),
    )?;
    let items = expect::array(&catalog, "/items")?;
    let postgres = items
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some("postgresql"))
        .ok_or_else(|| anyhow::anyhow!("PostgreSQL is absent from the catalog: {catalog}"))?;
    if expect::string(postgres, "/version")? != "17.11" {
        bail!("PostgreSQL 17.11 is absent from the catalog: {postgres}");
    }
    let postgres_detail = client.call(
        "proofstorm_catalog_entry_read",
        json!({"id": "postgresql", "version": "17.11"}),
    )?;

    let cdk_summary = items
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some("cdk"))
        .ok_or_else(|| anyhow::anyhow!("CDK is absent from the catalog"))?;
    let cdk = client.call(
        "proofstorm_catalog_entry_read",
        json!({
            "id": "cdk",
            "version": expect::string(cdk_summary, "/version")?
        }),
    )?;
    let advertises = expect::array(&cdk, "/features")?
        .iter()
        .any(|feature| feature.as_str() == Some("postgres"))
        && expect::array(&cdk, "/support_matrix/storage")?
            .iter()
            .any(|storage| storage.as_str() == Some("postgres"));
    if !advertises {
        bail!("CDK does not advertise its typed PostgreSQL support");
    }

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-cdk-postgres"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-cdk-postgres", "include_revision": true}),
    )?;

    let database_lock = lab::lock_entry(&published, "postgresql")?;
    let locked_image = expect::string(database_lock, "/image")?;
    if locked_image != expect::string(&postgres_detail, "/image")?
        || !locked_image.contains("@sha256:")
    {
        bail!("PostgreSQL lock is not the catalog-pinned image: {database_lock}");
    }

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-cdk-postgres"}),
    )?;
    let ready = lab::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let public_config = context.kubectl.run(&[
        "get",
        "configmap/mint-config",
        "-n",
        namespace,
        "-o",
        r"jsonpath={.data.config\.toml}",
    ])?;
    if public_config.contains("postgresql://") || public_config.contains("@database:5432") {
        bail!("the public mint ConfigMap contains the private database URL");
    }
    if !public_config.contains("[database]\nengine = \"postgres\"") {
        bail!("the public mint ConfigMap is missing the CDK 0.18 database selector");
    }
    if !public_config.contains("url = \"env:CDK_MINTD_POSTGRES_URL\"") {
        bail!("the public mint ConfigMap does not use CDK 0.18's secret environment reference");
    }

    let private_config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in [
        "[database]\nengine = \"postgres\"",
        "url = \"env:CDK_MINTD_POSTGRES_URL\"",
    ] {
        if !private_config.contains(fragment) {
            bail!("materialized mint configuration is missing {fragment:?}");
        }
    }
    if private_config.contains("postgresql://") || private_config.contains("@database:5432") {
        bail!("materialized public configuration leaked the private database URL");
    }

    let deployment = context
        .kubectl
        .get_json(&["get", "deployment/mint", "-n", namespace])?;
    for group in ["initContainers", "containers"] {
        let containers = expect::array(&deployment, &format!("/spec/template/spec/{group}"))?;
        let first = containers
            .first()
            .ok_or_else(|| anyhow::anyhow!("{group} is empty"))?;
        let url = first
            .get("env")
            .and_then(Value::as_array)
            .and_then(|env| {
                env.iter().find(|entry| {
                    entry.get("name").and_then(Value::as_str) == Some("CDK_MINTD_POSTGRES_URL")
                })
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{group} does not receive the secret-backed PostgreSQL bootstrap URL"
                )
            })?;
        if url.pointer("/valueFrom/secretKeyRef")
            != Some(&json!({"name": "database-credentials", "key": "DATABASE_URL"}))
        {
            bail!("{group} does not receive the secret-backed PostgreSQL bootstrap URL");
        }
    }

    let secret_args = [
        "get",
        "secret/database-credentials",
        "-n",
        namespace,
        "-o",
        "json",
    ];
    let secret_digest = context.kubectl.digest(&secret_args)?;
    let secret =
        context
            .kubectl
            .get_json(&["get", "secret/database-credentials", "-n", namespace])?;
    let mut keys: Vec<&str> = expect::object(&secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    if keys
        != [
            "DATABASE_URL",
            "POSTGRES_DB",
            "POSTGRES_PASSWORD",
            "POSTGRES_USER",
            "database.toml",
        ]
    {
        bail!("generated database Secret has an unexpected key contract: {keys:?}");
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
        bail!("CDK did not initialize its PostgreSQL schema: only {tables} public tables");
    }

    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if context.kubectl.digest(&secret_args)? != secret_digest {
        bail!("controller reconciliation rotated or mutated the generated database Secret");
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

    lab::wait_phase(&mut client, INSTANCE, "ready", 60, Duration::from_secs(3))?;
    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(&mut client, INSTANCE, "closed", 60, Duration::from_secs(3))?;

    println!(
        "CDK 0.18.0 + PostgreSQL MCP materialization, database-backed configuration, Secret preservation, restart persistence, and teardown passed"
    );
    Ok(())
}
