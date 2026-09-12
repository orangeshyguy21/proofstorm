//! Shared controller custody. No payload body is ever serialized into an action.
use super::{
    Action, ActionPhase, Api, CellAction, Context, Error, Pod, ProofstormCell,
    ProofstormCellAction, ProofstormCellActionStatus, ResourceExt, instance_namespace, now_unix,
    patch_action_failure, patch_action_status, status_object,
};
use proofstorm_core::private_io::{PRIVATE_ACCESS_ANNOTATION, PayloadBinding, PrivateIo};
use proofstorm_core::{Capability, ComponentKind, OperationKind, PrivateAccessGrant};
use proofstorm_transfer::{
    Grant, Limits, NativeReceipt, PayloadManifest, ProducedPayload, Transfer, Vault,
};
use std::{
    io::Cursor,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

fn failure() -> Error {
    Error::LiveExec("private transfer unavailable or admission refused".into())
}
fn private<T>(result: Result<T, proofstorm_transfer::Error>) -> Result<T, Error> {
    result.map_err(|_| failure())
}

fn path(cell: &ProofstormCell) -> Result<PathBuf, Error> {
    if cell.spec.instance_key.is_empty()
        || !cell
            .spec
            .instance_key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || cell.spec.instance_key == "."
        || cell.spec.instance_key == ".."
    {
        return Err(failure());
    }
    let root = PathBuf::from(
        std::env::var("PROOFSTORM_PRIVATE_ROOT")
            .unwrap_or_else(|_| "/var/lib/proofstorm/private".into()),
    );
    if !root.exists() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|_| failure())?;
    }
    let meta = std::fs::symlink_metadata(&root).map_err(|_| failure())?;
    if !meta.is_dir() || meta.permissions().mode() & 0o777 != 0o700 {
        return Err(failure());
    }
    Ok(root.join(&cell.spec.instance_key))
}
fn vault(cell: &ProofstormCell) -> Result<Vault, Error> {
    private(Vault::open(
        &path(cell)?,
        &cell.spec.workspace_id,
        &cell.spec.instance_key,
        Limits::default(),
    ))
}
async fn live_cell(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<ProofstormCell, Error> {
    let cells = Api::<ProofstormCell>::namespaced(
        context.client.clone(),
        &action.namespace().ok_or_else(failure)?,
    );
    let cell = cells.get(&action.spec.cell_name).await?;
    if cell.spec.workspace_id != action.spec.workspace_id
        || cell.spec.instance_key != action.spec.instance_key
        || cell.spec.instance_id != action.spec.instance_id
        || cell.metadata.deletion_timestamp.is_some()
    {
        return Err(failure());
    }
    Ok(cell)
}
fn recipient_access(cell: &ProofstormCell, id: &str) -> Result<PrivateAccessGrant, Error> {
    let grants: std::collections::BTreeMap<String, PrivateAccessGrant> = serde_json::from_str(
        cell.annotations()
            .get(PRIVATE_ACCESS_ANNOTATION)
            .ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    let grant = grants.get(id).ok_or_else(failure)?;
    if grant.id != id
        || grant.workspace_id != cell.spec.workspace_id
        || grant.instance_id != cell.spec.instance_id
        || grant.revoked_at_unix.is_some()
    {
        return Err(failure());
    }
    Ok(grant.clone())
}

/// Validate every new delegated action, including typed observations, before dispatch.
pub fn validate_delegated_action(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
) -> Result<(), Error> {
    let Some(snapshot) = &action.spec.access_scope else {
        return Ok(());
    };
    let current = recipient_access(cell, &snapshot.id)?;
    if &current != snapshot
        || current.principal_id != action.spec.principal_id
        || current.instance_id != action.spec.instance_id
        || current.workspace_id != action.spec.workspace_id
    {
        return Err(failure());
    }
    let scope = &current.scope;
    let (kind, request) = match &action.spec.action {
        CellAction::WalletBalance(r) if action.spec.capability == Capability::WalletControl => (
            OperationKind::WalletBalance,
            serde_json::json!({"wallet":r.wallet,"mint":r.mint}),
        ),
        CellAction::PrivateTransfer(r)
            if action.spec.capability == Capability::ComponentExecLive =>
        {
            (
                OperationKind::PrivateTransfer,
                serde_json::json!({"transfer":r}),
            )
        }
        CellAction::ComponentExecLive(r)
            if action.spec.capability == Capability::ComponentExecLive =>
        {
            (
                OperationKind::ComponentExecLive,
                serde_json::json!({"component":r.component,"private_payload":r.private_payload,"output":r.output,"script":r.script,"argv":r.argv,"timeout_seconds":r.timeout_seconds}),
            )
        }
        _ => return Err(failure()),
    };
    if !scope.permits(kind, &request) {
        return Err(failure());
    }
    Ok(())
}

fn recipient_grant(access: &PrivateAccessGrant, cell: &ProofstormCell, wallet: &str) -> Grant {
    Grant {
        workspace: access.workspace_id.clone(),
        cell: cell.spec.instance_key.clone(),
        principal: access.principal_id.clone(),
        wallet: wallet.into(),
        authority: access.id.clone(),
    }
}

fn grant(
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    wallet: &str,
) -> Result<Grant, Error> {
    if action.spec.capability != Capability::ComponentExecLive
        || !cell
            .spec
            .cell
            .components
            .iter()
            .any(|c| c.id == wallet && c.kind == ComponentKind::Wallet)
    {
        return Err(failure());
    }
    validate_delegated_action(action, cell)?;
    if let Some(access) = &action.spec.access_scope {
        return Ok(recipient_grant(access, cell, wallet));
    }
    Ok(Grant {
        workspace: action.spec.workspace_id.clone(),
        cell: cell.spec.instance_key.clone(),
        principal: action.spec.principal_id.clone(),
        wallet: wallet.into(),
        authority: "owner".into(),
    })
}

pub async fn reconcile(action: &ProofstormCellAction, context: &Context) -> Result<Action, Error> {
    let result = metadata(action, context).await;
    match result {
        Ok(transfer) => {
            patch_action_status(
                action,
                context,
                ProofstormCellActionStatus {
                    phase: ActionPhase::Succeeded,
                    observed_generation: action.metadata.generation,
                    completed_at_unix: Some(now_unix()),
                    artifact: Some(status_object(serde_json::json!({"transfer":transfer}))),
                    ..ProofstormCellActionStatus::default()
                },
            )
            .await?;
            Ok(Action::await_change())
        }
        Err(_) => {
            patch_action_failure(
                action,
                context,
                "private_transfer_refused",
                "private transfer unavailable or admission refused; no native command started",
            )
            .await
        }
    }
}
async fn metadata(action: &ProofstormCellAction, context: &Context) -> Result<Transfer, Error> {
    use proofstorm_kube::TransferMethod;
    let CellAction::PrivateTransfer(request) = &action.spec.action else {
        return Err(failure());
    };
    let cell = live_cell(action, context).await?;
    let source = grant(action, &cell, &request.component)?;
    let mut vault = vault(&cell)?;
    private(vault.expire())?;
    if request.transfer_method == TransferMethod::Handoff {
        if request.destination_component.is_some()
            || request.maximum_bytes.is_some()
            || action.spec.access_scope.is_some()
        {
            return Err(failure());
        }
        let id = request.reference.as_deref().ok_or_else(failure)?;
        let recipient = recipient_access(
            &cell,
            request.recipient_grant_id.as_deref().ok_or_else(failure)?,
        )?;
        let scope = &recipient.scope;
        if scope.reference != id || scope.issuer_principal_id != source.principal {
            return Err(failure());
        }
        let destination = recipient_grant(&recipient, &cell, &scope.component);
        return private(vault.handoff(&source, &destination, id));
    }
    if request.recipient_grant_id.is_some() {
        return Err(failure());
    }
    if request.transfer_method == TransferMethod::Prepare {
        if request.reference.is_some() {
            return Err(failure());
        }
        let destination = grant(
            action,
            &cell,
            request
                .destination_component
                .as_deref()
                .ok_or_else(failure)?,
        )?;
        let maximum = request.maximum_bytes.ok_or_else(failure)?;
        if cell
            .spec
            .cell
            .components
            .iter()
            .any(|c| c.id == destination.wallet && c.implementation == "cdk-cli-wallet")
            && maximum > proofstorm_core::private_io::MAX_PRIVATE_ARG_BYTES
        {
            return Err(failure());
        }
        return private(vault.prepare(
            &source,
            &destination,
            &action.spec.operation_id,
            request.maximum_bytes.ok_or_else(failure)?,
        ));
    }
    if request.destination_component.is_some() || request.maximum_bytes.is_some() {
        return Err(failure());
    }
    let id = request.reference.as_deref().ok_or_else(failure)?;
    match request.transfer_method {
        TransferMethod::Status => private(vault.status(&source, id)),
        TransferMethod::Deliver => private(vault.deliver(&source, id)),
        TransferMethod::Release => private(vault.release(&source, id)),
        TransferMethod::Prepare | TransferMethod::Handoff => Err(failure()),
    }
}

pub async fn configure(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Option<PrivateIo>, Error> {
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    let cell = live_cell(action, context).await?;
    let authority = grant(action, &cell, &request.component)?;
    let mut vault = vault(&cell)?;
    private(vault.expire())?;
    let t = private(vault.status(&authority, binding.reference()))?;
    let io = match binding {
        PayloadBinding::Capture { format, .. }
            if t.source_wallet == request.component && t.source.operation_id.is_none() =>
        {
            PrivateIo::Capture {
                maximum_bytes: t.maximum_bytes,
                format: *format,
            }
        }
        PayloadBinding::Consume { input, .. }
            if t.destination_wallet == request.component
                && t.delivered
                && t.receiver.operation_id.is_none() =>
        {
            PrivateIo::Consume {
                bytes: t.bytes.ok_or_else(failure)?,
                sha256: t.sha256.ok_or_else(failure)?,
                input: input.clone(),
            }
        }
        _ => return Err(failure()),
    };
    Ok(Some(io))
}

/// Called only after the existing global native-execution handle fence commits.
pub async fn start(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Option<Vec<u8>>, Error> {
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    let cell = live_cell(action, context).await?;
    let authority = grant(action, &cell, &request.component)?;
    let mut vault = vault(&cell)?;
    let id = binding.reference();
    match binding {
        PayloadBinding::Capture { .. } => {
            private(vault.begin_capture(&authority, id, &action.spec.operation_id))?;
            Ok(None)
        }
        PayloadBinding::Consume { .. } => {
            private(vault.begin_receive(&authority, id, &action.spec.operation_id))?;
            let mut bytes = Vec::new();
            private(vault.consume(&authority, id, &action.spec.operation_id, &mut bytes))?;
            Ok(Some(bytes))
        }
    }
}

pub async fn complete(
    action: &ProofstormCellAction,
    context: &Context,
    receipt: &serde_json::Value,
) -> Result<Option<Transfer>, Error> {
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    // Completion may attach to an accepted operation after the session was released.
    let cells = Api::<ProofstormCell>::namespaced(
        context.client.clone(),
        &action.namespace().ok_or_else(failure)?,
    );
    let cell = cells.get(&action.spec.cell_name).await?;
    if cell.spec.instance_key != action.spec.instance_key
        || cell.spec.workspace_id != action.spec.workspace_id
    {
        return Err(failure());
    }
    let mut vault = vault(&cell)?;
    let native: NativeReceipt = serde_json::from_value(receipt.clone()).map_err(|_| failure())?;
    let id = binding.reference();
    let result = match binding {
        PayloadBinding::Consume { .. } => {
            private(vault.finish_receive(id, &action.spec.operation_id, native))?
        }
        PayloadBinding::Capture { .. } => {
            let current = private(vault.finish_source(id, &action.spec.operation_id, native))?;
            if current.capture != proofstorm_transfer::CapturePhase::Started {
                return Ok(Some(current));
            }
            let manifest: Option<PayloadManifest> = receipt
                .get("payload_manifest")
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            if manifest.is_none() || cell.metadata.deletion_timestamp.is_some() {
                return private(vault.interrupt(id)).map(Some);
            }
            let Ok(authority) = grant(action, &cell, &request.component) else {
                return private(vault.interrupt(id)).map(Some);
            };
            let reference = action
                .status
                .as_ref()
                .and_then(|s| s.native_execution.as_ref())
                .ok_or_else(failure)?;
            let pods = Api::<Pod>::namespaced(
                context.client.clone(),
                &instance_namespace(&action.spec.instance_key),
            );
            let bytes = super::native_exec::private_payload(&pods, reference).await?;
            private(vault.capture(
                &authority,
                id,
                &action.spec.operation_id,
                &mut Cursor::new(bytes),
                ProducedPayload { native, manifest },
            ))?
        }
    };
    Ok(Some(result))
}

pub fn close(cell: &ProofstormCell) -> Result<(), Error> {
    let directory = path(cell)?;
    if directory.exists() {
        let receipt = private(vault(cell)?.close())?;
        if !receipt.storage_cleanup_verified {
            return Err(failure());
        }
    }
    Ok(())
}
pub fn remove_closed(cell: &ProofstormCell) -> Result<(), Error> {
    close(cell)?;
    let directory = path(cell)?;
    if directory.exists() {
        std::fs::remove_dir_all(directory).map_err(|_| failure())?;
    }
    Ok(())
}

pub fn expire(cell: &ProofstormCell) -> Result<(), Error> {
    if path(cell)?.exists() {
        private(vault(cell)?.expire())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_kube::{
        PrivateTransferAction, ProofstormCellActionSpec, ProofstormCellSpec, TransferMethod,
    };
    fn fixture() -> (ProofstormCell, ProofstormCellAction) {
        let cell=ProofstormCell::new("cell",ProofstormCellSpec {
            workspace_id:"workspace".into(),instance_id:"instance".into(),instance_key:"instance-key".into(),revision_digest:"revision".into(),
            lock:proofstorm_core::ResolvedLock {api_version:"proofstorm/v1alpha1".into(),digest:"lock".into(),entries:vec![]},
            cell:serde_json::from_value(serde_json::json!({"api_version":"proofstorm/v1alpha1","name":"cell","components":[{"id":"wallet","kind":"wallet","implementation":"cocod-wallet","config_version":"test","control":"cell","config":{}}],"links":[]})).unwrap(),
        });
        let action = ProofstormCellAction::new(
            "action",
            ProofstormCellActionSpec {
                access_scope: None,
                cell_name: "cell".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "instance-key".into(),
                experiment_id: "experiment".into(),
                session_id: "session".into(),
                principal_id: "owner".into(),
                sequence: 1,
                operation_id: "operation".into(),
                request_digest: "request".into(),
                capability: Capability::ComponentExecLive,
                accepted_at_unix: now_unix(),
                action: CellAction::PrivateTransfer(PrivateTransferAction {
                    recipient_grant_id: None,
                    transfer_method: TransferMethod::Status,
                    component: "wallet".into(),
                    destination_component: None,
                    reference: Some("opaque".into()),
                    maximum_bytes: None,
                }),
            },
        );
        (cell, action)
    }
    #[test]
    fn ordinary_private_work_has_no_session_state_or_single_owner_annotation() {
        let (cell, mut action) = fixture();
        let first = grant(&action, &cell, "wallet").unwrap();
        action.spec.session_id = "another-session".into();
        let later = grant(&action, &cell, "wallet").unwrap();
        assert_eq!(first.authority, later.authority);
        assert!(grant(&action, &cell, "unknown-wallet").is_err());
    }
    #[test]
    fn private_access_is_bound_and_revocable_independently_of_sessions() {
        let (mut cell, mut action) = fixture();
        let access = PrivateAccessGrant {
            id: "receive-one".into(),
            workspace_id: "workspace".into(),
            instance_id: "instance".into(),
            principal_id: "receiver".into(),
            scope: proofstorm_core::PrivateTransferScope {
                issuer_principal_id: "owner".into(),
                receive_command_digest: format!("sha256:{}", "a".repeat(64)),
                component: "wallet".into(),
                mint: "mint".into(),
                reference: "opaque".into(),
            },
            created_at_unix: 0,
            revoked_at_unix: None,
        };
        cell.metadata.annotations = Some(std::collections::BTreeMap::from([(
            PRIVATE_ACCESS_ANNOTATION.into(),
            serde_json::json!({access.id.clone():access}).to_string(),
        )]));
        action.spec.principal_id = "receiver".into();
        action.spec.access_scope = Some(access.clone());
        assert!(validate_delegated_action(&action, &cell).is_ok());
        action.spec.session_id = "new-session".into();
        assert!(validate_delegated_action(&action, &cell).is_ok());
        let mut revoked = access.clone();
        revoked.revoked_at_unix = Some(1);
        cell.metadata.annotations.as_mut().unwrap().insert(
            PRIVATE_ACCESS_ANNOTATION.into(),
            serde_json::json!({access.id:revoked}).to_string(),
        );
        assert!(validate_delegated_action(&action, &cell).is_err());
    }
}
