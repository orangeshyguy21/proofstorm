//! Explicit private-transfer permissions, independent of passive sessions.
use super::{
    Capability, LabOperation, OperationKind, OptionalExtension, Store, StoreError, is_slug,
    now_unix, params, validate_session_request,
};
use proofstorm_core::{ComponentKind, PrivateAccessGrant, PrivateTransferScope};

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn issue_private_access(
        &self,
        workspace: &str,
        principal: &str,
        recipient: &str,
        id: &str,
        instance: &str,
        scope: &PrivateTransferScope,
        key: &str,
    ) -> Result<PrivateAccessGrant, StoreError> {
        self.authorize(workspace, principal, Capability::LabOperate)?;
        self.authorize(workspace, principal, Capability::ComponentExecLive)?;
        self.authorize(workspace, recipient, Capability::ComponentExecLive)?;
        validate_session_request(id)?;
        if scope.issuer_principal_id != principal
            || principal == recipient
            || !is_slug(&scope.component)
            || !is_slug(&scope.mint)
            || scope.reference.is_empty()
            || scope.reference.len() > 128
            || !scope
                .receive_command_digest
                .strip_prefix("sha256:")
                .is_some_and(|d| d.len() == 64 && d.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(StoreError::Validation(
                "invalid private transfer permission".into(),
            ));
        }
        let (_, revision) = self.operation_context(
            workspace,
            principal,
            instance,
            Capability::ComponentExecLive,
        )?;
        for (id, kind) in [
            (&scope.component, ComponentKind::Wallet),
            (&scope.mint, ComponentKind::Mint),
        ] {
            if !revision
                .lab
                .components
                .iter()
                .any(|c| &c.id == id && c.kind == kind)
            {
                return Err(StoreError::Validation(
                    "private scope endpoints must belong to the lab".into(),
                ));
            }
        }
        let request =
            serde_json::json!({"recipient":recipient,"id":id,"instance":instance,"scope":scope});
        if let Some(previous) = self.idempotent_response::<PrivateAccessGrant, _>(
            workspace,
            principal,
            key,
            "private_access.issue",
            &request,
        )? {
            return self.private_access(workspace, principal, &previous.id);
        }
        let grant = PrivateAccessGrant {
            id: id.into(),
            workspace_id: workspace.into(),
            instance_id: instance.into(),
            principal_id: recipient.into(),
            scope: scope.clone(),
            created_at_unix: now_unix(),
            revoked_at_unix: None,
        };
        let encoded = serde_json::to_string(&grant)?;
        let db = self.lock()?;
        let existing: Option<String> = db
            .query_row(
                "SELECT grant_json FROM private_access_grants WHERE workspace_id=?1 AND id=?2",
                params![workspace, id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let previous: PrivateAccessGrant = serde_json::from_str(&existing)?;
            if previous.instance_id != instance
                || previous.principal_id != recipient
                || previous.scope != *scope
            {
                return Err(StoreError::Conflict {
                    resource: "private access grant",
                    id: id.into(),
                });
            }
            return Ok(previous);
        }
        db.execute(
            "INSERT INTO private_access_grants(workspace_id,id,grant_json) VALUES(?1,?2,?3)",
            params![workspace, id, encoded],
        )?;
        drop(db);
        self.record_idempotency(
            workspace,
            principal,
            key,
            "private_access.issue",
            &request,
            &grant,
        )?;
        Ok(grant)
    }
    pub fn private_access(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<PrivateAccessGrant, StoreError> {
        self.authorize(workspace, principal, Capability::ExperimentRead)?;
        let encoded: String = self
            .lock()?
            .query_row(
                "SELECT grant_json FROM private_access_grants WHERE workspace_id=?1 AND id=?2",
                params![workspace, id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                resource: "private access grant",
                id: id.into(),
            })?;
        Ok(serde_json::from_str(&encoded)?)
    }
    pub fn revoke_private_access(
        &self,
        workspace: &str,
        principal: &str,
        id: &str,
    ) -> Result<PrivateAccessGrant, StoreError> {
        let mut grant = self.private_access(workspace, principal, id)?;
        if principal != grant.principal_id && principal != grant.scope.issuer_principal_id {
            return Err(StoreError::Validation(
                "private access can only be revoked by its issuer or recipient".into(),
            ));
        }
        grant.revoked_at_unix = Some(grant.revoked_at_unix.unwrap_or_else(now_unix));
        self.lock()?.execute(
            "UPDATE private_access_grants SET grant_json=?1 WHERE workspace_id=?2 AND id=?3",
            params![serde_json::to_string(&grant)?, workspace, id],
        )?;
        Ok(grant)
    }
    fn matching_access(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        kind: OperationKind,
        request: &serde_json::Value,
    ) -> Result<Option<PrivateAccessGrant>, StoreError> {
        let db = self.lock()?;
        let rows=db.prepare("SELECT grant_json FROM private_access_grants WHERE workspace_id=?1 AND json_extract(grant_json,'$.principal_id')=?2 AND json_extract(grant_json,'$.instance_id')=?3 AND json_extract(grant_json,'$.revoked_at_unix') IS NULL ORDER BY id")?
            .query_map(params![workspace,principal,instance],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let grants = rows
            .iter()
            .map(|s| serde_json::from_str::<PrivateAccessGrant>(s))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(grants.into_iter().find(|g| g.scope.permits(kind, request)))
    }
    pub(super) fn authorize_operation_access(
        &self,
        workspace: &str,
        principal: &str,
        instance: &str,
        kind: OperationKind,
        request: &serde_json::Value,
    ) -> Result<(), StoreError> {
        if self
            .authorize(workspace, principal, Capability::LabOperate)
            .is_ok()
            || self
                .matching_access(workspace, principal, instance, kind, request)?
                .is_some()
        {
            return Ok(());
        }
        Err(StoreError::AccessDenied {
            workspace: workspace.into(),
            principal: principal.into(),
            capability: Capability::LabOperate,
        })
    }
    pub fn operation_access_scope(
        &self,
        operation: &LabOperation,
    ) -> Result<Option<PrivateAccessGrant>, StoreError> {
        self.matching_access(
            &operation.workspace_id,
            &operation.principal_id,
            &operation.instance_id,
            operation.kind,
            &operation.request,
        )
    }
}
