//! Durable supervisor handles. Reconciliation polls receipts; it never replays starts.
use super::{
    ACTION_CANCEL_ANNOTATION, Action, ActionPhase, Api, AttachParams, COMPONENT_LABEL, CellAction,
    Context, Duration, Error, INSTANCE_LABEL, ListParams, Pod, ProofstormCell,
    ProofstormCellAction, ProofstormCellActionStatus, ResourceExt, compile_component_plans,
    exec_exit_code, instance_namespace, now_unix, patch_action_failure, patch_action_status,
    patch_invalid_action, read_bounded_output, status_object,
};
use kube::api::{Patch, PatchParams};
use proofstorm_core::native::{NativeCommand, cap_public_streams};
use proofstorm_kube::NativeExecutionRef;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

/// Native exec work this controller runs at once, in two separate pools so
/// neither can starve the other. A start uploads the runner and, under load,
/// can take the whole transport timeout; so can a receipt poll. Without caps,
/// starts took every action-controller slot and starved status polls, cancels
/// and all other actions (PS-44). Together the pools leave controller slots for
/// everything else. A cancel of a running command bypasses both.
///
/// Each component may hold only some of the start places, so a component whose
/// execs hang (frozen, overloaded) leaves starts into every other component
/// going. The sizes come from `runtime.toml`; `ControllerLimits::validate`
/// keeps these rules.
pub(super) struct Gate {
    polls: Semaphore,
    starts: Semaphore,
    starts_per_component: usize,
    /// Starts in progress per `instance_key/component`.
    components: Mutex<HashMap<String, usize>>,
}

impl Gate {
    pub(super) fn new(limits: &proofstorm_core::ControllerLimits) -> Self {
        Self {
            polls: Semaphore::new(usize::from(limits.native_polls)),
            starts: Semaphore::new(usize::from(limits.native_starts)),
            starts_per_component: usize::from(limits.native_starts_per_component),
            components: Mutex::default(),
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new(&proofstorm_core::ControllerLimits::default())
    }
}

impl Gate {
    /// Reserve one of a component's start places, or `None` if it has none free.
    fn start_component(&self, key: &str) -> Option<ComponentStart<'_>> {
        let mut active = self.components.lock().expect("native start gate lock");
        let count = active.entry(key.to_owned()).or_default();
        if *count >= self.starts_per_component {
            return None;
        }
        *count += 1;
        Some(ComponentStart {
            gate: self,
            key: key.to_owned(),
        })
    }
}

/// A component start place, released when dropped.
struct ComponentStart<'a> {
    gate: &'a Gate,
    key: String,
}

impl Drop for ComponentStart<'_> {
    fn drop(&mut self) {
        let mut active = self.gate.components.lock().expect("native start gate lock");
        if let Some(count) = active.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.key);
            }
        }
    }
}

/// Try again soon, spread by operation so waiting actions don't retry in step.
fn gate_busy(action: &ProofstormCellAction) -> Action {
    super::jittered_requeue(&action.spec.operation_id, 1, 2)
}

/// Delay before the next receipt poll: every second at first, so short
/// commands finish promptly, then less often while a long command runs.
fn poll_interval(elapsed_seconds: i64) -> Duration {
    Duration::from_secs(if elapsed_seconds < 10 {
        1
    } else if elapsed_seconds < 60 {
        2
    } else {
        5
    })
}

/// Upload the runner into a fresh private directory under `base` and print the
/// directory. An incomplete upload (the transport timed out mid-stream) or a
/// hangup removes the directory instead of leaving a partial runner behind.
///
/// A complete upload also replaces this pod's cached runner (removing caches of
/// other versions), so later starts copy it instead of uploading. Caching is
/// skipped where no SHA-256 tool can check the cache before use.
fn install_script(base: &str, bytes: usize, digest: &str) -> String {
    format!(
        "set -eu; umask 077; dir=$(mktemp -d {base}/proofstorm-exec.XXXXXXXX); \
         trap 'rm -rf -- \"$dir\"; exit 70' HUP PIPE TERM; \
         if cat > \"$dir/runner\" && size=$(wc -c < \"$dir/runner\") && [ \"$((size))\" -eq {bytes} ]; then \
         chmod 700 \"$dir/runner\"; \
         if {CHECKSUM_TOOL} && cp \"$dir/runner\" {base}/.proofstorm-runner.$$ 2>/dev/null; then \
         for old in {base}/proofstorm-runner-*; do [ \"$old\" = {base}/proofstorm-runner-{digest} ] || rm -f -- \"$old\"; done; \
         mv -f {base}/.proofstorm-runner.$$ {base}/proofstorm-runner-{digest} 2>/dev/null || rm -f -- {base}/.proofstorm-runner.$$; fi; \
         printf '%s' \"$dir\"; trap - HUP PIPE TERM; \
         else rm -rf -- \"$dir\"; exit 65; fi"
    )
}

