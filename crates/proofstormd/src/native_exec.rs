//! Durable supervisor handles. Reconciliation polls receipts; it never replays starts.
use super::{
    ACTION_CANCEL_ANNOTATION, Action, ActionPhase, Api, AttachParams, COMPONENT_LABEL, Context,
    Duration, Error, INSTANCE_LABEL, LabAction, ListParams, Pod, ProofstormLab,
    ProofstormLabAction, ProofstormLabActionStatus, ResourceExt, compile_component_plans,
    exec_exit_code, instance_namespace, now_unix, patch_action_failure, patch_action_status,
    patch_invalid_action, read_bounded_output, status_object,
};
use kube::api::{Patch, PatchParams};
use proofstorm_core::native::{NativeCommand, cap_public_streams};
use proofstorm_kube::NativeExecutionRef;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn exec_bounded(
    pods: &Api<Pod>,
    pod: &str,
    container: &str,
    args: Vec<String>,
    input: Option<&[u8]>,
    maximum: usize,
) -> Result<Vec<u8>, Error> {
    let run = async {
        let attach = AttachParams::default()
            .container(container.to_owned())
            .stdin(input.is_some())
            .stdout(true)
            .stderr(true);
        let mut process = pods.exec(pod, args, &attach).await?;
        let stdout = process
            .stdout()
            .ok_or_else(|| Error::LiveExec("stdout unavailable".into()))?;
        let stderr = process
            .stderr()
            .ok_or_else(|| Error::LiveExec("stderr unavailable".into()))?;
        let status = process
            .take_status()
            .ok_or_else(|| Error::LiveExec("status unavailable".into()))?;
        let stdin = process.stdin();
        let upload = async {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin
                    .write_all(input)
                    .await
                    .map_err(|_| Error::LiveExec("native request transport failed".into()))?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|_| Error::LiveExec("native request transport failed".into()))?;
            }
            Ok::<_, Error>(())
        };
        let (uploaded, out, err, status) = tokio::join!(
            upload,
            async {
                let mut out = Vec::new();
                stdout
                    .take(maximum as u64 + 1)
                    .read_to_end(&mut out)
                    .await
                    .map_err(|_| Error::LiveExec("private transport interrupted".into()))?;
                let truncated = out.len() > maximum;
                Ok::<_, Error>((out, truncated))
            },
            read_bounded_output(stderr),
            status
        );
        uploaded?;
        let (out, truncated) = out?;
        let _ = err?; // Never echo helper stderr into ordinary artifacts.
        process
            .join()
            .await
            .map_err(|_| Error::LiveExec("native transport interrupted".into()))?;
        if exec_exit_code(status.as_ref()) != 0 || truncated {
            return Err(Error::LiveExec("native supervisor transport failed".into()));
        }
        Ok(out)
    };
    tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .map_err(|_| Error::LiveExec("native supervisor transport timed out".into()))?
}

async fn exec(
    pods: &Api<Pod>,
    pod: &str,
    container: &str,
    args: Vec<String>,
    input: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    exec_bounded(pods, pod, container, args, input, 65536).await
}

pub(super) async fn private_payload(
    pods: &Api<Pod>,
    reference: &NativeExecutionRef,
) -> Result<Vec<u8>, Error> {
    let pod = pods.get(&reference.pod).await?;
    if pod.metadata.uid.as_deref() != Some(&reference.pod_uid) {
        return Err(Error::LiveExec("private source pod changed".into()));
    }
    exec_bounded(
        pods,
        &reference.pod,
        &reference.container,
        helper_args(reference, "payload"),
        None,
        proofstorm_core::private_io::MAX_PRIVATE_BYTES as usize,
    )
    .await
}

fn cached_native_receipt(
    artifact: Option<&std::collections::BTreeMap<String, Value>>,
) -> Option<Value> {
    let mut receipt = serde_json::to_value(artifact?).ok()?;
    if receipt.get("supervisor_version") != Some(&json!("proofstorm-exec/v1")) {
        return None;
    }
    // Retain supervisor facts, retry only custody. Previous attempt diagnostics
    // must not make a subsequently successful collection permanently pending.
    for key in ["transfer_error", "private_files_retired", "transfer"] {
        receipt.as_object_mut()?.remove(key);
    }
    Some(receipt)
}

