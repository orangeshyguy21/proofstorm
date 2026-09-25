//! Optional PostgreSQL storage variant shared by several gates.
//!
//! Ported from `tests/kubernetes/postgres_acceptance.py`. Each gate runs twice
//! in CI: once on SQLite and once with `PROOFSTORM_STORAGE=postgres`, which
//! appends a database component and asserts the private URL never reaches a
//! public object.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{Kubectl, json as expect};

/// Minimum schema tables a healthy CDK PostgreSQL initialization creates.
const MINIMUM_TABLES: u64 = 13;

/// Whether the ambient environment selects the PostgreSQL variant.
///
/// Gate names ending in `-postgres` pass `true` directly; this keeps the older
/// `PROOFSTORM_STORAGE=postgres` wrapper contract working for the plain names.
pub fn enabled() -> bool {
    std::env::var("PROOFSTORM_STORAGE").as_deref() == Ok("postgres")
}

/// Append the database component and its link when the variant is active.
pub fn augment_cell(enabled: bool, cell: &mut Value, database_name: &str) {
    if !enabled {
        return;
    }
    if let Some(name) = cell.get("name").and_then(Value::as_str).map(str::to_owned) {
        cell["name"] = Value::from(format!("{name}-postgres"));
    }
    if let Some(components) = cell.get_mut("components").and_then(Value::as_array_mut) {
        components.push(json!({
            "id": "database",
            "kind": "database",
            "implementation": "postgresql",
            "version": "17.11",
            "config_version": "postgresql/17/v1",
            "control": "cell",
            "config": {"storage_size": "2Gi"}
        }));
    }
    if let Some(links) = cell.get_mut("links").and_then(Value::as_array_mut) {
        links.push(json!({
            "id": "mint-database",
            "kind": "database_backend",
            "from": "mint",
            "to": "database",
            "binding": {"type": "database", "role": "primary", "database": database_name}
        }));
    }
}

