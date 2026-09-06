//! Stable developer-facing names over immutable lab instances.
use super::{Capability, Store, StoreError, TransactionBehavior, is_slug, params};
use rusqlite::OptionalExtension;

pub use proofstorm_view::{LabHandle, LabHandlePhase};

fn read(
    db: &rusqlite::Connection,
    workspace: &str,
    name: &str,
) -> Result<Option<LabHandle>, StoreError> {
    let row = db.query_row(
        "SELECT generation,owner,config_digest,phase FROM lab_handles WHERE workspace_id=?1 AND name=?2",
        params![workspace,name], |row| Ok((row.get::<_,u32>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?))
    ).optional()?;
    row.map(|(generation, owner, config_digest, phase)| {
        let identity = proofstorm_core::digest_json(&(workspace, name, generation));
        Ok(LabHandle {
            name: name.into(),
            generation,
            owner,
            config_digest,
            phase: serde_json::from_str(&phase)?,
            instance_id: format!("lab-{}", &identity[7..31]),
        })
    })
    .transpose()
}

impl Store {
    /// Reserve a name before provisioning. Retry resumes the same instance;
    /// a verified closed generation can be replaced without reusing execution identities.
    pub fn reserve_lab(
        &self,
        workspace: &str,
        principal: &str,
        name: &str,
        config_digest: &str,
    ) -> Result<LabHandle, StoreError> {
        self.authorize(workspace, principal, Capability::LabCreate)?;
        if !is_slug(name) {
            return Err(StoreError::Validation(
                "lab name must be a lowercase kebab-case identifier of 1..=63 bytes".into(),
            ));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = read(&tx, workspace, name)?;
        if let Some(ref handle) = existing {
            if handle.owner != principal && handle.phase == LabHandlePhase::Closed {
                return Err(StoreError::Validation(
                    "lab name belongs to another principal".into(),
                ));
            }
            if handle.phase != LabHandlePhase::Closed {
                if handle.config_digest != config_digest {
                    return Err(StoreError::Validation("lab already exists with different configuration; close it before replacing it".into()));
                }
                return Ok(handle.clone());
            }
        }
        let generation = existing.map_or(Ok(1), |h| {
            h.generation
                .checked_add(1)
                .ok_or_else(|| StoreError::Validation("lab generation exhausted".into()))
        })?;
        let identity = proofstorm_core::digest_json(&(workspace, name, generation));
        let instance_id = format!("lab-{}", &identity[7..31]);
        tx.execute("INSERT INTO lab_handles(workspace_id,name,generation,owner,config_digest,phase,instance_id) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(workspace_id,name) DO UPDATE SET generation=excluded.generation,owner=excluded.owner,config_digest=excluded.config_digest,phase=excluded.phase,instance_id=excluded.instance_id",
            params![workspace,name,generation,principal,config_digest,serde_json::to_string(&LabHandlePhase::Open)?,instance_id])?;
        let handle = read(&tx, workspace, name)?
            .ok_or_else(|| StoreError::Validation("reserved lab disappeared".into()))?;
        tx.commit()?;
        Ok(handle)
    }

    pub fn lab_handle(
        &self,
        workspace: &str,
        principal: &str,
        name: &str,
    ) -> Result<LabHandle, StoreError> {
        self.authorize(workspace, principal, Capability::LabStatus)?;
        read(&*self.lock()?, workspace, name)?.ok_or_else(|| StoreError::NotFound {
            resource: "lab",
            id: name.into(),
        })
    }

    /// A monotonic shutdown latch. It never reopens authority.
    pub fn set_lab_phase(
        &self,
        workspace: &str,
        principal: &str,
        handle: &LabHandle,
        phase: LabHandlePhase,
    ) -> Result<(), StoreError> {
        self.authorize(workspace, principal, Capability::LabClose)?;
        if phase == LabHandlePhase::Open {
            return Err(StoreError::Validation(
                "only up can create an open generation".into(),
            ));
        }
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read(&tx, workspace, &handle.name)?.ok_or_else(|| StoreError::NotFound {
            resource: "lab",
            id: handle.name.clone(),
        })?;
        if current.owner != principal || current.generation != handle.generation {
            return Err(StoreError::Validation(
                "lab owner or generation changed".into(),
            ));
        }
        if current.phase != LabHandlePhase::Closed {
            tx.execute(
                "UPDATE lab_handles SET phase=?1 WHERE workspace_id=?2 AND name=?3",
                params![serde_json::to_string(&phase)?, workspace, handle.name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