/// True where the container can compute SHA-256.
const CHECKSUM_TOOL: &str =
    "{ command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1; }";

/// Exit code of [`cached_install_script`] when this pod has no usable cache.
const NO_CACHED_RUNNER: i32 = 75;

/// Start from this pod's cached runner instead of uploading it. Anything in the
/// container can write the cache, so the private copy must match the digest
/// before it is used; a mismatch deletes the cache. Exits
/// [`NO_CACHED_RUNNER`] when the cache is absent, wrong or uncheckable.
fn cached_install_script(base: &str, digest: &str) -> String {
    format!(
        "set -eu; umask 077; cache={base}/proofstorm-runner-{digest}; \
         {CHECKSUM_TOOL} && [ -f \"$cache\" ] || exit {NO_CACHED_RUNNER}; \
         sum() {{ sha256sum \"$1\" 2>/dev/null || shasum -a 256 \"$1\"; }}; \
         dir=$(mktemp -d {base}/proofstorm-exec.XXXXXXXX); \
         trap 'rm -rf -- \"$dir\"; exit 70' HUP PIPE TERM; \
         if cp \"$cache\" \"$dir/runner\" && [ \"$(sum \"$dir/runner\" | cut -c1-64)\" = {digest} ]; then \
         chmod 700 \"$dir/runner\"; printf '%s' \"$dir\"; trap - HUP PIPE TERM; exit 0; fi; \
         rm -rf -- \"$dir\"; rm -f -- \"$cache\"; exit {NO_CACHED_RUNNER}"
    )
}

/// The runner binary and its SHA-256, read once per controller process.
fn runner() -> Result<&'static (Vec<u8>, String), Error> {
    static RUNNER: std::sync::OnceLock<(Vec<u8>, String)> = std::sync::OnceLock::new();
    if let Some(runner) = RUNNER.get() {
        return Ok(runner);
    }
    let binary = std::fs::read(
        std::env::var("PROOFSTORM_NATIVE_RUNNER")
            .unwrap_or_else(|_| "/usr/local/lib/proofstorm-exec".into()),
    )
    .map_err(|_| Error::LiveExec("native supervisor binary unavailable".into()))?;
    let digest = format!("{:x}", Sha256::digest(&binary));
    Ok(RUNNER.get_or_init(|| (binary, digest)))
}

/// Best effort: remove an exec directory that holds no receipt anyone needs.
async fn remove_directory(pods: &Api<Pod>, reference: &NativeExecutionRef) {
    if !is_exec_directory(&reference.directory) {
        return;
    }
    let _ = exec(
        pods,
        &reference.pod,
        &reference.container,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "rm -rf -- \"$1\"".into(),
            "proofstorm-exec".into(),
            reference.directory.clone(),
        ],
        None,
    )
    .await;
}

/// kube-client sends stdin in chunks no larger than this buffer, one WebSocket
/// message each, and never more than 4 KiB at a time. Its default is 1 KiB:
/// about 3,700 messages per runner upload. 4 KiB measured about twice as fast.
const EXEC_STDIN_BUFFER_BYTES: usize = 4096;

fn attach_params(container: &str, stdin: bool) -> AttachParams {
    AttachParams::default()
        .container(container.to_owned())
        .stdin(stdin)
        .stdout(true)
        .stderr(true)
        .max_stdin_buf_size(EXEC_STDIN_BUFFER_BYTES)
}

pub(super) async fn exec_bounded(
    pods: &Api<Pod>,
    pod: &str,
    container: &str,
    args: Vec<String>,
    input: Option<&[u8]>,
    maximum: usize,
) -> Result<Vec<u8>, Error> {
    match exec_status(pods, pod, container, args, input, maximum).await? {
        (0, out) => Ok(out),
        _ => Err(Error::LiveExec("native supervisor transport failed".into())),
    }
}

