//! Retire session identity from existing custody bindings without touching payload bytes.
use crate::{Error, Result};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn upgrade(db: &mut Connection) -> Result<()> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)?;
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    if version >= 1 {
        return Ok(());
    }
    let rows=tx.prepare("SELECT id,request_key,request_json,source_json,destination_json,metadata FROM transfers").map_err(|_|Error::Storage)?
        .query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).map_err(|_|Error::Storage)?
        .collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Storage)?;
    // Move old keys out of the way before assigning their normalized names.
    tx.execute("UPDATE transfers SET request_key='migration:' || id", [])
        .map_err(|_| Error::Storage)?;
    let mut keys = BTreeSet::new();
    for (id, key, request, source, destination, metadata) in rows {
        let source = owner(&source)?;
        let mut metadata: Value = serde_json::from_str(&metadata).map_err(|_| Error::Storage)?;
        let destination = if metadata.get("recipient").is_none_or(Value::is_null) {
            owner(&destination)?
        } else {
            destination
        };
        if let Some(recipient) = metadata.get_mut("recipient").and_then(Value::as_object_mut) {
            if let Some(authority) = recipient
                .remove("lease")
                .or_else(|| recipient.remove("session"))
            {
                recipient.insert("authority".into(), authority);
            }
            recipient.remove("expires_at_unix");
        }
        let mut request: Vec<Value> = serde_json::from_str(&request).map_err(|_| Error::Storage)?;
        if request.len() != 3 {
            return Err(Error::Storage);
        }
        for identity in &mut request[..2] {
            *identity = Value::String(owner(identity.as_str().ok_or(Error::Storage)?)?);
        }
        let parts = key.splitn(3, '/').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(Error::Storage);
        }
        let next_key = format!("{}/owner/{}", parts[0], parts[2]);
        let encoded = serde_json::to_string(&request).map_err(|_| Error::Storage)?;
        // Formerly different sessions could reuse one key. Preserve both handles,
        // and reject that now-ambiguous key instead of silently replaying either.
        let (next_key, encoded) = if keys.insert(next_key.clone()) {
            (next_key, encoded)
        } else {
            tx.execute(
                "UPDATE transfers SET request_json='legacy-ambiguous' WHERE request_key=?1",
                [&next_key],
            )
            .map_err(|_| Error::Storage)?;
            (format!("legacy/{id}/{key}"), encoded)
        };
        tx.execute("UPDATE transfers SET request_key=?1,request_json=?2,source_json=?3,destination_json=?4,metadata=?5 WHERE id=?6",params![next_key,encoded,source,destination,metadata.to_string(),id]).map_err(|_|Error::Storage)?;
    }
    tx.pragma_update(None, "user_version", 1)
        .map_err(|_| Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)
}
fn owner(encoded: &str) -> Result<String> {
    let mut identity: Vec<String> = serde_json::from_str(encoded).map_err(|_| Error::Storage)?;
    if identity.len() != 5 {
        return Err(Error::Storage);
    }
    identity[4] = "owner".into();
    serde_json::to_string(&identity).map_err(|_| Error::Storage)
}
