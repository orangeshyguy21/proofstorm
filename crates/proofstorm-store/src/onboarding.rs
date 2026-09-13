//! Explicit local onboarding, never called by normal agent startup.
use crate::{Capability, Store, StoreError, capability_name};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

impl Store {
    /// Atomically create a new actor and its preset receipt. Replays never regrant.
    pub fn initialize_actor_once(
        &self,
        workspace: &str,
        principal: &str,
        identity: &str,
        preset: &str,
        capabilities: &[Capability],
    ) -> Result<bool, StoreError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_actor_presets (
            principal_id TEXT PRIMARY KEY REFERENCES principals(id),
            workspace_id TEXT NOT NULL REFERENCES workspaces(id),
            identity TEXT NOT NULL, preset TEXT NOT NULL
        )",
        )?;
        let old: Option<(String, String, String)> = tx.query_row(
            "SELECT workspace_id, identity, preset FROM local_actor_presets WHERE principal_id=?1",
            [principal], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        if let Some(old) = old {
            if old != (workspace.into(), identity.into(), preset.into()) {
                return Err(StoreError::Validation(
                    "attachment identity or preset changed; explicit migration required".into(),
                ));
            }
            return Ok(false);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM principals WHERE id=?1)",
            [principal],
            |row| row.get(0),
        )?;
        if exists {
            return Err(StoreError::Validation(
                "refusing to adopt an existing manually configured actor".into(),
            ));
        }
        tx.execute("INSERT INTO principals(id) VALUES (?1)", [principal])?;
        for capability in capabilities {
            tx.execute(
                "INSERT INTO grants(workspace_id,principal_id,capability) VALUES (?1,?2,?3)",
                params![workspace, principal, capability_name(*capability)?],
            )?;
        }
        tx.execute("INSERT INTO local_actor_presets(principal_id,workspace_id,identity,preset) VALUES (?1,?2,?3,?4)",
            params![principal,workspace,identity,preset])?;
        tx.commit()?;
        Ok(true)
    }

    /// Read-only startup check; never creates a receipt or restores permissions.
    pub fn actor_preset(
        &self,
        workspace: &str,
        principal: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.lock()?;
        let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_actor_presets')", [], |row| row.get(0))?;
        if !exists {
            return Ok(None);
        }
        Ok(connection
            .query_row(
                "SELECT preset FROM local_actor_presets WHERE workspace_id=?1 AND principal_id=?2",
                params![workspace, principal],
                |row| row.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actor_creation_is_atomic_repeatable_and_never_regrants() {
        let store = Store::memory().unwrap();
        assert!(store.actor_preset("w", "a").unwrap().is_none());
        assert!(
            store
                .initialize_actor_once("missing", "a", "identity", "v1", &[Capability::CellRead])
                .is_err()
        );
        store
            .put_workspace(&crate::Workspace {
                id: "w".into(),
                name: "w".into(),
            })
            .unwrap();
        assert!(
            store
                .initialize_actor_once("w", "a", "identity", "v1", &[Capability::CellRead])
                .unwrap()
        );
        store.revoke("w", "a", Capability::CellRead).unwrap();
        assert!(
            !store
                .initialize_actor_once("w", "a", "identity", "v1", &[Capability::CellRead])
                .unwrap()
        );
        assert!(store.capabilities("w", "a").unwrap().is_empty());
        assert!(
            store
                .initialize_actor_once("w", "a", "foreign", "v1", &[])
                .is_err()
        );
        store.put_principal("manual").unwrap();
        assert!(
            store
                .initialize_actor_once("w", "manual", "identity", "v1", &[])
                .is_err()
        );
    }
}