fn helper_args(reference: &NativeExecutionRef, mode: &str) -> Vec<String> {
    vec![
        format!("{}/runner", reference.directory),
        mode.into(),
        reference.directory.clone(),
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "installation and replay fence must precede the single native start"
)]
pub async fn reconcile(
    action: &ProofstormLabAction,
    lab: &ProofstormLab,
    context: &Context,
) -> Result<Action, Error> {
    let LabAction::ComponentExecLive(request) = &action.spec.action else {
        return Err(Error::ControllerInvariant("native action expected"));
    };
    if action.spec.capability != proofstorm_core::Capability::ComponentExecLive {
        return patch_invalid_action(action, context, "native execution capability required").await;
    }
    let mut command = NativeCommand {
        private_io: None,
        script: request.script.clone(),
        argv: request.argv.clone(),
        timeout_seconds: request.timeout_seconds,
        output: request.output.clone(),
    };
    if let Err(message) = command.validate() {
        return patch_invalid_action(action, context, message).await;
    }
    let plans = compile_component_plans(
        &lab.spec.instance_key,
        &lab.spec.revision_digest,
        &lab.spec.lab,
        &lab.spec.lock,
    )?;
    let Some(component) = plans
        .iter()
        .find(|plan| plan.component_id == request.component)
    else {
        return patch_invalid_action(action, context, "component missing from immutable lab").await;
    };
    let pods = Api::<Pod>::namespaced(
        context.client.clone(),
        &instance_namespace(&action.spec.instance_key),
    );
    let reference = action
        .status
        .as_ref()
        .and_then(|status| status.native_execution.as_ref());
    if let Some(reference) = reference {
        let cached = cached_native_receipt(
            action
                .status
                .as_ref()
                .and_then(|status| status.artifact.as_ref()),
        );
        let pod = pods.get_opt(&reference.pod).await?;
        let same_pod =
            pod.as_ref().and_then(|pod| pod.metadata.uid.as_ref()) == Some(&reference.pod_uid);
        if !same_pod && cached.is_none() {
            return patch_action_failure(action, context, "native_execution_lost", "execution pod changed; process and mutation outcome are unknown; command was not replayed").await;
        }
        if cached.is_none() && action.annotations().contains_key(ACTION_CANCEL_ANNOTATION) {
            let _ = exec(
                &pods,
                &reference.pod,
                &reference.container,
                helper_args(reference, "cancel"),
                None,
            )
            .await;
        }
        let reply = if let Some(cached) = cached {
            serde_json::to_vec(&cached)
                .map_err(|_| Error::LiveExec("native receipt serialization failed".into()))
        } else {
            exec(
                &pods,
                &reference.pod,
                &reference.container,
                helper_args(reference, "status"),
                None,
            )
            .await
        };
        if let Ok(bytes) = reply {
            if let Ok(mut receipt) = serde_json::from_slice::<Value>(&bytes) {
                if receipt.get("supervisor_version") == Some(&json!("proofstorm-exec/v1")) {
                    if request.private_payload.is_some() {
                        match super::private_transfer::complete(action, context, &receipt).await {
                            Ok(Some(transfer)) => {
                                receipt["transfer"] = json!(transfer);
                                if transfer.capture == proofstorm_transfer::CapturePhase::Ready
                                    || transfer.receiver.receipt.is_some()
                                {
                                    let retired = !same_pod
                                        || exec(
                                            &pods,
                                            &reference.pod,
                                            &reference.container,
                                            helper_args(reference, "retire"),
                                            None,
                                        )
                                        .await
                                        .is_ok();
                                    receipt["private_files_retired"] = json!(retired);
                                }
                            }
                            Ok(None) => (),
                            Err(_) => {
                                receipt["transfer_error"] = json!(
                                    "private_custody_incomplete; reconcile original operation without replay"
                                );
                            }
                        }
                    }
                    let custody_pending = receipt.get("transfer_error").is_some()
                        || receipt.get("private_files_retired") == Some(&json!(false));
                    let started = action
                        .status
                        .as_ref()
                        .and_then(|status| status.started_at_unix)
                        .unwrap_or(action.spec.accepted_at_unix);
                    if custody_pending
                        && now_unix() <= started + i64::from(request.timeout_seconds) + 60
                    {
                        patch_action_status(
                            action,
                            context,
                            ProofstormLabActionStatus {
                                phase: ActionPhase::Running,
                                native_execution: Some(reference.clone()),
                                started_at_unix: Some(started),
                                artifact: Some(status_object(cap_public_streams(receipt))),
                                ..ProofstormLabActionStatus::default()
                            },
                        )
                        .await?;
                        return Ok(Action::requeue(Duration::from_secs(1)));
                    }
                    receipt = cap_public_streams(receipt);
                    receipt["component"] = json!(request.component);
                    receipt["kind"] = json!(component.kind);
                    receipt["pod"] = json!(reference.pod);
                    receipt["container"] = json!(reference.container);
                    receipt["execution_context"] = json!("live_component");
                    receipt["runner_digest"] = json!(reference.runner_digest);
                    receipt["private_output_ref"] = json!(action.spec.operation_id);
                    let clean = receipt.get("cleanup_verified") == Some(&json!(true));
                    let cancelled = receipt.get("cancelled") == Some(&json!(true));
                    patch_action_status(
                        action,
                        context,
                        ProofstormLabActionStatus {
                            phase: if !clean || custody_pending {
                                ActionPhase::Failed
                            } else if cancelled {
                                ActionPhase::Cancelled
                            } else {
                                ActionPhase::Succeeded
                            },
                            observed_generation: action.metadata.generation,
                            native_execution: Some(reference.clone()),
                            started_at_unix: action
                                .status
                                .as_ref()
                                .and_then(|status| status.started_at_unix),
                            completed_at_unix: Some(now_unix()),
                            artifact: Some(status_object(receipt)),
                            error: (!clean).then(|| {
                                status_object(json!({"code":"native_cleanup_unverified"}))
                            }),
                            ..ProofstormLabActionStatus::default()
                        },
                    )
                    .await?;
                    return Ok(Action::await_change());
                }
                if receipt.get("runner_error").is_some() {
                    return patch_action_failure(action, context, "native_runner_failed", "supervisor did not establish verified process cleanup; inspect the lab before retrying").await;
                }
            }
        }
        let started = action
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix)
            .unwrap_or(action.spec.accepted_at_unix);
        if now_unix() > started + i64::from(request.timeout_seconds) + 30 {
            return patch_action_failure(action, context, "native_receipt_unavailable", "no terminal supervisor receipt; process and mutation outcome are unknown; command was not replayed").await;
        }
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    // Legacy in-flight raw execs must retain their non-replay fence on upgrade.
    if action
        .status
        .as_ref()
        .and_then(|status| status.job_name.as_deref())
        == Some("live-exec-started")
    {
        return patch_action_failure(
            action,
            context,
            "live_exec_interrupted",
            "legacy execution has no supervisor handle; command was not replayed",
        )
        .await;
    }
    let selector = format!(
        "{COMPONENT_LABEL}={},{INSTANCE_LABEL}={}",
        request.component, action.spec.instance_key
    );
    let pod = pods
        .list(&ListParams::default().labels(&selector))
        .await?
        .items
        .into_iter()
        .filter(|pod| {
            pod.metadata.deletion_timestamp.is_none()
                && pod
                    .status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref())
                    == Some("Running")
        })
        .max_by_key(|pod| pod.metadata.creation_timestamp.clone());
    let Some(pod) = pod else {
        return patch_action_failure(
            action,
            context,
            "component_pod_not_ready",
            "no running component; command not started",
        )
        .await;
    };
    let container = pod
        .spec
        .as_ref()
        .and_then(|spec| spec.containers.first())
        .ok_or(Error::ControllerInvariant("container missing"))?
        .name
        .clone();
    match super::private_transfer::configure(action, context).await {
        Ok(binding) => command.private_io = binding,
        Err(_) => {
            return patch_action_failure(
                action,
                context,
                "private_transfer_refused",
                "private payload binding or private access permission refused; command not started",
            )
            .await;
        }
    }
    if let Err(message) = command.validate() {
        return patch_invalid_action(action, context, message).await;
    }
    let binary = std::fs::read(
        std::env::var("PROOFSTORM_NATIVE_RUNNER")
            .unwrap_or_else(|_| "/usr/local/lib/proofstorm-exec".into()),
    )
    .map_err(|_| Error::LiveExec("native supervisor binary unavailable".into()))?;
    let install = "set -eu; umask 077; dir=$(mktemp -d /tmp/proofstorm-exec.XXXXXXXX); cat > \"$dir/runner\"; chmod 700 \"$dir/runner\"; printf '%s' \"$dir\"";
    let directory = exec(
        &pods,
        &pod.name_any(),
        &container,
        vec!["/bin/sh".into(), "-c".into(), install.into()],
        Some(&binary),
    )
    .await?;
    let directory = String::from_utf8(directory)
        .map_err(|_| Error::LiveExec("invalid supervisor directory".into()))?;
    if !directory.starts_with("/tmp/proofstorm-exec.")
        || !directory
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/.-".contains(&b))
    {
        return Err(Error::LiveExec("invalid supervisor directory".into()));
    }
    let reference = NativeExecutionRef {
        pod: pod.name_any(),
        pod_uid: pod
            .metadata
            .uid
            .ok_or(Error::ControllerInvariant("pod UID missing"))?,
        container,
        directory,
        runner_digest: format!("sha256:{:x}", Sha256::digest(&binary)),
    };
    let version = action.resource_version().ok_or(Error::ControllerInvariant(
        "native start requires an observed resource version",
    ))?;
    let namespace = action.namespace().ok_or(Error::ControllerInvariant(
        "native action namespace missing",
    ))?;
    let actions = Api::<ProofstormLabAction>::namespaced(context.client.clone(), &namespace);
    // A conditional status write is the global start fence. Overlapping
    // controllers or a concurrent cancellation invalidate a stale start claim.
    // No command is launched unless this exact observed version wins the claim.
    actions
        .patch_status(
            &action.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata":{"resourceVersion":version},
                "status":ProofstormLabActionStatus {
                    phase: ActionPhase::Running,
                    observed_generation: action.metadata.generation,
                    native_execution: Some(reference.clone()),
                    job_name: Some("live-exec-started".into()),
                    started_at_unix: Some(now_unix()),
                    ..ProofstormLabActionStatus::default()
                }
            })),
        )
        .await?;
    let Ok(private_input) = super::private_transfer::start(action, context).await else {
        return patch_action_failure(
            action,
            context,
            "private_transfer_refused",
            "private custody admission refused; native command not started",
        )
        .await;
    };
    if let Some(input) = private_input {
        let Some(proofstorm_core::private_io::PrivateIo::Consume { bytes, sha256, .. }) =
            &command.private_io
        else {
            return Err(Error::ControllerInvariant("private input contract missing"));
        };
        let mut args = helper_args(&reference, "input");
        args.extend([bytes.to_string(), sha256.clone()]);
        if exec(
            &pods,
            &reference.pod,
            &reference.container,
            args,
            Some(&input),
        )
        .await
        .is_err()
        {
            return patch_action_failure(action,context,"private_input_interrupted","private input transport interrupted; input was not replayed and native command was not started").await;
        }
    }
    // An uncertain start is polled through the persisted handle, never retried.
    let input = serde_json::to_vec(&command)
        .map_err(|_| Error::LiveExec("invalid native request".into()))?;
    let _ = exec(
        &pods,
        &reference.pod,
        &reference.container,
        helper_args(&reference, "start"),
        Some(&input),
    )
    .await;
    Ok(Action::requeue(Duration::from_secs(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_native_completion_survives_failed_collection_and_status_loss() {
        let first = json!({"supervisor_version":"proofstorm-exec/v1","exit_code":0,"exit_signal":null,"cleanup_verified":true,"payload_manifest":{"bytes":3,"sha256":"source-hash"},"transfer_error":"temporary collection failure","private_files_retired":false});
        let saved = status_object(first);
        let retried = cached_native_receipt(Some(&saved)).unwrap();
        assert_eq!(retried["exit_code"], 0);
        assert_eq!(retried["payload_manifest"]["sha256"], "source-hash");
        assert!(retried.get("transfer_error").is_none());
        assert!(retried.get("private_files_retired").is_none());
        assert!(cached_native_receipt(Some(&status_object(json!({"running":true})))).is_none());
    }
}
