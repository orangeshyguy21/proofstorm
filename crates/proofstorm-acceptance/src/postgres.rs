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
pub fn augment_lab(enabled: bool, lab: &mut Value, database_name: &str) {
    if !enabled {
        return;
    }
    if let Some(name) = lab.get("name").and_then(Value::as_str).map(str::to_owned) {
        lab["name"] = Value::from(format!("{name}-postgres"));
    }
    if let Some(components) = lab.get_mut("components").and_then(Value::as_array_mut) {
        components.push(json!({
            "id": "database",
            "kind": "database",
            "implementation": "postgresql",
            "version": "17.11",
            "config_version": "postgresql/17/v1",
            "control": "laboratory",
            "config": {"database_name": database_name, "storage_size": "2Gi"}
        }));
    }
    if let Some(links) = lab.get_mut("links").and_then(Value::as_array_mut) {
        links.push(json!({
            "id": "mint-database",
            "kind": "database_backend",
            "from": "mint",
            "to": "database",
            "binding": {"type": "database", "role": "primary"}
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
    if keys
        != [
            "DATABASE_URL",
            "POSTGRES_DB",
            "POSTGRES_PASSWORD",
            "POSTGRES_USER",
            "database.toml",
        ]
    {
        bail!("generated PostgreSQL Secret has an unexpected key contract: {keys:?}");
    }

    let database_url = decode_base64(expect::string(&secret, "/data/DATABASE_URL")?)
        .context("decode the generated database URL")?;
    if !database_url.contains(&format!("@database:5432/{database_name}")) {
        bail!("private PostgreSQL URL does not target the selected database");
    }

    let deployment = kubectl.get_json(&["get", "deployment/mint", "-n", namespace])?;
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
        let url = container
            .get("env")
            .and_then(Value::as_array)
            .and_then(|env| {
                env.iter().find(|entry| {
                    entry.get("name").and_then(Value::as_str) == Some("CDK_MINTD_POSTGRES_URL")
                })
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} does not receive the secret-backed PostgreSQL URL",
                    container["name"]
                )
            })?;
        let reference = url.pointer("/valueFrom/secretKeyRef");
        if reference != Some(&json!({"name": "database-credentials", "key": "DATABASE_URL"})) {
            bail!(
                "{} does not receive the secret-backed PostgreSQL URL",
                container["name"]
            );
        }
    }

    let count = psql(
        kubectl,
        namespace,
        "SELECT count(*) FROM pg_tables WHERE schemaname = 'public';",
    )?;
    let tables: u64 = count
        .trim()
        .parse()
        .context("parse the schema table count")?;
    if tables < MINIMUM_TABLES {
        bail!("CDK initialized only {tables} PostgreSQL schema tables");
    }
    Ok(tables)
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

fn psql(kubectl: &Kubectl, namespace: &str, statement: &str) -> Result<String> {
    let script = format!(
        "PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -At -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\" -c \"{statement}\""
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
