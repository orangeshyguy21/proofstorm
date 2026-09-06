//! Shared controller custody. No payload body is ever serialized into an action.
use super::{
    Action, ActionPhase, Api, Context, Error, LabAction, Pod, ProofstormLab, ProofstormLabAction,
    ProofstormLabActionStatus, ResourceExt, instance_namespace, now_unix, patch_action_failure,
    patch_action_status, status_object,
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

fn path(lab: &ProofstormLab) -> Result<PathBuf, Error> {
    if lab.spec.instance_key.is_empty()
        || !lab
            .spec
            .instance_key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || lab.spec.instance_key == "."
        || lab.spec.instance_key == ".."
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
    Ok(root.join(&lab.spec.instance_key))
}
fn vault(lab: &ProofstormLab) -> Result<Vault, Error> {
    private(Vault::open(
        &path(lab)?,
        &lab.spec.workspace_id,
        &lab.spec.instance_key,
        Limits::default(),
    ))
}
async fn live_lab(action: &ProofstormLabAction, context: &Context) -> Result<ProofstormLab, Error> {
    let labs = Api::<ProofstormLab>::namespaced(
        context.client.clone(),
        &action.namespace().ok_or_else(failure)?,
    );
    let lab = labs.get(&action.spec.lab_name).await?;
    if lab.spec.workspace_id != action.spec.workspace_id
        || lab.spec.instance_key != action.spec.instance_key
        || lab.spec.instance_id != action.spec.instance_id
        || lab.metadata.deletion_timestamp.is_some()
    {
        return Err(failure());
    }
    Ok(lab)
}
fn recipient_access(lab: &ProofstormLab, id: &str) -> Result<PrivateAccessGrant, Error> {
    let grants: std::collections::BTreeMap<String, PrivateAccessGrant> = serde_json::from_str(
        lab.annotations()
            .get(PRIVATE_ACCESS_ANNOTATION)
            .ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    let grant = grants.get(id).ok_or_else(failure)?;
    if grant.id != id
        || grant.workspace_id != lab.spec.workspace_id
        || grant.instance_id != lab.spec.instance_id
        || grant.revoked_at_unix.is_some()
    {
        return Err(failure());
    }
    Ok(grant.clone())
}

/// Validate every new delegated action, including typed observations, before dispatch.
pub fn validate_delegated_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<(), Error> {
    let Some(snapshot) = &action.spec.access_scope else {
        return Ok(());
    };
    let current = recipient_access(lab, &snapshot.id)?;
    if &current != snapshot
        || current.principal_id != action.spec.principal_id
        || current.instance_id != action.spec.instance_id
        || current.workspace_id != action.spec.workspace_id
    {
        return Err(failure());
    }
    let scope = &current.scope;
    let (kind, request) = match &action.spec.action {
        LabAction::WalletBalance(r) if action.spec.capability == Capability::WalletControl => (
            OperationKind::WalletBalance,
            serde_json::json!({"wallet":r.wallet,"mint":r.mint}),
        ),
        LabAction::PrivateTransfer(r)
            if action.spec.capability == Capability::ComponentExecLive =>
        {
            (
                OperationKind::PrivateTransfer,
                serde_json::json!({"transfer":r}),
            )
        }
        LabAction::ComponentExecLive(r)
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

fn recipient_grant(access: &PrivateAccessGrant, lab: &ProofstormLab, wallet: &str) -> Grant {
    Grant {
        workspace: access.workspace_id.clone(),
        lab: lab.spec.instance_key.clone(),
        principal: access.principal_id.clone(),
        wallet: wallet.into(),
        authority: access.id.clone(),
    }
}

fn grant(action: &ProofstormLabAction, lab: &ProofstormLab, wallet: &str) -> Result<Grant, Error> {
    if action.spec.capability != Capability::ComponentExecLive
        || !lab
            .spec
            .lab
            .components
            .iter()
            .any(|c| c.id == wallet && c.kind == ComponentKind::Wallet)
    {
        return Err(failure());
    }
    validate_delegated_action(action, lab)?;
    if let Some(access) = &action.spec.access_scope {
        return Ok(recipient_grant(access, lab, wallet));
    }
    Ok(Grant {
        workspace: action.spec.workspace_id.clone(),
        lab: lab.spec.instance_key.clone(),
        principal: action.spec.principal_id.clone(),
        wallet: wallet.into(),
        authority: "owner".into(),
    })
}

pub async fn reconcile(action: &ProofstormLabAction, context: &Context) -> Result<Action, Error> {
    let result = metadata(action, context).await;
    match result {
        Ok(transfer) => {
            patch_action_status(
                action,
                context,
                ProofstormLabActionStatus {
                    phase: ActionPhase::Succeeded,
                    observed_generation: action.metadata.generation,
                    completed_at_unix: Some(now_unix()),
                    artifact: Some(status_object(serde_json::json!({"transfer":transfer}))),
                    ..ProofstormLabActionStatus::default()
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
async fn metadata(action: &ProofstormLabAction, context: &Context) -> Result<Transfer, Error> {
    use proofstorm_kube::TransferMethod;
    let LabAction::PrivateTransfer(request) = &action.spec.action else {
        return Err(failure());
    };
    let lab = live_lab(action, context).await?;
    let source = grant(action, &lab, &request.component)?;
    let mut vault = vault(&lab)?;
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
            &lab,
            request.recipient_grant_id.as_deref().ok_or_else(failure)?,
        )?;
        let scope = &recipient.scope;
        if scope.reference != id || scope.issuer_principal_id != source.principal {
            return Err(failure());
        }
        let destination = recipient_grant(&recipient, &lab, &scope.component);
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
            &lab,
            request
                .destination_component
                .as_deref()
                .ok_or_else(failure)?,
        )?;
        let maximum = request.maximum_bytes.ok_or_else(failure)?;
        if lab
            .spec
            .lab
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
    action: &ProofstormLabAction,
    context: &Context,
) -> Result<Option<PrivateIo>, Error> {
    let LabAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    let lab = live_lab(action, context).await?;
    let authority = grant(action, &lab, &request.component)?;
    let mut vault = vault(&lab)?;
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
    action: &ProofstormLabAction,
    context: &Context,
) -> Result<Option<Vec<u8>>, Error> {
    let LabAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    let lab = live_lab(action, context).await?;
    let authority = grant(action, &lab, &request.component)?;
    let mut vault = vault(&lab)?;
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
    action: &ProofstormLabAction,
    context: &Context,
    receipt: &serde_json::Value,
) -> Result<Option<Transfer>, Error> {
    let LabAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(failure());
    };
    let Some(binding) = &request.private_payload else {
        return Ok(None);
    };
    // Completion may attach to an accepted operation after the session was released.
    let labs = Api::<ProofstormLab>::namespaced(
        context.client.clone(),
        &action.namespace().ok_or_else(failure)?,
    );
    let lab = labs.get(&action.spec.lab_name).await?;
    if lab.spec.instance_key != action.spec.instance_key
        || lab.spec.workspace_id != action.spec.workspace_id
    {
        return Err(failure());
    }
    let mut vault = vault(&lab)?;
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
            if manifest.is_none() || lab.metadata.deletion_timestamp.is_some() {
                return private(vault.interrupt(id)).map(Some);
            }
            let Ok(authority) = grant(action, &lab, &request.component) else {
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

pub fn close(lab: &ProofstormLab) -> Result<(), Error> {
    let directory = path(lab)?;
    if directory.exists() {
        let receipt = private(vault(lab)?.close())?;
        if !receipt.storage_cleanup_verified {
            return Err(failure());
        }
    }
    Ok(())
}
pub fn remove_closed(lab: &ProofstormLab) -> Result<(), Error> {
    close(lab)?;
    let directory = path(lab)?;
    if directory.exists() {
        std::fs::remove_dir_all(directory).map_err(|_| failure())?;
    }
    Ok(())
}

pub fn expire(lab: &ProofstormLab) -> Result<(), Error> {
    if path(lab)?.exists() {
        private(vault(lab)?.expire())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_kube::{
        PrivateTransferAction, ProofstormLabActionSpec, ProofstormLabSpec, TransferMethod,
    };
    fn fixture() -> (ProofstormLab, ProofstormLabAction) {
        let lab=ProofstormLab::new("lab",ProofstormLabSpec {
            workspace_id:"workspace".into(),instance_id:"instance".into(),instance_key:"instance-key".into(),revision_digest:"revision".into(),
            lock:proofstorm_core::ResolvedLock {api_version:"proofstorm/v1alpha1".into(),digest:"lock".into(),entries:vec![]},
            lab:serde_json::from_value(serde_json::json!({"api_version":"proofstorm/v1alpha1","name":"lab","components":[{"id":"wallet","kind":"wallet","implementation":"cocod-wallet","config_version":"test","control":"laboratory","config":{}}],"links":[]})).unwrap(),
        });
        let action = ProofstormLabAction::new(
            "action",
            ProofstormLabActionSpec {
                access_scope: None,
                lab_name: "lab".into(),
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
                action: LabAction::PrivateTransfer(PrivateTransferAction {
                    recipient_grant_id: None,
                    transfer_method: TransferMethod::Status,
                    component: "wallet".into(),
                    destination_component: None,
                    reference: Some("opaque".into()),
                    maximum_bytes: None,
                }),
            },
        );
        (lab, action)
    }
    #[test]
    fn ordinary_private_work_has_no_session_state_or_single_owner_annotation() {
        let (lab, mut action) = fixture();
        let first = grant(&action, &lab, "wallet").unwrap();
        action.spec.session_id = "another-session".into();
        let later = grant(&action, &lab, "wallet").unwrap();
        assert_eq!(first.authority, later.authority);
        assert!(grant(&action, &lab, "unknown-wallet").is_err());
    }
    #[test]
    fn private_access_is_bound_and_revocable_independently_of_sessions() {
        let (mut lab, mut action) = fixture();
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
        lab.metadata.annotations = Some(std::collections::BTreeMap::from([(
            PRIVATE_ACCESS_ANNOTATION.into(),
            serde_json::json!({access.id.clone():access}).to_string(),
        )]));
        action.spec.principal_id = "receiver".into();
        action.spec.access_scope = Some(access.clone());
        assert!(validate_delegated_action(&action, &lab).is_ok());
        action.spec.session_id = "new-session".into();
        assert!(validate_delegated_action(&action, &lab).is_ok());
        let mut revoked = access.clone();
        revoked.revoked_at_unix = Some(1);
        lab.metadata.annotations.as_mut().unwrap().insert(
            PRIVATE_ACCESS_ANNOTATION.into(),
            serde_json::json!({access.id:revoked}).to_string(),
        );
        assert!(validate_delegated_action(&action, &lab).is_err());
    }
}
