//! Shared controller custody. No payload body is ever serialized into an action.
use super::{
    Action, ActionPhase, Api, Context, Error, LabAction, Pod, ProofstormLab, ProofstormLabAction,
    ProofstormLabActionStatus, ResourceExt, instance_namespace, now_unix, patch_action_failure,
    patch_action_status, status_object,
};
use proofstorm_core::private_io::{
    PRIVATE_DELEGATIONS_ANNOTATION, PRIVATE_LEASE_ANNOTATION, PayloadBinding, PrivateIo,
};
use proofstorm_core::{Capability, ComponentKind, ExperimentLease, LeasePhase, OperationKind};
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
fn root_lease(lab: &ProofstormLab) -> Result<ExperimentLease, Error> {
    let root: ExperimentLease = serde_json::from_str(
        lab.annotations()
            .get(PRIVATE_LEASE_ANNOTATION)
            .ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    if root.phase != LeasePhase::Active
        || root.delegation.is_some()
        || root.workspace_id != lab.spec.workspace_id
        || root.instance_id != lab.spec.instance_id
        || root.expires_at_unix <= now_unix()
    {
        return Err(failure());
    }
    Ok(root)
}

fn recipient_lease(lab: &ProofstormLab, id: &str) -> Result<ExperimentLease, Error> {
    let root = root_lease(lab)?;
    let children: std::collections::BTreeMap<String, ExperimentLease> = serde_json::from_str(
        lab.annotations()
            .get(PRIVATE_DELEGATIONS_ANNOTATION)
            .ok_or_else(failure)?,
    )
    .map_err(|_| failure())?;
    if children.len() > 32 {
        return Err(failure());
    }
    let child = children.get(id).ok_or_else(failure)?;
    let scope = child.delegation.as_ref().ok_or_else(failure)?;
    if child.id != id
        || child.principal_id == root.principal_id
        || child.workspace_id != root.workspace_id
        || child.instance_id != root.instance_id
        || child.experiment_id != root.experiment_id
        || scope.parent_lease_id != root.id
        || child.phase != LeasePhase::Active
        || child.expires_at_unix <= now_unix()
        || child.expires_at_unix > root.expires_at_unix
        || !lab
            .spec
            .lab
            .components
            .iter()
            .any(|c| c.id == scope.component && c.kind == ComponentKind::Wallet)
        || !lab
            .spec
            .lab
            .components
            .iter()
            .any(|c| c.id == scope.mint && c.kind == ComponentKind::Mint)
    {
        return Err(failure());
    }
    Ok(child.clone())
}

fn action_lease(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<ExperimentLease, Error> {
    let lease = if action.spec.lease_scope.is_some() {
        recipient_lease(lab, &action.spec.lease_id)?
    } else {
        root_lease(lab)?
    };
    if lease.instance_id != action.spec.instance_id
        || lease.id != action.spec.lease_id
        || lease.experiment_id != action.spec.experiment_id
        || lease.principal_id != action.spec.principal_id
        || lease.workspace_id != action.spec.workspace_id
        || lease.delegation != action.spec.lease_scope
    {
        return Err(failure());
    }
    Ok(lease)
}

/// Validate every new delegated action, including typed observations, before dispatch.
pub fn validate_delegated_action(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
) -> Result<(), Error> {
    action_lease(action, lab)?;
    let Some(scope) = &action.spec.lease_scope else {
        return Ok(());
    };
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

fn lease_grant(lease: &ExperimentLease, lab: &ProofstormLab, wallet: &str) -> Result<Grant, Error> {
    Ok(Grant {
        workspace: lease.workspace_id.clone(),
        lab: lab.spec.instance_key.clone(),
        principal: lease.principal_id.clone(),
        wallet: wallet.into(),
        lease: lease.id.clone(),
        expires_at_unix: u64::try_from(lease.expires_at_unix).map_err(|_| failure())?,
    })
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
    let lease = action_lease(action, lab)?;
    lease_grant(&lease, lab, wallet)
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
            || action.spec.lease_scope.is_some()
        {
            return Err(failure());
        }
        let id = request.reference.as_deref().ok_or_else(failure)?;
        let recipient = recipient_lease(
            &lab,
            request.recipient_lease_id.as_deref().ok_or_else(failure)?,
        )?;
        let scope = recipient.delegation.as_ref().ok_or_else(failure)?;
        if scope.reference != id || scope.parent_lease_id != source.lease {
            return Err(failure());
        }
        let destination = lease_grant(&recipient, &lab, &scope.component)?;
        return private(vault.handoff(&source, &destination, id));
    }
    if request.recipient_lease_id.is_some() {
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
    // Completion may attach to an accepted operation after the lease expired.
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
    fn root_fixture() -> (ProofstormLab, ProofstormLabAction, ExperimentLease) {
        let mut lab=ProofstormLab::new("lab",ProofstormLabSpec {
            workspace_id:"workspace".into(),instance_id:"instance".into(),instance_key:"instance-key".into(),revision_digest:"revision".into(),
            lock:proofstorm_core::ResolvedLock {api_version:"proofstorm/v1alpha1".into(),digest:"lock".into(),entries:vec![]},
            lab:serde_json::from_value(serde_json::json!({"api_version":"proofstorm/v1alpha1","name":"lab","components":[{"id":"wallet","kind":"wallet","implementation":"cocod-wallet","config_version":"test","control":"laboratory","config":{}}],"links":[]})).unwrap(),
        });
        let action = ProofstormLabAction::new(
            "action",
            ProofstormLabActionSpec {
                lease_scope: None,
                lab_name: "lab".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "instance-key".into(),
                experiment_id: "experiment".into(),
                lease_id: "lease".into(),
                principal_id: "owner".into(),
                sequence: 1,
                operation_id: "operation".into(),
                request_digest: "request".into(),
                capability: Capability::ComponentExecLive,
                accepted_at_unix: now_unix(),
                action: LabAction::PrivateTransfer(PrivateTransferAction {
                    recipient_lease_id: None,
                    transfer_method: TransferMethod::Status,
                    component: "wallet".into(),
                    destination_component: None,
                    reference: Some("opaque".into()),
                    maximum_bytes: None,
                }),
            },
        );
        let original = ExperimentLease {
            delegation: None,
            id: "lease".into(),
            workspace_id: "workspace".into(),
            experiment_id: "experiment".into(),
            instance_id: "instance".into(),
            principal_id: "owner".into(),
            phase: LeasePhase::Active,
            acquired_at_unix: now_unix(),
            expires_at_unix: now_unix() + 600,
            max_actions: 10,
            released_at_unix: None,
        };
        lab.metadata.annotations = Some(std::collections::BTreeMap::from([(
            PRIVATE_LEASE_ANNOTATION.into(),
            serde_json::to_string(&original).unwrap(),
        )]));
        (lab, action, original)
    }
    #[test]
    fn runtime_grants_require_the_current_exact_active_lease_and_wallet() {
        let (mut lab, mut action, original) = root_fixture();
        assert!(grant(&action, &lab, "wallet").is_ok());
        assert!(grant(&action, &lab, "other-wallet").is_err());
        for field in [
            "id",
            "workspace_id",
            "instance_id",
            "experiment_id",
            "principal_id",
            "phase",
            "expires_at_unix",
        ] {
            let mut value = serde_json::to_value(&original).unwrap();
            value[field] = match field {
                "phase" => serde_json::json!("released"),
                "expires_at_unix" => serde_json::json!(0),
                _ => serde_json::json!("other"),
            };
            lab.metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(PRIVATE_LEASE_ANNOTATION.into(), value.to_string());
            assert!(
                grant(&action, &lab, "wallet").is_err(),
                "accepted stale/mismatched {field}"
            );
        }
        lab.metadata.annotations = None;
        assert!(grant(&action, &lab, "wallet").is_err());
        lab.metadata.annotations = Some(std::collections::BTreeMap::from([(
            PRIVATE_LEASE_ANNOTATION.into(),
            serde_json::to_string(&original).unwrap(),
        )]));
        action.spec.capability = Capability::LabRead;
        assert!(grant(&action, &lab, "wallet").is_err());
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture proves admission, scope substitution, and fresh revocation boundaries"
    )]
    fn delegated_runtime_checks_fresh_identity_scope_and_parent_revocation() {
        use proofstorm_core::PrivateTransferLeaseScope;
        let (mut lab, mut action, root) = root_fixture();
        let mut mint = lab.spec.lab.components[0].clone();
        mint.id = "mint".into();
        mint.kind = ComponentKind::Mint;
        lab.spec.lab.components.push(mint);
        let scope = PrivateTransferLeaseScope {
            receive_command_digest: proofstorm_core::PrivateReceiveCommand {
                script: String::new(),
                argv: vec!["receive".into()],
                timeout_seconds: 30,
                input: proofstorm_core::private_io::InputBinding::Stdin,
            }
            .digest(),
            parent_lease_id: root.id.clone(),
            component: "wallet".into(),
            mint: "mint".into(),
            reference: "opaque".into(),
        };
        let child = ExperimentLease {
            delegation: Some(scope.clone()),
            id: "child".into(),
            principal_id: "recipient".into(),
            ..root.clone()
        };
        action.spec.lease_scope = Some(scope);
        action.spec.lease_id = child.id.clone();
        action.spec.principal_id = child.principal_id.clone();
        let annotate = |lab: &mut ProofstormLab, value: serde_json::Value| {
            lab.metadata.annotations.as_mut().unwrap().insert(
                PRIVATE_DELEGATIONS_ANNOTATION.into(),
                serde_json::json!({"child":value}).to_string(),
            );
        };
        annotate(&mut lab, serde_json::to_value(&child).unwrap());
        assert!(validate_delegated_action(&action, &lab).is_ok());
        assert!(grant(&action, &lab, "wallet").is_ok());
        let mut omitted = action.clone();
        omitted.spec.lease_scope = None;
        assert!(validate_delegated_action(&omitted, &lab).is_err());
        for field in [
            "id",
            "principal_id",
            "instance_id",
            "workspace_id",
            "experiment_id",
            "phase",
            "expires_at_unix",
        ] {
            let mut value = serde_json::to_value(&child).unwrap();
            value[field] = match field {
                "phase" => serde_json::json!("released"),
                "expires_at_unix" => serde_json::json!(0),
                _ => serde_json::json!("wrong"),
            };
            annotate(&mut lab, value);
            assert!(validate_delegated_action(&action, &lab).is_err(), "{field}");
        }
        annotate(&mut lab, serde_json::to_value(&child).unwrap());
        action.spec.action = LabAction::WalletBalance(proofstorm_kube::WalletBalanceAction {
            wallet: "wallet".into(),
            mint: "mint".into(),
        });
        action.spec.capability = Capability::WalletControl;
        assert!(validate_delegated_action(&action, &lab).is_ok());
        for (wallet, mint) in [("other", "mint"), ("wallet", "other")] {
            let mut wrong = action.clone();
            wrong.spec.action = LabAction::WalletBalance(proofstorm_kube::WalletBalanceAction {
                wallet: wallet.into(),
                mint: mint.into(),
            });
            assert!(validate_delegated_action(&wrong, &lab).is_err());
        }
        action.spec.capability = Capability::ComponentExecLive;
        action.spec.action =
            LabAction::ComponentExecLive(proofstorm_kube::ComponentExecLiveAction {
                component: "wallet".into(),
                script: String::new(),
                argv: vec!["receive".into()],
                timeout_seconds: 30,
                output: proofstorm_core::native::NativeOutput::default(),
                private_payload: Some(PayloadBinding::Consume {
                    reference: "opaque".into(),
                    input: proofstorm_core::private_io::InputBinding::Stdin,
                }),
            });
        assert!(validate_delegated_action(&action, &lab).is_ok());
        let mut substituted = action.clone();
        if let LabAction::ComponentExecLive(r) = &mut substituted.spec.action {
            r.argv = vec!["wallet-spend".into()];
        }
        assert!(validate_delegated_action(&substituted, &lab).is_err());
        let mut other = action.clone();
        if let LabAction::ComponentExecLive(r) = &mut other.spec.action {
            r.private_payload = None;
        }
        assert!(validate_delegated_action(&other, &lab).is_err());
        let annotations = lab.metadata.annotations.as_mut().unwrap();
        annotations.remove(PRIVATE_DELEGATIONS_ANNOTATION);
        assert!(validate_delegated_action(&action, &lab).is_err());
        annotate(&mut lab, serde_json::to_value(&child).unwrap());
        lab.metadata
            .annotations
            .as_mut()
            .unwrap()
            .remove(PRIVATE_LEASE_ANNOTATION);
        assert!(validate_delegated_action(&action, &lab).is_err());
    }
}