/// Verify the rendered storage contract, returning the schema table count.
///
/// On SQLite this only checks that the engine was rendered; the PostgreSQL
/// path additionally proves the private URL is secret-backed and absent from
/// every public object.
pub fn assert_materialized(
    enabled: bool,
    kubectl: &Kubectl,
    namespace: &str,
    private_config: &str,
    database_name: &str,
) -> Result<u64> {
    if !enabled {
        if !private_config.contains("[database]\nengine = \"sqlite\"") {
            bail!("SQLite scenario did not render its database engine");
        }
        return Ok(0);
    }

    let public_config = kubectl.run(&[
        "get",
        "configmap/mint-config",
        "-n",
        namespace,
        "-o",
        r"jsonpath={.data.config\.toml}",
    ])?;
    if public_config.contains("postgresql://") || public_config.contains("@database:5432") {
        bail!("public mint ConfigMap contains the private PostgreSQL URL");
    }

    for fragment in [
        "[database]\nengine = \"postgres\"",
        "[database.postgres]",
        "url = \"env:CDK_MINTD_POSTGRES_URL\"",
        "tls_mode = \"disable\"",
        "max_connections = 20",
        "connection_timeout_seconds = 10",
    ] {
        if !private_config.contains(fragment) {
            bail!("CDK PostgreSQL configuration is missing {fragment:?}");
        }
    }
    if private_config.contains("postgresql://") || private_config.contains("@database:5432") {
        bail!("materialized CDK configuration leaked the private PostgreSQL URL");
    }

    let secret = kubectl.get_json(&["get", "secret/database-credentials", "-n", namespace])?;
    let mut keys: Vec<&str> = expect::object(&secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    if keys != ["POSTGRES_DB", "POSTGRES_PASSWORD", "POSTGRES_USER"] {
        bail!("generated PostgreSQL Secret has an unexpected key contract: {keys:?}");
    }

    let deployment = kubectl.get_json(&["get", "deployment/mint", "-n", namespace])?;
    let init = expect::array(&deployment, "/spec/template/spec/initContainers")?;
    if !init
        .iter()
        .any(|entry| entry.get("name").and_then(Value::as_str) == Some("ensure-database"))
    {
        bail!("mint does not create its own PostgreSQL database before initialization");
    }
    for group in ["initContainers", "containers"] {
        let containers = expect::array(&deployment, &format!("/spec/template/spec/{group}"))?;
        let container = containers
            .iter()
            .find(|entry| {
                matches!(
                    entry.get("name").and_then(Value::as_str),
                    Some("initialize-config" | "component")
                )
            })
            .ok_or_else(|| anyhow::anyhow!("no configuration container in {group}"))?;
        let env = container
            .get("env")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("{} has no environment", container["name"]))?;
        let named = |name: &str| {
            env.iter()
                .position(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        };
        // The password is secret-backed and declared before the URL expanding it.
        let (Some(password), Some(url)) = (
            named("CDK_MINTD_POSTGRES_URL_PASSWORD"),
            named("CDK_MINTD_POSTGRES_URL"),
        ) else {
            bail!(
                "{} does not receive the secret-backed PostgreSQL URL",
                container["name"]
            );
        };
        if password > url
            || env[password].pointer("/valueFrom/secretKeyRef")
                != Some(&json!({"name": "database-credentials", "key": "POSTGRES_PASSWORD"}))
            || env[url]["value"]
                != format!(
                    "postgresql://proofstorm:$(CDK_MINTD_POSTGRES_URL_PASSWORD)@database:5432/{database_name}"
                )
        {
            bail!(
                "{} does not compose its own database URL from the owner secret",
                container["name"]
            );
        }
    }

    let tables = schema_table_count(kubectl, namespace, database_name)?;
    if tables < MINIMUM_TABLES {
        bail!("CDK initialized only {tables} PostgreSQL schema tables");
    }
    Ok(tables)
}

/// Count public schema tables in one component's database; each gate chooses
/// its own initialization threshold.
pub(crate) fn schema_table_count(
    kubectl: &Kubectl,
    namespace: &str,
    database: &str,
) -> Result<u64> {
    psql(
        kubectl,
        namespace,
        database,
        "SELECT count(*) FROM pg_tables WHERE schemaname = 'public';",
    )?
    .trim()
    .parse()
    .context("parse the schema table count")
}

/// Write a marker row that must survive a database restart.
pub fn seed_sentinel(
    enabled: bool,
    kubectl: &Kubectl,
    namespace: &str,
    marker: &str,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    let script = format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -v ON_ERROR_STOP=1 -U \"$POSTGRES_USER\" \
         -d \"$POSTGRES_DB\" -c \"CREATE TABLE IF NOT EXISTS proofstorm_acceptance \
         (id integer primary key, marker text not null);\" \
         -c \"INSERT INTO proofstorm_acceptance VALUES (1, '{marker}') \
         ON CONFLICT (id) DO UPDATE SET marker = EXCLUDED.marker;\""
    );
    kubectl.exec(namespace, "statefulset/database", &["sh", "-c", &script])?;
    Ok(())
}

/// Restart the database and wait for it to come back.
pub fn restart_database(enabled: bool, kubectl: &Kubectl, namespace: &str) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    kubectl.rollout_restart(namespace, "statefulset/database")
}

/// Prove the marker row survived the restart.
pub fn verify_sentinel(
    enabled: bool,
    kubectl: &Kubectl,
    namespace: &str,
    marker: &str,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    let persisted = psql(
        kubectl,
        namespace,
        "postgres",
        "SELECT marker FROM proofstorm_acceptance WHERE id = 1;",
    )?;
    if persisted.trim() != marker {
        bail!(
            "PostgreSQL sentinel did not survive restart: expected {marker:?}, got {:?}",
            persisted.trim()
        );
    }
    Ok(())
}

fn psql(kubectl: &Kubectl, namespace: &str, database: &str, statement: &str) -> Result<String> {
    let script = format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -At -U \"$POSTGRES_USER\" -d \"{database}\" -c \"{statement}\""
    );
    kubectl.exec(namespace, "statefulset/database", &["sh", "-c", &script])
}

/// Decode standard base64 without pulling in a dependency for one field.
pub fn decode_base64(encoded: &str) -> Result<String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0u32;
    let mut output = Vec::new();
    for byte in encoded.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let value = TABLE
            .iter()
            .position(|candidate| *candidate == byte)
            .ok_or_else(|| anyhow::anyhow!("invalid base64 byte {byte:?}"))?;
        bits = (bits << 6) | u32::try_from(value)?;
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push(u8::try_from((bits >> count) & 0xFF)?);
        }
    }
    String::from_utf8(output).context("decoded secret is not UTF-8")
}
