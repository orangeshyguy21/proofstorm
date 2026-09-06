//! One-way upgrade of persisted lease history into passive sessions.
use super::{Connection, StoreError, TransactionBehavior, now_unix, params};
use serde_json::{Value, json};

fn columns(db: &Connection, table: &str) -> Result<Vec<String>, StoreError> {
    Ok(db
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?)
}

#[allow(
    clippy::too_many_lines,
    reason = "one atomic, one-way upgrade preserves authority and journal history"
)]
pub(super) fn prepare(db: &mut Connection) -> Result<(), StoreError> {
    if columns(db, "experiment_leases")?.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if columns(&tx, "experiment_leases")?.is_empty() {
        return Ok(());
    }
    tx.execute_batch("DROP INDEX IF EXISTS one_active_lease_per_instance; ALTER TABLE experiment_leases RENAME TO sessions;
        ALTER TABLE sessions RENAME COLUMN acquired_at TO started_at;
        ALTER TABLE sessions RENAME COLUMN released_at TO finished_at;
        ALTER TABLE sessions ADD COLUMN last_activity_at INTEGER NOT NULL DEFAULT 0;
        UPDATE sessions SET last_activity_at=COALESCE(finished_at,started_at);
        UPDATE sessions SET phase_json='\"finished\"',finished_at=COALESCE(finished_at,unixepoch()) WHERE phase_json!='\"active\"';")?;
    for table in [
        "actions",
        "wallet_payment_claims",
        "wallet_quote_observations",
    ] {
        if columns(&tx, table)?.iter().any(|c| c == "lease_id") {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN lease_id TO session_id;"
            ))?;
        }
    }
    tx.execute_batch("UPDATE sessions SET last_activity_at=MAX(last_activity_at,COALESCE((SELECT MAX(COALESCE(completed_at,started_at,accepted_at)) FROM actions WHERE actions.workspace_id=sessions.workspace_id AND actions.session_id=sessions.id),last_activity_at));")?;
    // Legacy scopes are authority records, independent from the new session intervals.
    create_grants(&tx)?;
    if columns(&tx, "sessions")?
        .iter()
        .any(|c| c == "delegation_json")
    {
        let rows=tx.prepare("SELECT child.workspace_id,child.id,child.instance_id,child.principal_id,child.delegation_json,child.started_at,child.finished_at,parent.principal_id,parent.finished_at FROM sessions child LEFT JOIN sessions parent ON parent.workspace_id=child.workspace_id AND parent.id=json_extract(child.delegation_json,'$.parent_lease_id') WHERE child.delegation_json IS NOT NULL")?
            .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<i64>>(8)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (
            workspace,
            id,
            instance,
            principal,
            scope,
            created,
            revoked,
            issuer,
            parent_finished,
        ) in rows
        {
            let mut scope: Value = serde_json::from_str(&scope)?;
            scope
                .as_object_mut()
                .ok_or_else(|| StoreError::Validation("invalid legacy scope".into()))?
                .remove("parent_lease_id");
            scope["issuer_principal_id"] = json!(issuer.clone().unwrap_or_default());
            let grant = proofstorm_core::PrivateAccessGrant {
                id: id.clone(),
                workspace_id: workspace.clone(),
                instance_id: instance,
                principal_id: principal,
                scope: serde_json::from_value(scope)?,
                created_at_unix: created,
                revoked_at_unix: revoked
                    .or(parent_finished)
                    .or_else(|| issuer.is_none().then_some(now_unix())),
            };
            tx.execute(
                "INSERT INTO private_access_grants(workspace_id,id,grant_json) VALUES(?1,?2,?3)",
                params![workspace, id, serde_json::to_string(&grant)?],
            )?;
        }
        tx.execute_batch("ALTER TABLE sessions DROP COLUMN delegation_json;")?;
    }
    for (table, retired) in [
        ("sessions", &["expires_at", "max_actions"][..]),
        ("lab_handles", &["duration_seconds", "max_actions"][..]),
    ] {
        for column in retired {
            if columns(&tx, table)?.iter().any(|c| c == column) {
                tx.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))?;
            }
        }
    }
    tx.execute_batch("UPDATE grants SET capability='lab.operate' WHERE capability='lease.acquire'; DELETE FROM grants WHERE capability='lease.release';")?;
    // Normalize public coordination keys in durable receipts, preserving payload fields.
    for (table, column) in [("idempotency", "response_json")] {
        if columns(&tx, table)?.iter().any(|c| c == column) {
            let rows = tx
                .prepare(&format!("SELECT rowid,{column} FROM {table}"))?
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            for (rowid, encoded) in rows {
                let mut value: Value = serde_json::from_str(&encoded)?;
                normalize(&mut value);
                tx.execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                    params![value.to_string(), rowid],
                )?;
            }
        }
    }
    // Rehash operation envelopes from their retained request, so exact retries survive upgrade.
    let rows = tx
        .prepare(
            "SELECT rowid,response_json FROM idempotency WHERE operation='lab.operation.create'",
        )?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (rowid, encoded) in rows {
        let response: Value = serde_json::from_str(&encoded)?;
        let mut request = response["request"].clone();
        if let Some(object) = request.as_object_mut() {
            if let Some(id) = object.remove("lease_id") {
                object.insert("session_id".into(), id);
            }
        }
        let envelope = json!({"instanceId":response["instance_id"],"experimentId":response["experiment_id"],"sessionId":response["session_id"],"operationId":response["id"],"kind":response["kind"],"request":request});
        tx.execute(
            "UPDATE idempotency SET request_hash=?1 WHERE rowid=?2",
            params![proofstorm_core::digest_json(&envelope), rowid],
        )?;
    }
    // Obsolete coordination RPC receipts are history, never consulted by session admission.
    tx.commit()?;
    Ok(())
}
fn normalize(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(normalize),
        Value::Object(object) => {
            if object.contains_key("experiment_id") && object.contains_key("acquired_at_unix") {
                if let Some(v) = object.remove("acquired_at_unix") {
                    object.insert("started_at_unix".into(), v.clone());
                    object.insert("last_activity_at_unix".into(), v);
                }
                if let Some(v) = object.remove("released_at_unix") {
                    object.insert("finished_at_unix".into(), v);
                }
                if object.get("phase").and_then(Value::as_str) != Some("active") {
                    object.insert("phase".into(), json!("finished"));
                }
                for key in ["expires_at_unix", "max_actions", "delegation"] {
                    object.remove(key);
                }
            }
            if let Some(v) = object.remove("lease_id") {
                object.insert("session_id".into(), v);
            }
            for (key, child) in object.iter_mut() {
                if !matches!(key.as_str(), "request" | "artifact" | "content") {
                    normalize(child);
                }
            }
        }
        _ => {}
    }
}
fn create_grants(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS private_access_grants(workspace_id TEXT NOT NULL,id TEXT NOT NULL,grant_json TEXT NOT NULL,PRIMARY KEY(workspace_id,id));")?;
    Ok(())
}
pub(super) fn upgrade(db: &mut Connection) -> Result<(), StoreError> {
    create_grants(db)
}
