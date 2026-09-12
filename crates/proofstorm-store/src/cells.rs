//! Stable developer-facing names over immutable cell instances.
use super::{Capability, Store, StoreError, TransactionBehavior, is_slug, params};
use rusqlite::OptionalExtension;

pub use proofstorm_view::{CellHandle, CellHandlePhase};

fn read(
    db: &rusqlite::Connection,
    workspace: &str,
    name: &str,
) -> Result<Option<CellHandle>, StoreError> {
    let row = db.query_row(
        "SELECT generation,owner,config_digest,phase,instance_id FROM cell_handles WHERE workspace_id=?1 AND name=?2",
        params![workspace,name], |row| Ok((row.get::<_,u32>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?))
    ).optional()?;
    row.map(|(generation, owner, config_digest, phase, instance_id)| {
        Ok(CellHandle {
            name: name.into(),
            generation,
            owner,
            config_digest,
            phase: serde_json::from_str(&phase)?,
            instance_id,
        })
    })
    .transpose()
}

impl Store {
    /// Resolve a display name or canonical instance ID without creating bookkeeping.
    /// An unnamed instance has no recorded name owner; the empty owner is informational.
    /// Authority always comes from workspace capabilities, not this display projection.
    pub fn resolve_cell(
        &self,
        workspace: &str,
        principal: &str,
        reference: &str,
    ) -> Result<CellHandle, StoreError> {
        self.authorize(workspace, principal, Capability::CellStatus)?;
        let named = read(&*self.lock()?, workspace, reference)?;
        let exact = match self.instance_unchecked(workspace, reference) {
            Ok(instance) => Some(instance),
            Err(StoreError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        };
        if let Some(handle) = named {
            if exact.as_ref().is_some_and(|i| i.id != handle.instance_id) {
                return Err(StoreError::CellUpdate {
                    code: "cell_reference_ambiguous",
                    message: "This name also identifies another instance; use the intended canonical instance ID".into(),
                });
            }
            return self.cell_handle(workspace, principal, &handle.name);
        }
        let instance = exact.ok_or_else(|| StoreError::NotFound {
            resource: "cell",
            id: reference.into(),
        })?;
        let alias: Option<String> = self
            .lock()?
            .query_row(
                "SELECT name FROM cell_handles WHERE workspace_id=?1 AND instance_id=?2",
                params![workspace, instance.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(alias) = alias {
            return self.cell_handle(workspace, principal, &alias);
        }
        let revision = self.revision_unchecked(workspace, &instance.revision_digest)?;
        let closing = self
            .update_state(workspace, principal, &instance.id)?
            .closing;
        Ok(CellHandle {
            name: instance.id.clone(),
            instance_id: instance.id,
            generation: 1,
            owner: String::new(),
            config_digest: proofstorm_core::digest_json(&revision.cell),
            phase: if closing {
                CellHandlePhase::Closing
            } else {
                CellHandlePhase::Open
            },
        })
    }

    /// Reserve a name before provisioning. Retry resumes the same instance;
    /// a verified closed generation can be replaced without reusing execution identities.
    pub fn reserve_cell(
        &self,
        workspace: &str,
        principal: &str,
        name: &str,
        config_digest: &str,
    ) -> Result<CellHandle, StoreError> {
        self.authorize(workspace, principal, Capability::CellCreate)?;
        if !is_slug(name) {
            return Err(StoreError::Validation(
                "cell name must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = read(&tx, workspace, name)?;
        if let Some(ref handle) = existing {
            if handle.owner != principal && handle.phase == CellHandlePhase::Closed {
                return Err(StoreError::Validation(
                    "cell name belongs to another principal".into(),
                ));
            }
            if handle.phase != CellHandlePhase::Closed {
                if handle.config_digest != config_digest {
                    return Err(StoreError::Validation("cell already exists with different configuration; close it before replacing it".into()));
                }
                return Ok(handle.clone());
            }
        }
        let generation = existing.map_or(Ok(1), |h| {
            h.generation
                .checked_add(1)
                .ok_or_else(|| StoreError::Validation("cell generation exhausted".into()))
        })?;
        let nonce: String = tx.query_row("SELECT hex(randomblob(16))", [], |r| r.get(0))?;
        let identity = proofstorm_core::digest_json(&(workspace, name, generation, nonce));
        let instance_id = format!("cell-{}", &identity[7..31]);
        tx.execute("INSERT INTO cell_handles(workspace_id,name,generation,owner,config_digest,phase,instance_id) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(workspace_id,name) DO UPDATE SET generation=excluded.generation,owner=excluded.owner,config_digest=excluded.config_digest,phase=excluded.phase,instance_id=excluded.instance_id",
            params![workspace,name,generation,principal,config_digest,serde_json::to_string(&CellHandlePhase::Open)?,instance_id])?;
        let handle = read(&tx, workspace, name)?
            .ok_or_else(|| StoreError::Validation("reserved cell disappeared".into()))?;
        tx.commit()?;
        Ok(handle)
    }

    pub fn cell_handle(
        &self,
        workspace: &str,
        principal: &str,
        name: &str,
    ) -> Result<CellHandle, StoreError> {
        self.authorize(workspace, principal, Capability::CellStatus)?;
        let mut handle =
            read(&*self.lock()?, workspace, name)?.ok_or_else(|| StoreError::NotFound {
                resource: "cell",
                id: name.into(),
            })?;
        if handle.phase != CellHandlePhase::Closed
            && self
                .update_state(workspace, principal, &handle.instance_id)?
                .closing
        {
            handle.phase = CellHandlePhase::Closing;
        }
        Ok(handle)
    }

    /// A monotonic shutdown latch. It never reopens authority.
    pub fn set_cell_phase(
        &self,
        workspace: &str,
        principal: &str,
        handle: &CellHandle,
        phase: CellHandlePhase,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::CellClose)?;
        if phase == CellHandlePhase::Open {
            return Err(StoreError::Validation(
                "only up can create an open generation".into(),
            ));
        }
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read(&tx, workspace, &handle.name)?.ok_or_else(|| StoreError::NotFound {
            resource: "cell",
            id: handle.name.clone(),
        })?;
        if current.owner != principal || current.generation != handle.generation {
            return Err(StoreError::Validation(
                "cell owner or generation changed".into(),
            ));
        }
        if current.phase != CellHandlePhase::Closed {
            tx.execute(
                "UPDATE cell_handles SET phase=?1 WHERE workspace_id=?2 AND name=?3",
                params![serde_json::to_string(&phase)?, workspace, handle.name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