/// Run a helper and return its exit code with its bounded stdout.
async fn exec_status(
    pods: &Api<Pod>,
    pod: &str,
    container: &str,
    args: Vec<String>,
    input: Option<&[u8]>,
    maximum: usize,
) -> Result<(i32, Vec<u8>), Error> {
    let run = async {
        let attach = attach_params(container, input.is_some());
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
        if truncated {
            return Err(Error::LiveExec("native supervisor transport failed".into()));
        }
        Ok((exec_exit_code(status.as_ref()), out))
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

/// Output of `mktemp -d /tmp/proofstorm-exec.XXXXXXXX`: one directory, no traversal.
fn is_exec_directory(directory: &str) -> bool {
    directory
        .strip_prefix("/tmp/proofstorm-exec.")
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
        })
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
    action: &ProofstormCellAction,
    cell: &ProofstormCell,
    context: &Context,
) -> Result<Action, Error> {
    let CellAction::ComponentExecLive(request) = &action.spec.action else {
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
    if let Some(start) = super::workspace_bridge::controlled_start(action, cell) {
        command.argv = vec![
            proofstorm_core::workspace::WORKSPACE_RUNNER.into(),
            "workspace".into(),
            "bridge".into(),
            serde_json::to_string(&proofstorm_core::workspace::control::BridgeRequest::Start {
                start,
                owner: action.spec.operation_id.clone(),
            })
            .map_err(|_| Error::ControllerInvariant("workspace start serialization failed"))?,
        ];
    }
    if let Err(message) = command.validate() {
        return patch_invalid_action(action, context, message).await;
    }
    let plans = compile_component_plans(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.spec.cell,
        &cell.spec.lock,
    )?;
    let Some(component) = plans
        .iter()
        .find(|plan| plan.component_id == request.component)
    else {
        return patch_invalid_action(action, context, "component missing from immutable cell")
            .await;
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
        // Cancels are rare and must not wait behind a full poll pool.
        let _poll = if action.annotations().contains_key(ACTION_CANCEL_ANNOTATION) {
            None
        } else {
            let Ok(permit) = context.native.polls.try_acquire() else {
                return Ok(gate_busy(action));
            };
            Some(permit)
        };
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
        if let Ok(bytes) = reply
            && let Ok(mut receipt) = serde_json::from_slice::<Value>(&bytes)
        {
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
                        ProofstormCellActionStatus {
                            phase: ActionPhase::Running,
                            native_execution: Some(reference.clone()),
                            started_at_unix: Some(started),
                            artifact: Some(status_object(cap_public_streams(receipt))),
                            ..ProofstormCellActionStatus::default()
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
                    ProofstormCellActionStatus {
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
                        error: (!clean)
                            .then(|| status_object(json!({"code":"native_cleanup_unverified"}))),
                        ..ProofstormCellActionStatus::default()
                    },
                )
                .await?;
                // The receipt is now durable in the action status, so the in-pod
                // directory and its runner copy can go. Task starts keep theirs:
                // the task outlives this receipt.
                if same_pod
                    && clean
                    && !custody_pending
                    && super::workspace_bridge::controlled_start(action, cell).is_none()
                {
                    remove_directory(&pods, reference).await;
                }
                return Ok(Action::await_change());
            }
            if receipt.get("runner_error").is_some() {
                return patch_action_failure(action, context, "native_runner_failed", "supervisor did not establish verified process cleanup; inspect the cell before retrying").await;
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
        return Ok(Action::requeue(poll_interval(now_unix() - started)));
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
    // A start holds one of its component's start places and a start permit.
    let Some(_component) = context.native.start_component(&format!(
        "{}/{}",
        action.spec.instance_key, request.component
    )) else {
        return Ok(gate_busy(action));
    };
    let Ok(_start) = context.native.starts.try_acquire() else {
        return Ok(gate_busy(action));
    };
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
    let (binary, digest) = runner()?;
    // Copy this pod's cached runner when it checks out; upload it otherwise.
    let directory = match exec_status(
        &pods,
        &pod.name_any(),
        &container,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            cached_install_script("/tmp", digest),
        ],
        None,
        65536,
    )
    .await?
    {
        (0, directory) => directory,
        (NO_CACHED_RUNNER, _) => {
            exec(
                &pods,
                &pod.name_any(),
                &container,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    install_script("/tmp", binary.len(), digest),
                ],
                Some(binary),
            )
            .await?
        }
        _ => return Err(Error::LiveExec("native supervisor transport failed".into())),
    };
    let directory = String::from_utf8(directory)
        .map_err(|_| Error::LiveExec("invalid supervisor directory".into()))?;
    if !is_exec_directory(&directory) {
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
        runner_digest: format!("sha256:{digest}"),
    };
    let version = action.resource_version().ok_or(Error::ControllerInvariant(
        "native start requires an observed resource version",
    ))?;
    let namespace = action.namespace().ok_or(Error::ControllerInvariant(
        "native action namespace missing",
    ))?;
    let actions = Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace);
    // A conditional status write is the global start fence. Overlapping
    // controllers or a concurrent cancellation invalidate a stale start claim.
    // No command is launched unless this exact observed version wins the claim.
    // A lost claim leaves this attempt's directory unreferenced, so remove it.
    if let Err(error) = actions
        .patch_status(
            &action.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata":{"resourceVersion":version},
                "status":ProofstormCellActionStatus {
                    phase: ActionPhase::Running,
                    observed_generation: action.metadata.generation,
                    native_execution: Some(reference.clone()),
                    job_name: Some("live-exec-started".into()),
                    started_at_unix: Some(now_unix()),
                    ..ProofstormCellActionStatus::default()
                }
            })),
        )
        .await
    {
        remove_directory(&pods, &reference).await;
        return Err(error.into());
    }
    let Ok(private_input) = super::private_transfer::start(action, context).await else {
        remove_directory(&pods, &reference).await;
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
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[test]
    fn runner_uploads_use_the_largest_stdin_chunks_kube_sends() {
        let attach = attach_params("component", true);
        assert_eq!(attach.max_stdin_buf_size, Some(4096));
        assert_eq!(attach.container.as_deref(), Some("component"));
        assert!(attach.stdin && attach.stdout && attach.stderr && !attach.tty);
        assert!(!attach_params("component", false).stdin);
    }

    #[test]
    fn polls_and_starts_cannot_starve_each_other_or_the_controller() {
        let limits = proofstorm_core::ControllerLimits::default();
        let (polls_cap, starts_cap) = (
            usize::from(limits.native_polls),
            usize::from(limits.native_starts),
        );
        let gate = Gate::new(&limits);
        let polls = (0..polls_cap)
            .map(|_| gate.polls.try_acquire().unwrap())
            .collect::<Vec<_>>();
        assert!(gate.polls.try_acquire().is_err());
        // A full poll pool doesn't block starts, and the reverse.
        let starts = (0..starts_cap)
            .map(|_| gate.starts.try_acquire().unwrap())
            .collect::<Vec<_>>();
        assert!(
            gate.starts.try_acquire().is_err(),
            "a fifth concurrent start waits"
        );
        drop((polls, starts));
        assert_eq!(gate.polls.available_permits(), polls_cap);
        assert_eq!(gate.starts.available_permits(), starts_cap);
    }

    #[test]
    fn configured_limits_size_every_pool() {
        let limits = proofstorm_core::ControllerLimits {
            action_slots: 64,
            native_starts: 24,
            native_starts_per_component: 3,
            native_polls: 24,
            start_deadline_seconds: 300,
        };
        let gate = Gate::new(&limits);
        assert_eq!(gate.starts.available_permits(), 24);
        assert_eq!(gate.polls.available_permits(), 24);
        let held = (0..3)
            .map(|_| gate.start_component("i1/wallet").unwrap())
            .collect::<Vec<_>>();
        assert!(gate.start_component("i1/wallet").is_none());
        drop(held);
    }

    #[test]
    fn a_hung_component_holds_only_its_own_start_places() {
        let gate = Gate::default();
        let hung = (0..gate.starts_per_component)
            .map(|_| gate.start_component("i1/chain").unwrap())
            .collect::<Vec<_>>();
        assert!(gate.start_component("i1/chain").is_none());
        // Other components, and the same name in another cell, still start.
        let other = gate.start_component("i1/wallet").unwrap();
        let elsewhere = gate.start_component("i2/chain").unwrap();
        drop((other, elsewhere));
        drop(hung);
        assert!(
            gate.components.lock().unwrap().is_empty(),
            "places are released"
        );
        assert!(gate.start_component("i1/chain").is_some());
    }

    #[test]
    fn receipt_polls_slow_down_while_a_command_runs() {
        let schedule = [0, 9, 10, 59, 60, 3600].map(|elapsed| poll_interval(elapsed).as_secs());
        assert_eq!(schedule, [1, 1, 2, 2, 5, 5]);
        // A clock step backwards still polls promptly.
        assert_eq!(poll_interval(-30), Duration::from_secs(1));
    }

    fn digest(runner: &[u8]) -> String {
        format!("{:x}", Sha256::digest(runner))
    }

    fn exec_dirs(base: &std::path::Path) -> usize {
        std::fs::read_dir(base)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("proofstorm-exec.")
            })
            .count()
    }

    fn cached(base: &str, digest: &str) -> std::process::Output {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(cached_install_script(base, digest))
            .output()
            .unwrap()
    }

    fn install(base: &str, input: &[u8], expected_bytes: usize) -> std::process::Output {
        install_as(base, input, expected_bytes, &digest(input))
    }

    fn install_as(
        base: &str,
        input: &[u8],
        expected_bytes: usize,
        digest: &str,
    ) -> std::process::Output {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(install_script(base, expected_bytes, digest))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn an_incomplete_runner_upload_leaves_no_directory_behind() {
        let base =
            std::env::temp_dir().join(format!("proofstorm-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let base_path = base.to_str().unwrap();
        let runner = b"#!/bin/sh\nexit 0\n";

        let complete = install(base_path, runner, runner.len());
        assert!(complete.status.success(), "{complete:?}");
        let directory = String::from_utf8(complete.stdout).unwrap();
        assert!(directory.starts_with(&format!("{base_path}/proofstorm-exec.")));
        assert_eq!(
            std::fs::read(format!("{directory}/runner")).unwrap(),
            runner
        );
        std::fs::remove_dir_all(&directory).unwrap();

        // The transport timed out mid-stream: fewer bytes arrived than were sent.
        let short = install(base_path, &runner[..5], runner.len());
        assert_eq!(short.status.code(), Some(65));
        assert!(short.stdout.is_empty());
        assert_eq!(exec_dirs(&base), 0, "partial runner left behind");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_cached_runner_is_used_only_when_its_private_copy_matches_the_digest() {
        let base =
            std::env::temp_dir().join(format!("proofstorm-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let base_path = base.to_str().unwrap();
        let runner = b"#!/bin/sh\nexit 0\n";
        let expected = digest(runner);
        let cache = base.join(format!("proofstorm-runner-{expected}"));

        // Nothing cached yet: the controller uploads instead.
        assert_eq!(
            cached(base_path, &expected).status.code(),
            Some(NO_CACHED_RUNNER)
        );
        assert_eq!(exec_dirs(&base), 0);

        // A complete upload caches the runner for this pod.
        let uploaded = install(base_path, runner, runner.len());
        assert!(uploaded.status.success(), "{uploaded:?}");
        std::fs::remove_dir_all(String::from_utf8(uploaded.stdout).unwrap()).unwrap();
        assert_eq!(std::fs::read(&cache).unwrap(), runner);

        // A later start copies it without an upload.
        let hit = cached(base_path, &expected);
        assert!(hit.status.success(), "{hit:?}");
        let directory = String::from_utf8(hit.stdout).unwrap();
        assert!(is_exec_directory(&directory.replace(base_path, "/tmp")));
        assert_eq!(
            std::fs::read(format!("{directory}/runner")).unwrap(),
            runner
        );
        std::fs::remove_dir_all(&directory).unwrap();

        // Something in the container replaced the cache: refuse it and remove it.
        std::fs::write(&cache, b"#!/bin/sh\nexit 7\n").unwrap();
        assert_eq!(
            cached(base_path, &expected).status.code(),
            Some(NO_CACHED_RUNNER)
        );
        assert!(!cache.exists());
        assert_eq!(exec_dirs(&base), 0);

        // A new runner version's upload removes the old version's cache.
        std::fs::write(&cache, runner).unwrap();
        let newer = b"#!/bin/sh\nexit 0 # v2\n";
        let upgraded = install(base_path, newer, newer.len());
        assert!(upgraded.status.success(), "{upgraded:?}");
        std::fs::remove_dir_all(String::from_utf8(upgraded.stdout).unwrap()).unwrap();
        assert!(!cache.exists());
        assert_eq!(
            std::fs::read(base.join(format!("proofstorm-runner-{}", digest(newer)))).unwrap(),
            newer
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_a_single_mktemp_directory_is_removable() {
        assert!(is_exec_directory("/tmp/proofstorm-exec.Ab12Cd34"));
        for directory in [
            "/tmp/proofstorm-exec.",
            "/tmp/proofstorm-exec../etc",
            "/tmp/proofstorm-exec.Ab12/..",
            "/tmp/proofstorm-exec.Ab-12",
            "/tmp/other.Ab12Cd34",
            "/",
        ] {
            assert!(!is_exec_directory(directory), "{directory}");
        }
    }
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
