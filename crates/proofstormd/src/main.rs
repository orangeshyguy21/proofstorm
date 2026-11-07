#[cfg(test)]
mod candidate_tests;
mod component_lifecycle;
mod probes;
mod retry;
#[cfg(test)]
mod start_deadline_tests;
#[cfg(test)]
use component_lifecycle::same_lifecycle_identity;
mod cell_updates;
mod component_logs;
mod generated_secrets;
mod workspace_bridge;
mod workspace_faults;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    batch::v1::{Job, JobStatus},
    core::v1::{
        ConfigMap, LimitRange, Namespace, PersistentVolumeClaim, Pod, ResourceQuota, Secret,
        Service, ServiceAccount,
    },
    discovery::v1::EndpointSlice,
    networking::v1::NetworkPolicy,
};
use kube::{
    Api, Client, ResourceExt,
    api::{
        AttachParams, DeleteParams, ListParams, LogParams, Patch, PatchParams, PropagationPolicy,
    },
    runtime::{
        Controller, WatchStreamExt,
        controller::Action,
        finalizer::{Event, finalizer},
        reflector, watcher,
    },
};
use proofstorm_core::CandidateBuildPhase;
use proofstorm_core::WorkloadControllerKind;
use proofstorm_kube::CANDIDATE_BUILD_LABEL;
use proofstorm_kube::{
    ACTION_CANCEL_ANNOTATION, ActionAdmissionError, ActionPhase, ActionRenderError, AdapterError,
    AuthenticationConformanceResult, AuthenticationProtectedSpendResult,
    AuthenticationReplayResult, AuthenticationSessionFailureStage, BACKEND_ID_ANNOTATION,
    CANDIDATE_CANCEL_ANNOTATION, CELL_NETWORK_POLICY, COMPONENT_LABEL, CellAction, CellPhase,
    ComponentObservationResources, EXECUTION_STATE_CONTRACT_ANNOTATION,
    FINISHED_ACTION_RETENTION_SECONDS, INSTANCE_LABEL, LIFECYCLE_RESTART_ANNOTATION,
    LIFECYCLE_SEQUENCE_ANNOTATION, LIFECYCLE_STATE_ANNOTATION, LoadedCell,
    ProofstormCandidateBuild, ProofstormCandidateBuildStatus, ProofstormCell, ProofstormCellAction,
    ProofstormCellActionStatus, ProofstormCellStatus, ProofstormComponent, action_result_container,
    evaluate_action_admission, instance_namespace, observe_component_statuses,
    render_candidate_build_job, render_cell, render_cell_action_job, render_cell_network_policy,
    render_component_network_policy, render_security_spine,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

const FINALIZER: &str = "proofstorm.dev/cell-cleanup";
const FIELD_MANAGER: &str = "proofstormd";
const CELL_CONTROLLER_CONCURRENCY: u16 = 8;
const MAX_ACTION_STATUS_BYTES: usize = 64 * 1024;
/// Owns only the cell's protocol freshness heartbeat, so the full status
/// apply never carries it and a probe-only change is one small write.
const HEARTBEAT_FIELD_MANAGER: &str = "proofstormd-heartbeat";
/// Component object writes in flight per cell observation.
const COMPONENT_WRITE_CONCURRENCY: usize = 16;

#[derive(Clone)]
struct Context {
    client: Client,
    probes: Arc<probes::Manager>,
    /// Shared by every controller; keyed by object reference.
    retries: Arc<retry::Backoff>,
    /// Caps native exec work so it can't take every action-controller slot.
    native: Arc<native_exec::Gate>,
    /// Slot counts and the start deadline, from `runtime.toml` via the chart.
    limits: proofstorm_core::ControllerLimits,
    /// The cell controller's watch cache, so actions don't fetch whole cells.
    cells: kube::runtime::reflector::Store<ProofstormCell>,
    /// Immutable cell revisions, read once each.
    revisions: Arc<proofstorm_kube::RevisionCache>,
    /// Every cell's component status objects, from one watch.
    components: reflector::Store<ProofstormComponent>,
}

#[cfg(test)]
impl Context {
    /// A context around a fake API, with default limits and empty caches.
    fn for_tests(client: Client, probes: Arc<probes::Manager>) -> Self {
        Self {
            client,
            probes,
            retries: Arc::default(),
            native: Arc::default(),
            limits: proofstorm_core::ControllerLimits::default(),
            cells: reflector::store().0,
            revisions: Arc::default(),
            components: reflector::store().0,
        }
    }
}

/// Ok when the object was deleted or was already gone.
fn gone<T>(result: Result<T, kube::Error>) -> Result<(), Error> {
    match result {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The cached component objects of one cell instance, once the cache's first
/// list has landed. A cache with no writer (tests) is used as it is.
async fn stored_components(
    context: &Context,
    instance_key: &str,
) -> Result<Vec<Arc<ProofstormComponent>>, Error> {
    // A dropped writer still counts as ready: the cache keeps what it has.
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        context.components.wait_until_ready(),
    )
    .await
    .map_err(|_| Error::ControllerInvariant("component status cache is not ready"))?;
    Ok(context
        .components
        .state()
        .into_iter()
        .filter(|object| object.spec.instance_key == instance_key)
        .collect())
}

/// The loaded cell with its component statuses filled from the cache.
async fn with_component_statuses(
    context: &Context,
    mut cell: LoadedCell,
) -> Result<LoadedCell, Error> {
    let stored = stored_components(context, &cell.spec.instance_key).await?;
    cell.assemble_components(stored.iter().map(AsRef::as_ref));
    Ok(cell)
}

/// An action's cell from the watch cache, or from the API before the cache has
/// it. Actions bind to their own revision through `cell_updates::action_cell`,
/// so a cache entry that lags an edit is resolved there.
async fn action_cell_source(
    action: &ProofstormCellAction,
    namespace: &str,
    context: &Context,
) -> Result<ProofstormCell, Error> {
    let reference =
        kube::runtime::reflector::ObjectRef::new(&action.spec.cell_name).within(namespace);
    if let Some(cell) = context.cells.get(&reference) {
        return Ok((*cell).clone());
    }
    Ok(
        Api::<ProofstormCell>::namespaced(context.client.clone(), namespace)
            .get(&action.spec.cell_name)
            .await?,
    )
}

#[derive(Debug, Error)]
enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("ProofstormCell {0} has no namespace")]
    MissingNamespace(String),
    #[error("invalid instance key {0:?}")]
    InvalidInstanceKey(String),
    #[error("controller invariant failed: {0}")]
    ControllerInvariant(&'static str),
    #[error("generated Secret contract failed: {0}")]
    SecretContract(String),
    #[error("cleanup pending while instance namespace {0} still exists")]
    CleanupPending(String),
    #[error("cleanup pending while runtime actions for instance {0} are being removed")]
    ActionCleanupPending(String),
    #[error("component adapter failed: {0}")]
    Adapter(#[from] AdapterError),
    #[error("action adapter failed: {0}")]
    Action(#[from] ActionRenderError),
    #[error("candidate build adapter failed: {0}")]
    CandidateBuild(#[from] proofstorm_kube::CandidateBuildRenderError),
    #[error("{kind} status is {actual} bytes; controller maximum is {maximum} bytes")]
    StatusBudgetExceeded {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("live component execution failed: {0}")]
    LiveExec(String),
    #[error("cell revision: {0}")]
    Revision(#[from] proofstorm_kube::RevisionError),
}

#[cfg(test)]
fn pod_belongs_to_instance(pod: &Pod, key: &str) -> bool {
    pod.labels()
        .get(INSTANCE_LABEL)
        .is_some_and(|value| value == key)
        && pod.metadata.namespace.as_deref() == Some(instance_namespace(key).as_str())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(argument) = std::env::args().nth(1) {
        if argument == "--release-info" {
            use sha2::{Digest, Sha256};
            println!(
                "{}",
                serde_json::json!({
                    "format_version": 1, "version": env!("CARGO_PKG_VERSION"),
                    "source_sha256": option_env!("PROOFSTORM_CONTROLLER_SOURCE_SHA256").unwrap_or("development"),
                    "runtime_contract_sha256": format!("{:x}", Sha256::digest(proofstorm_kube::release::contract().to_string()))
                })
            );
            return Ok(());
        }
        return Err(format!("unknown argument: {argument}").into());
    }
    let client = Client::try_default().await?;
    let cells = Api::<ProofstormCell>::all(client.clone());
    let actions = Api::<ProofstormCellAction>::all(client.clone());
    let candidate_builds = Api::<ProofstormCandidateBuild>::all(client.clone());
    let image = std::env::var("PROOFSTORM_PROBER_IMAGE")
        .map_err(|_| "PROOFSTORM_PROBER_IMAGE must name the installed controller image")?;
    let (probes, triggers) = probes::Manager::new(client.clone(), image);
    let limits = proofstorm_core::ControllerLimits::from_env(|name| std::env::var(name).ok())?;
    let cell_watch = Controller::new(cells, watcher::Config::default());
    let (components, component_writer) = reflector::store();
    let component_cache = reflector(
        component_writer,
        watcher(
            Api::<ProofstormComponent>::all(client.clone()),
            watcher::Config::default(),
        ),
    )
    .default_backoff()
    .for_each(|event| async move {
        if let Err(error) = event {
            eprintln!("component status watch failed: {error}");
        }
    });
    let context = Arc::new(Context {
        client,
        probes,
        retries: Arc::default(),
        native: Arc::new(native_exec::Gate::new(&limits)),
        limits,
        cells: cell_watch.store(),
        revisions: Arc::default(),
        components,
    });
    let trigger_stream = futures::stream::unfold(triggers, |mut receive| async {
        receive.recv().await.map(|object| (object, receive))
    });
    let cell_controller = cell_watch
        .reconcile_on(trigger_stream)
        .with_config(
            kube::runtime::controller::Config::default().concurrency(CELL_CONTROLLER_CONCURRENCY),
        )
        .shutdown_on_signal()
        .run(reconcile_cell, error_policy, context.clone())
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormCell {object:?}"),
                Err(error) => eprintln!("reconciliation failed: {error}"),
            }
        });
    let action_controller = Controller::new(actions, watcher::Config::default())
        .with_config(kube::runtime::controller::Config::default().concurrency(limits.action_slots))
        .shutdown_on_signal()
        .run(
            reconcile_action_with_backoff,
            action_error_policy,
            context.clone(),
        )
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormCellAction {object:?}"),
                Err(error) => eprintln!("action reconciliation failed: {error}"),
            }
        });
    let candidate_controller = Controller::new(candidate_builds, watcher::Config::default())
        .with_config(kube::runtime::controller::Config::default().concurrency(2))
        .shutdown_on_signal()
        .run(
            reconcile_candidate_build_with_backoff,
            candidate_error_policy,
            context.clone(),
        )
        .for_each(|result| async move {
            match result {
                Ok((object, _)) => eprintln!("reconciled ProofstormCandidateBuild {object:?}"),
                Err(error) => eprintln!("candidate build reconciliation failed: {error}"),
            }
        });
    let probe_scheduler = context.probes.clone().run();
    tokio::select! {
        result = probe_scheduler => result.map_err(Into::into),
        () = async { tokio::join!(cell_controller, action_controller, candidate_controller); } => Ok(()),
        () = component_cache => Ok(()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass keeps durable Job creation and monotonic terminal observation together"
)]
async fn reconcile_candidate_build(
    build: Arc<ProofstormCandidateBuild>,
    context: Arc<Context>,
) -> Result<Action, Error> {
    let namespace = build
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(build.name_any()))?;
    if build
        .status
        .as_ref()
        .is_some_and(|status| status.phase.terminal())
    {
        if build
            .status
            .as_ref()
            .is_some_and(|s| s.diagnostics.is_some())
        {
            let jobs = Api::<Job>::namespaced(context.client.clone(), &namespace);
            let job_name = format!("{}-build", build.name_any());
            if jobs.get_opt(&job_name).await?.is_some() {
                jobs.patch(
                    &job_name,
                    &PatchParams::default(),
                    &Patch::Merge(serde_json::json!({"spec":{"ttlSecondsAfterFinished":600}})),
                )
                .await?;
            }
        }
        return Ok(Action::await_change());
    }
    let jobs = Api::<Job>::namespaced(context.client.clone(), &namespace);
    let job_name = format!("{}-build", build.name_any());
    if build
        .annotations()
        .contains_key(CANDIDATE_CANCEL_ANNOTATION)
    {
        let diagnostics =
            if let Some(diagnostics) = build.status.as_ref().and_then(|s| s.diagnostics.clone()) {
                diagnostics
            } else {
                archive_candidate_logs(build.as_ref(), &context).await?
            };
        let mut archived = build.status.clone().unwrap_or_default();
        archived.diagnostics = Some(diagnostics.clone());
        patch_candidate_status(build.as_ref(), &context, archived).await?;
        let deleted = jobs
            .delete(
                &job_name,
                &DeleteParams {
                    propagation_policy: Some(PropagationPolicy::Background),
                    ..DeleteParams::default()
                },
            )
            .await;
        if let Err(error) = deleted
            && !matches!(&error, kube::Error::Api(response) if response.code == 404)
        {
            return Err(error.into());
        }
        patch_candidate_status(
            build.as_ref(),
            &context,
            ProofstormCandidateBuildStatus {
                phase: CandidateBuildPhase::Cancelled,
                diagnostics: Some(diagnostics),
                observed_generation: build.metadata.generation,
                job_name: Some(job_name),
                started_at_unix: build
                    .status
                    .as_ref()
                    .and_then(|status| status.started_at_unix),
                completed_at_unix: Some(now_unix()),
                ..ProofstormCandidateBuildStatus::default()
            },
        )
        .await?;
        return Ok(Action::await_change());
    }
    let observed = jobs.get_opt(&job_name).await?;
    let Some(observed) = observed else {
        if build
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix)
            .is_some()
        {
            patch_candidate_status(
                build.as_ref(),
                &context,
                ProofstormCandidateBuildStatus {
                    phase: CandidateBuildPhase::Failed,
                    diagnostics: Some(archive_candidate_logs(build.as_ref(), &context).await?),
                    observed_generation: build.metadata.generation,
                    job_name: Some(job_name),
                    started_at_unix: build
                        .status
                        .as_ref()
                        .and_then(|status| status.started_at_unix),
                    completed_at_unix: Some(now_unix()),
                    error_code: Some("candidate_job_lost".into()),
                    message: Some(
                        "controller-owned build Job disappeared after execution began".into(),
                    ),
                    ..ProofstormCandidateBuildStatus::default()
                },
            )
            .await?;
            return Ok(Action::await_change());
        }
        let job = render_candidate_build_job(build.as_ref())?;
        jobs.patch(
            &job_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&job),
        )
        .await?;
        patch_candidate_status(
            build.as_ref(),
            &context,
            ProofstormCandidateBuildStatus {
                phase: CandidateBuildPhase::Building,
                observed_generation: build.metadata.generation,
                job_name: Some(job_name),
                started_at_unix: Some(now_unix()),
                ..ProofstormCandidateBuildStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(3)));
    };
    let status = observed.status.unwrap_or_default();
    let succeeded = status.succeeded.unwrap_or_default() > 0;
    let failed = status.failed.unwrap_or_default() > 0
        || status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Failed" && condition.status == "True")
        });
    let started_at_unix = build
        .status
        .as_ref()
        .and_then(|value| value.started_at_unix)
        .or_else(|| Some(now_unix()));
    if !succeeded && !failed {
        patch_candidate_status(
            build.as_ref(),
            &context,
            ProofstormCandidateBuildStatus {
                phase: CandidateBuildPhase::Building,
                observed_generation: build.metadata.generation,
                job_name: Some(job_name),
                started_at_unix,
                ..ProofstormCandidateBuildStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(3)));
    }
    let pod_api = Api::<Pod>::namespaced(context.client.clone(), &namespace);
    let pods = pod_api
        .list(&ListParams::default().labels(&format!("job-name={job_name}")))
        .await?;
    let mut terminal = if failed {
        let failure = candidate_failure_message(&pod_api, &status, &pods.items).await;
        ProofstormCandidateBuildStatus {
            phase: CandidateBuildPhase::Failed,
            observed_generation: build.metadata.generation,
            job_name: Some(job_name),
            started_at_unix,
            completed_at_unix: Some(now_unix()),
            error_code: Some("candidate_build_failed".into()),
            message: Some(failure),
            ..ProofstormCandidateBuildStatus::default()
        }
    } else {
        let digest = pods
            .items
            .iter()
            .find_map(|pod| termination_message(pod, "buildkit"))
            .and_then(|message| serde_json::from_str::<serde_json::Value>(&message).ok())
            .and_then(|receipt| receipt.get("digest")?.as_str().map(str::to_owned));
        if digest.as_deref().is_some_and(valid_sha256_digest) {
            ProofstormCandidateBuildStatus {
                phase: CandidateBuildPhase::Succeeded,
                observed_generation: build.metadata.generation,
                job_name: Some(job_name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                image: Some(format!(
                    "{}@{}",
                    build.spec.image_repository,
                    digest.unwrap_or_default()
                )),
                ..ProofstormCandidateBuildStatus::default()
            }
        } else {
            ProofstormCandidateBuildStatus {
                phase: CandidateBuildPhase::Failed,
                observed_generation: build.metadata.generation,
                job_name: Some(job_name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                error_code: Some("candidate_digest_missing".into()),
                message: Some("successful build Job did not report an image digest".into()),
                ..ProofstormCandidateBuildStatus::default()
            }
        }
    };
    terminal.diagnostics = Some(archive_candidate_logs(build.as_ref(), &context).await?);
    patch_candidate_status(build.as_ref(), &context, terminal).await?;
    // Cleanup becomes eligible only after diagnostics are durably committed to the CRD.
    jobs.patch(
        &format!("{}-build", build.name_any()),
        &PatchParams::default(),
        &Patch::Merge(serde_json::json!({"spec":{"ttlSecondsAfterFinished":600}})),
    )
    .await?;
    Ok(Action::await_change())
}

async fn archive_candidate_logs(
    build: &ProofstormCandidateBuild,
    context: &Context,
) -> Result<proofstorm_core::CandidateDiagnostics, Error> {
    let namespace = build
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(build.name_any()))?;
    let pods = Api::<Pod>::namespaced(context.client.clone(), &namespace);
    let observed = pods
        .list(
            &ListParams::default().labels(&format!("{CANDIDATE_BUILD_LABEL}={}", build.name_any())),
        )
        .await?;
    let mut logs = std::collections::BTreeMap::new();
    for container in ["source", "buildkit"] {
        let result = if let Some(pod) = observed.items.first() {
            pods.logs(
                &pod.name_any(),
                &kube::api::LogParams {
                    container: Some(container.into()),
                    // A byte limit alone reads the beginning of a long build.
                    // Request recent lines so terminal diagnostics survive cleanup.
                    tail_lines: Some(128),
                    limit_bytes: Some(32_769),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| e.to_string())
        } else {
            Err("Build Pod is unavailable; no log was captured".into())
        };
        let log = match result {
            Ok(mut text) => {
                let truncated = text.len() > 32_768 || text.lines().count() >= 128;
                if text.len() > 32_768 {
                    let mut start = text.len() - 32_768;
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    text.drain(..start);
                }
                proofstorm_core::CandidateLog {
                    text,
                    truncated,
                    unavailable: None,
                }
            }
            Err(message) => proofstorm_core::CandidateLog {
                text: String::new(),
                truncated: false,
                unavailable: Some(message.chars().take(1000).collect()),
            },
        };
        logs.insert(container.into(), log);
    }
    Ok(proofstorm_core::CandidateDiagnostics {
        captured_at_unix: now_unix(),
        logs,
    })
}

async fn candidate_failure_message(
    pods: &Api<Pod>,
    status: &JobStatus,
    observed: &[Pod],
) -> String {
    let log_tail = if let Some(pod) = observed.first() {
        pods.logs(
            &pod.name_any(),
            &kube::api::LogParams {
                container: Some("buildkit".into()),
                tail_lines: Some(24),
                ..kube::api::LogParams::default()
            },
        )
        .await
        .ok()
        .map(|logs| bounded_log_tail(&logs, 3_000))
    } else {
        None
    };
    serde_json::json!({
        "failure": action_failure(status, observed),
        "log_tail": log_tail,
        "recovery": "this exact candidate request is terminal; inspect the diagnostic and do not retry under a new ID unless the source or build environment changes"
    })
    .to_string()
}

fn bounded_log_tail(logs: &str, maximum_chars: usize) -> String {
    let reversed = logs.chars().rev().take(maximum_chars).collect::<String>();
    reversed.chars().rev().collect()
}

fn valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

async fn patch_candidate_status(
    build: &ProofstormCandidateBuild,
    context: &Context,
    status: ProofstormCandidateBuildStatus,
) -> Result<(), Error> {
    if build.status.as_ref() == Some(&status) {
        return Ok(());
    }
    let namespace = build
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(build.name_any()))?;
    let builds = Api::<ProofstormCandidateBuild>::namespaced(context.client.clone(), &namespace);
    builds
        .patch_status(
            &build.name_any(),
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(serde_json::json!({
                "apiVersion": "proofstorm.dev/v1alpha1",
                "kind": "ProofstormCandidateBuild",
                "status": status,
            })),
        )
        .await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass keeps typed admission, deterministic Job identity, and terminal observation visibly contiguous"
)]
async fn reconcile_action(
    action: Arc<ProofstormCellAction>,
    context: Arc<Context>,
) -> Result<Action, Error> {
    let control_namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    if workspace_faults::is_owned_network(&action) {
        return workspace_faults::reconcile(&action, &context).await;
    }
    if action
        .status
        .as_ref()
        .is_some_and(|status| is_terminal_action(status.phase))
    {
        if workspace_bridge::is_controlled_start(&action) {
            return workspace_bridge::reconcile(&action, &context).await;
        }
        return expire_finished_action(&action, &context).await;
    }
    if action.annotations().contains_key(ACTION_CANCEL_ANNOTATION) {
        if matches!(action.spec.action, CellAction::ComponentExecLive(_))
            && action
                .status
                .as_ref()
                .is_some_and(|status| status.native_execution.is_some())
        {
            let cell = action_cell_source(&action, &control_namespace, &context).await?;
            let cell = cell_updates::action_cell(&action, cell, &context).await?;
            return Box::pin(native_exec::reconcile(action.as_ref(), &cell, &context)).await;
        }
        return reconcile_action_cancellation(action.as_ref(), &context).await;
    }
    if start_deadline_passed(
        action.as_ref(),
        now_unix(),
        i64::from(context.limits.start_deadline_seconds),
    ) && let Some(next) = expire_unstarted_action(action.as_ref(), &context).await?
    {
        return Ok(next);
    }
    let cell = action_cell_source(&action, &control_namespace, &context).await?;
    if action
        .annotations()
        .contains_key(workspace_bridge::PARENT_ANNOTATION)
        && action
            .status
            .as_ref()
            .is_none_or(|status| status.native_execution.is_none())
        && !workspace_bridge::admit_child(
            &action,
            &context
                .revisions
                .loaded(&context.client, cell.clone())
                .await?,
            &context,
        )
        .await?
    {
        return patch_action_failure(
            &action,
            &context,
            "workspace_control_closed",
            "task control is no longer active; effects already applied are not rolled back",
        )
        .await;
    }
    if action
        .annotations()
        .get("proofstorm.dev/action-revision")
        .is_none_or(String::is_empty)
        && cell
            .annotations()
            .get("proofstorm.dev/desired-generation")
            .is_some_and(|g| g != "1")
    {
        return patch_invalid_action(&action, &context, "Operation does not identify its configuration revision. Reconnect the MCP server and submit a new operation.").await;
    }
    let cell = cell_updates::action_cell(&action, cell, &context).await?;
    // Existing native handles only reconcile owned work; changed readiness must
    // not discard a completed native receipt or turn collection into a new start.
    if matches!(action.spec.action, CellAction::ComponentExecLive(_))
        && action
            .status
            .as_ref()
            .is_some_and(|status| status.native_execution.is_some())
    {
        return Box::pin(native_exec::reconcile(action.as_ref(), &cell, &context)).await;
    }
    if !action
        .status
        .as_ref()
        .is_some_and(|status| status.phase == ActionPhase::Running)
        && private_transfer::validate_delegated_action(action.as_ref(), &cell).is_err()
    {
        return patch_action_failure(
            action.as_ref(),
            &context,
            "private_access_refused",
            "private access unavailable or action outside its scope; no new command started",
        )
        .await;
    }
    if let Err(error) = evaluate_action_admission(action.as_ref(), &cell) {
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Failed,
                observed_generation: action.metadata.generation,
                completed_at_unix: Some(now_unix()),
                error: Some(action_admission_failure(&error)),
                ..ProofstormCellActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::await_change());
    }
    if matches!(
        action.spec.action,
        CellAction::NodeStart(_)
            | CellAction::NodeStop(_)
            | CellAction::NodeRestart(_)
            | CellAction::ComponentStart(_)
            | CellAction::ComponentStop(_)
            | CellAction::ComponentRestart(_)
    ) {
        return component_lifecycle::reconcile(action.as_ref(), &cell, &context).await;
    }
    if matches!(
        action.spec.action,
        CellAction::NetworkPartition(_) | CellAction::NetworkHeal(_)
    ) {
        return reconcile_network_fault(action.as_ref(), &cell, &context).await;
    }
    if matches!(action.spec.action, CellAction::ComponentLogs(_)) {
        return reconcile_component_logs(action.as_ref(), &cell, &context).await;
    }
    if matches!(action.spec.action, CellAction::PrivateTransfer(_)) {
        return private_transfer::reconcile(action.as_ref(), &context).await;
    }
    if matches!(action.spec.action, CellAction::ComponentExecLive(_)) {
        return Box::pin(reconcile_component_exec_live(
            action.as_ref(),
            &cell,
            &context,
        ))
        .await;
    }
    let mut job = match render_cell_action_job(action.as_ref(), &cell) {
        Ok(job) => job,
        Err(error) => {
            patch_action_status(
                action.as_ref(),
                &context,
                ProofstormCellActionStatus {
                    phase: ActionPhase::Failed,
                    observed_generation: action.metadata.generation,
                    completed_at_unix: Some(now_unix()),
                    error: Some(status_object(serde_json::json!({
                        "code": "invalid_action",
                        "message": error.to_string(),
                    }))),
                    ..ProofstormCellActionStatus::default()
                },
            )
            .await?;
            return Ok(Action::await_change());
        }
    };
    if let Some(pod) = job
        .spec
        .as_mut()
        .and_then(|spec| spec.template.spec.as_mut())
    {
        proofstorm_kube::drivers::bind_image(pod, &context.probes.image);
    }
    let instance_namespace = instance_namespace(&action.spec.instance_key);
    let jobs = Api::<Job>::namespaced(context.client.clone(), &instance_namespace);
    let name = action.name_any();
    let Some(observed) = jobs.get_opt(&name).await? else {
        if action_execution_started(action.status.as_ref()) {
            patch_action_status(
                action.as_ref(),
                &context,
                lost_action_job_status(action.as_ref(), name, now_unix()),
            )
            .await?;
            return Ok(Action::await_change());
        }
        jobs.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&job),
        )
        .await?;
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix: Some(now_unix()),
                ..ProofstormCellActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(2)));
    };

    let job_status = observed.status.unwrap_or_default();
    let succeeded = job_status.succeeded.unwrap_or_default() > 0;
    let failed = job_status.failed.unwrap_or_default() > 0
        || job_status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Failed" && condition.status == "True")
        });
    if !succeeded && !failed {
        let started = action
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix)
            .unwrap_or_else(now_unix);
        patch_action_status(
            action.as_ref(),
            &context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix: Some(started),
                ..ProofstormCellActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(2)));
    }

    let pod_api = Api::<Pod>::namespaced(context.client.clone(), &instance_namespace);
    let pods = pod_api
        .list(&ListParams::default().labels(&format!("job-name={name}")))
        .await?;
    let started_at_unix = action
        .status
        .as_ref()
        .and_then(|status| status.started_at_unix);
    let status = if failed {
        ProofstormCellActionStatus {
            phase: ActionPhase::Failed,
            observed_generation: action.metadata.generation,
            job_name: Some(name),
            started_at_unix,
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(action_failure(&job_status, &pods.items))),
            ..ProofstormCellActionStatus::default()
        }
    } else {
        let artifact = if matches!(action.spec.action, CellAction::ComponentForensics(_)) {
            let endpoint_slices =
                Api::<EndpointSlice>::namespaced(context.client.clone(), &instance_namespace);
            native_exec_artifact(&pod_api, &endpoint_slices, action.as_ref(), &pods.items).await?
        } else if matches!(action.spec.action, CellAction::AuthenticationConformance(_)) {
            authentication_conformance_artifact(action.as_ref(), &cell, &pods.items)
        } else if matches!(
            action.spec.action,
            CellAction::AuthenticationProtectedSpend(_)
        ) {
            authentication_protected_spend_artifact(action.as_ref(), &pods.items, &context).await?
        } else if matches!(action.spec.action, CellAction::AuthenticationReplay(_)) {
            authentication_replay_artifact(action.as_ref(), &cell, &pods.items)
        } else {
            pods.items
                .iter()
                .find_map(|pod| {
                    termination_message(pod, action_result_container(&action.spec.action))
                })
                .and_then(|message| serde_json::from_str(&message).ok())
        };
        match artifact {
            Some(artifact) => ProofstormCellActionStatus {
                phase: ActionPhase::Succeeded,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                artifact: Some(artifact),
                ..ProofstormCellActionStatus::default()
            },
            None => ProofstormCellActionStatus {
                phase: ActionPhase::Failed,
                observed_generation: action.metadata.generation,
                job_name: Some(name),
                started_at_unix,
                completed_at_unix: Some(now_unix()),
                error: Some(status_object(
                    serde_json::json!({"code": "terminal_artifact_missing"}),
                )),
                ..ProofstormCellActionStatus::default()
            },
        }
    };
    patch_action_status(action.as_ref(), &context, status).await?;
    Ok(Action::await_change())
}

/// Read a component's startup or application logs and journal their availability.
///
/// Cell workloads hold no Kubernetes credentials by design, so no Job can read
/// another Pod's log. The controller already holds that authority for native
/// execution and uses it here, which also keeps the log readable while the
/// component is unready or stopped.
async fn reconcile_component_logs(
    action: &ProofstormCellAction,
    cell: &LoadedCell,
    context: &Context,
) -> Result<Action, Error> {
    if action.spec.capability != proofstorm_core::Capability::ComponentLogs {
        return patch_invalid_action(action, context, "component logs require component.logs")
            .await;
    }
    let CellAction::ComponentLogs(request) = &action.spec.action else {
        return Err(Error::ControllerInvariant("expected component logs action"));
    };
    let plans = cell.plans()?;
    if !plans
        .iter()
        .any(|plan| plan.component_id == request.component)
    {
        return patch_invalid_action(action, context, "component is not in the immutable cell")
            .await;
    }

    let pods = Api::<Pod>::namespaced(
        context.client.clone(),
        &instance_namespace(&action.spec.instance_key),
    );
    let matching = pods
        .list(&ListParams::default().labels(&format!(
            "{COMPONENT_LABEL}={},{INSTANCE_LABEL}={}",
            request.component, cell.spec.instance_key
        )))
        .await?;
    let artifact = component_logs::component_log_artifact(
        &pods,
        &request.component,
        request.tail_lines,
        matching.items,
    )
    .await?;

    let observed = now_unix();
    patch_action_status(
        action,
        context,
        ProofstormCellActionStatus {
            phase: ActionPhase::Succeeded,
            observed_generation: action.metadata.generation,
            started_at_unix: Some(observed),
            completed_at_unix: Some(observed),
            artifact: Some(artifact),
            ..ProofstormCellActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

mod management_tls;
mod native_exec;
mod private_transfer;
const LIVE_EXEC_OUTPUT_LIMIT: usize = 256 * 1024;

async fn reconcile_component_exec_live(
    action: &ProofstormCellAction,
    cell: &LoadedCell,
    context: &Context,
) -> Result<Action, Error> {
    Box::pin(native_exec::reconcile(action, cell, context)).await
}

async fn read_bounded_output(reader: impl AsyncRead + Unpin) -> Result<(Vec<u8>, bool), Error> {
    let mut bytes = Vec::new();
    reader
        .take((LIVE_EXEC_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| Error::LiveExec(error.to_string()))?;
    let truncated = bytes.len() > LIVE_EXEC_OUTPUT_LIMIT;
    bytes.truncate(LIVE_EXEC_OUTPUT_LIMIT);
    Ok((bytes, truncated))
}

fn exec_exit_code(status: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Status>) -> i32 {
    if status.and_then(|status| status.status.as_deref()) == Some("Success") {
        return 0;
    }
    status
        .and_then(|status| status.details.as_ref())
        .and_then(|details| details.causes.as_ref())
        .and_then(|causes| causes.iter().find_map(|cause| cause.message.as_deref()))
        .and_then(|message| message.split_whitespace().last())
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

#[allow(
    clippy::too_many_lines,
    reason = "network fault reconciliation keeps journal validation, policy application, and terminal proof contiguous"
)]
async fn reconcile_network_fault(
    action: &ProofstormCellAction,
    cell: &LoadedCell,
    context: &Context,
) -> Result<Action, Error> {
    let expected_capability = match action.spec.action {
        CellAction::NetworkPartition(_) => proofstorm_core::Capability::NetworkPartition,
        CellAction::NetworkHeal(_) => proofstorm_core::Capability::NetworkHeal,
        _ => return Err(Error::ControllerInvariant("expected network fault action")),
    };
    if action.spec.capability != expected_capability {
        return patch_invalid_action(action, context, "network fault capability mismatch").await;
    }
    if action.spec.cell_name != cell.name_any()
        || action.spec.workspace_id != cell.spec.workspace_id
        || action.spec.instance_id != cell.spec.instance_id
        || action.spec.instance_key != cell.spec.instance_key
    {
        return patch_invalid_action(action, context, "network fault identity mismatch").await;
    }

    let actions = list_instance_actions(action, context).await?;
    let (from_component, to_component, healed) = match &action.spec.action {
        CellAction::NetworkPartition(request) => {
            if request.from_component == request.to_component
                || !cell
                    .revision
                    .cell
                    .components
                    .iter()
                    .any(|component| component.id == request.from_component)
                || !cell
                    .revision
                    .cell
                    .components
                    .iter()
                    .any(|component| component.id == request.to_component)
            {
                return patch_invalid_action(
                    action,
                    context,
                    "partition endpoints must be distinct components in the immutable cell",
                )
                .await;
            }
            (
                request.from_component.clone(),
                request.to_component.clone(),
                false,
            )
        }
        CellAction::NetworkHeal(request) => {
            let partition = actions.iter().find(|candidate| {
                candidate.spec.sequence < action.spec.sequence
                    && candidate.spec.operation_id == request.partition_operation_id
                    && candidate
                        .status
                        .as_ref()
                        .is_some_and(|status| status.phase == ActionPhase::Succeeded)
                    && matches!(candidate.spec.action, CellAction::NetworkPartition(_))
            });
            let Some(partition) = partition else {
                return patch_invalid_action(
                    action,
                    context,
                    "heal requires a prior succeeded partition in this cell",
                )
                .await;
            };
            let CellAction::NetworkPartition(partition) = &partition.spec.action else {
                unreachable!();
            };
            (
                partition.from_component.clone(),
                partition.to_component.clone(),
                true,
            )
        }
        _ => unreachable!(),
    };

    if action.status.is_none() {
        patch_action_status(
            action,
            context,
            ProofstormCellActionStatus {
                phase: ActionPhase::Running,
                observed_generation: action.metadata.generation,
                started_at_unix: Some(now_unix()),
                ..ProofstormCellActionStatus::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let active = apply_network_fault_policies(cell, Some(action.spec.sequence), context).await?;
    let partition_operation_id = match &action.spec.action {
        CellAction::NetworkPartition(_) => &action.spec.operation_id,
        CellAction::NetworkHeal(request) => &request.partition_operation_id,
        _ => unreachable!(),
    };
    let expected_active = !healed;
    if active.contains_key(partition_operation_id) != expected_active {
        return patch_action_failure(
            action,
            context,
            "network_fault_state_conflict",
            "ordered network fault state does not match the requested transition",
        )
        .await;
    }
    patch_action_status(
        action,
        context,
        ProofstormCellActionStatus {
            phase: ActionPhase::Succeeded,
            observed_generation: action.metadata.generation,
            started_at_unix: action
                .status
                .as_ref()
                .and_then(|status| status.started_at_unix),
            completed_at_unix: Some(now_unix()),
            artifact: Some(status_object(serde_json::json!({
                "partition_operation_id": partition_operation_id,
                "from_component": from_component,
                "to_component": to_component,
                "partitioned": !healed,
                "healed": healed,
                "active_partition_count": active.len(),
                "sequence": action.spec.sequence,
            }))),
            ..ProofstormCellActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

async fn list_instance_actions(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Vec<ProofstormCellAction>, Error> {
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    Ok(
        Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace)
            .list(&ListParams::default().labels(&format!(
                "proofstorm.dev/instance={}",
                action.spec.instance_key
            )))
            .await?
            .items,
    )
}

fn active_network_partitions(
    actions: &[ProofstormCellAction],
    maximum_sequence: Option<u64>,
) -> BTreeMap<String, (String, String)> {
    let mut ordered = actions.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|action| action.spec.sequence);
    let mut active = BTreeMap::new();
    for action in ordered {
        if workspace_faults::is_owned_network(action) {
            if !workspace_faults::is_active(action, now_unix()) {
                continue;
            }
        } else if maximum_sequence.is_some_and(|maximum| action.spec.sequence > maximum)
            || action.status.as_ref().is_some_and(|status| {
                matches!(status.phase, ActionPhase::Failed | ActionPhase::Cancelled)
            })
        {
            continue;
        }
        match &action.spec.action {
            CellAction::NetworkPartition(request) => {
                active.insert(
                    action.spec.operation_id.clone(),
                    (request.from_component.clone(), request.to_component.clone()),
                );
            }
            CellAction::NetworkHeal(request) => {
                active.remove(&request.partition_operation_id);
            }
            _ => {}
        }
    }
    active
}

/// One policy writer per cell; cells never wait on each other's partitions.
fn cell_policy_lock(instance_key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static WRITERS: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>> =
        std::sync::Mutex::new(BTreeMap::new());
    WRITERS
        .lock()
        .expect("policy writer registry")
        .entry(instance_key.to_owned())
        .or_default()
        .clone()
}

/// Bring a cell's network policies in line with its active partitions: one
/// shared policy for every component outside a partition, and a policy of its
/// own for each partitioned component. Returns the active partitions.
async fn apply_network_fault_policies(
    cell: &LoadedCell,
    maximum_sequence: Option<u64>,
    context: &Context,
) -> Result<BTreeMap<String, (String, String)>, Error> {
    // All of a cell's policy writers read the journal under this lock. An older
    // queued write cannot resurrect a lease after cleanup has reconciled its
    // release marker.
    let lock = cell_policy_lock(&cell.spec.instance_key);
    let _writer = lock.lock().await;
    let policies = Api::<NetworkPolicy>::namespaced(
        context.client.clone(),
        &instance_namespace(&cell.spec.instance_key),
    );
    // Read policy versions BEFORE the action journal. Across controller restarts,
    // a delayed old write must conflict with any subsequently completed cleanup.
    let existing = policies
        .list(
            &ListParams::default().labels(&format!("{INSTANCE_LABEL}={}", cell.spec.instance_key)),
        )
        .await?
        .items
        .into_iter()
        .map(|policy| (policy.name_any(), policy))
        .collect::<BTreeMap<_, _>>();
    let namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let actions = Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace)
        .list(&ListParams::default().labels(&format!(
            "proofstorm.dev/instance={}",
            cell.spec.instance_key
        )))
        .await?
        .items;
    let active = active_network_partitions(&actions, maximum_sequence);
    let components = cell
        .revision
        .cell
        .components
        .iter()
        .map(|component| component.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut exclusions = BTreeMap::<String, BTreeSet<String>>::new();
    for (from, to) in active.values() {
        for (component, peer) in [(from, to), (to, from)] {
            if components.contains(component.as_str()) {
                exclusions
                    .entry(component.clone())
                    .or_default()
                    .insert(peer.clone());
            }
        }
    }
    let invalid = |_| Error::ControllerInvariant("network policy did not serialize");
    // A pod no allow-policy selects is cut off from everything, so a component
    // gets its own policy before the shared one stops selecting it, and keeps it
    // until the shared one selects it again.
    for (component, peers) in &exclusions {
        let peers = peers.iter().cloned().collect::<Vec<_>>();
        let policy = render_component_network_policy(&cell.spec.instance_key, component, &peers)
            .map_err(invalid)?;
        write_policy(&policies, existing.get(component), policy).await?;
    }
    let partitioned = exclusions.keys().cloned().collect::<Vec<_>>();
    let shared =
        render_cell_network_policy(&cell.spec.instance_key, &partitioned).map_err(invalid)?;
    write_policy(&policies, existing.get(CELL_NETWORK_POLICY), shared).await?;
    // Healed components, removed components and the per-component policies of
    // earlier releases: a component policy is named after its component.
    for (name, policy) in &existing {
        let own = policy.labels().get(COMPONENT_LABEL) == Some(name);
        if own && !exclusions.contains_key(name) {
            gone(
                policies
                    .delete(
                        name,
                        &DeleteParams {
                            preconditions: Some(kube::api::Preconditions {
                                uid: policy.metadata.uid.clone(),
                                resource_version: policy.metadata.resource_version.clone(),
                            }),
                            ..DeleteParams::default()
                        },
                    )
                    .await,
            )?;
        }
    }
    Ok(active)
}

/// Create `desired`, or update `current` fenced by the version read before the
/// journal. An unchanged policy is left alone.
async fn write_policy(
    policies: &Api<NetworkPolicy>,
    current: Option<&NetworkPolicy>,
    mut desired: NetworkPolicy,
) -> Result<(), Error> {
    let Some(current) = current else {
        policies
            .create(&kube::api::PostParams::default(), &desired)
            .await?;
        return Ok(());
    };
    if current.spec == desired.spec && current.labels() == desired.labels() {
        return Ok(());
    }
    desired
        .metadata
        .resource_version
        .clone_from(&current.metadata.resource_version);
    policies
        .patch(
            &desired.name_any(),
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&desired),
        )
        .await?;
    Ok(())
}

async fn patch_invalid_action(
    action: &ProofstormCellAction,
    context: &Context,
    message: &str,
) -> Result<Action, Error> {
    patch_action_failure(action, context, "invalid_action", message).await
}

async fn patch_action_failure(
    action: &ProofstormCellAction,
    context: &Context,
    code: &str,
    message: &str,
) -> Result<Action, Error> {
    patch_action_status(
        action,
        context,
        ProofstormCellActionStatus {
            phase: ActionPhase::Failed,
            observed_generation: action.metadata.generation,
            started_at_unix: action
                .status
                .as_ref()
                .and_then(|status| status.started_at_unix),
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(
                serde_json::json!({"code": code, "message": message}),
            )),
            ..ProofstormCellActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

const fn is_terminal_action(phase: ActionPhase) -> bool {
    matches!(
        phase,
        ActionPhase::Succeeded | ActionPhase::Failed | ActionPhase::Cancelled
    )
}

/// When a finished action may be deleted. Only one-off actions qualify: nothing
/// replays them later. Network faults, component controls, workspace task records
/// and private custody stay for the life of the cell.
fn finished_action_expiry(action: &ProofstormCellAction) -> Option<i64> {
    let one_off = match &action.spec.action {
        CellAction::ComponentExecLive(request) => {
            request.private_payload.is_none()
                && action.spec.access_scope.is_none()
                && !workspace_bridge::is_controlled_start(action)
                && !action
                    .annotations()
                    .contains_key(workspace_bridge::PARENT_ANNOTATION)
        }
        CellAction::ComponentLogs(_)
        | CellAction::ComponentForensics(_)
        | CellAction::ReachabilityOracle(_)
        | CellAction::AuthenticationConformance(_)
        | CellAction::AuthenticationProtectedSpend(_)
        | CellAction::AuthenticationReplay(_) => true,
        _ => false,
    };
    let status = action.status.as_ref()?;
    (one_off && is_terminal_action(status.phase)).then(|| {
        status
            .completed_at_unix
            .unwrap_or(action.spec.accepted_at_unix)
            + FINISHED_ACTION_RETENTION_SECONDS
    })
}

async fn expire_finished_action(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Action, Error> {
    let Some(expiry) = finished_action_expiry(action) else {
        return Ok(Action::await_change());
    };
    let remaining = expiry - now_unix();
    if remaining > 0 {
        return Ok(Action::requeue(Duration::from_secs(
            remaining.unsigned_abs(),
        )));
    }
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    match Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace)
        .delete(&action.name_any(), &DeleteParams::default())
        .await
    {
        Ok(_) => Ok(Action::await_change()),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(Action::await_change()),
        Err(error) => Err(error.into()),
    }
}

/// Whether an action that never started is past its start deadline. Only the
/// one-off reads and commands a caller waits on qualify. Component controls,
/// network faults and private transfers keep their own lifecycles.
///
/// The clock starts at the later of host acceptance and the API server's
/// creation time. An upload, or an idempotent retry of an old request, can
/// create the action well after acceptance. The creation time is also on the
/// controller's own clock.
fn start_deadline_passed(
    action: &ProofstormCellAction,
    now_unix: i64,
    deadline_seconds: i64,
) -> bool {
    let one_off = matches!(
        action.spec.action,
        CellAction::ComponentExecLive(_)
            | CellAction::ComponentLogs(_)
            | CellAction::ComponentForensics(_)
            | CellAction::ReachabilityOracle(_)
            | CellAction::AuthenticationConformance(_)
            | CellAction::AuthenticationProtectedSpend(_)
            | CellAction::AuthenticationReplay(_)
    );
    let created = action
        .metadata
        .creation_timestamp
        .as_ref()
        .map_or(i64::MIN, |time| time.0.as_second());
    one_off
        && !action_execution_started(action.status.as_ref())
        && now_unix > action.spec.accepted_at_unix.max(created) + deadline_seconds
}

/// Fail an action whose start deadline passed, unless it may in fact have
/// started. Returns `None` when the action must reconcile normally instead.
///
/// - A Job-backed action whose Job exists has started even if its status write
///   was lost, so the normal path observes the Job.
/// - The failure is fenced on the observed resource version, like the native
///   start claim. A reconcile reading a stale cache must not overwrite a start
///   it hasn't seen yet.
async fn expire_unstarted_action(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Option<Action>, Error> {
    let jobs = Api::<Job>::namespaced(
        context.client.clone(),
        &instance_namespace(&action.spec.instance_key),
    );
    if jobs.get_opt(&action.name_any()).await?.is_some() {
        return Ok(None);
    }
    let Some(version) = action.resource_version() else {
        return Ok(None);
    };
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    let status = ProofstormCellActionStatus {
        phase: ActionPhase::Failed,
        observed_generation: action.metadata.generation,
        completed_at_unix: Some(now_unix()),
        error: Some(status_object(serde_json::json!({
            "code": "action_not_started",
            "message": format!("the runtime could not start this operation within {} seconds of acceptance; nothing was run; submit a new operation if it is still wanted", context.limits.start_deadline_seconds),
        }))),
        ..ProofstormCellActionStatus::default()
    };
    match Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace)
        .patch_status(
            &action.name_any(),
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "metadata": {"resourceVersion": version},
                "status": status,
            })),
        )
        .await
    {
        Ok(_) => Ok(Some(Action::await_change())),
        // The action changed since this reconcile read it: look again.
        Err(kube::Error::Api(error)) if error.code == 409 => {
            Ok(Some(Action::requeue(Duration::from_secs(1))))
        }
        Err(error) => Err(error.into()),
    }
}

fn action_execution_started(status: Option<&ProofstormCellActionStatus>) -> bool {
    status.is_some_and(|status| {
        status.phase == ActionPhase::Running
            || status.job_name.is_some()
            || status.started_at_unix.is_some()
    })
}

fn lost_action_job_status(
    action: &ProofstormCellAction,
    job_name: String,
    completed_at_unix: i64,
) -> ProofstormCellActionStatus {
    ProofstormCellActionStatus {
        phase: ActionPhase::Failed,
        observed_generation: action.metadata.generation,
        job_name: action
            .status
            .as_ref()
            .and_then(|status| status.job_name.clone())
            .or(Some(job_name)),
        started_at_unix: action
            .status
            .as_ref()
            .and_then(|status| status.started_at_unix),
        completed_at_unix: Some(completed_at_unix),
        error: Some(status_object(serde_json::json!({
            "code": "action_job_lost",
            "message": "controller-owned Job disappeared after execution began; automatic replay refused",
        }))),
        ..ProofstormCellActionStatus::default()
    }
}

async fn reconcile_action_cancellation(
    action: &ProofstormCellAction,
    context: &Context,
) -> Result<Action, Error> {
    if matches!(
        action.spec.action,
        CellAction::NodeStart(_)
            | CellAction::NodeStop(_)
            | CellAction::NodeRestart(_)
            | CellAction::ComponentStart(_)
            | CellAction::ComponentStop(_)
            | CellAction::ComponentRestart(_)
            | CellAction::ComponentExecLive(_)
            | CellAction::NetworkPartition(_)
            | CellAction::NetworkHeal(_)
    ) && action_execution_started(action.status.as_ref())
    {
        return patch_action_failure(
            action,
            context,
            "direct_action_cancellation_inconclusive",
            "direct controller execution began before cancellation; inspect current state before issuing another action",
        )
        .await;
    }
    let instance_namespace = instance_namespace(&action.spec.instance_key);
    let jobs = Api::<Job>::namespaced(context.client.clone(), &instance_namespace);
    let name = action.name_any();
    if jobs.get_opt(&name).await?.is_some() {
        jobs.delete(
            &name,
            &DeleteParams {
                propagation_policy: Some(PropagationPolicy::Foreground),
                ..DeleteParams::default()
            },
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let started_at_unix = action
        .status
        .as_ref()
        .and_then(|status| status.started_at_unix);
    patch_action_status(
        action,
        context,
        ProofstormCellActionStatus {
            phase: ActionPhase::Cancelled,
            observed_generation: action.metadata.generation,
            job_name: action
                .status
                .as_ref()
                .and_then(|status| status.job_name.clone()),
            started_at_unix,
            completed_at_unix: Some(now_unix()),
            error: Some(status_object(
                serde_json::json!({"code": "action_cancelled"}),
            )),
            ..ProofstormCellActionStatus::default()
        },
    )
    .await?;
    Ok(Action::await_change())
}

fn action_failure(status: &JobStatus, pods: &[Pod]) -> serde_json::Value {
    if let Some(failure) = pods
        .iter()
        .filter_map(container_failure)
        .find(|failure| failure.get("stage").is_some())
    {
        return failure;
    }
    if status.conditions.as_ref().is_some_and(|conditions| {
        conditions.iter().any(|condition| {
            condition.status == "True" && condition.reason.as_deref() == Some("DeadlineExceeded")
        })
    }) {
        return serde_json::json!({"code": "action_deadline_exceeded"});
    }
    pods.iter()
        .find_map(container_failure)
        .unwrap_or_else(|| serde_json::json!({"code": "action_failed"}))
}

fn action_admission_failure(error: &ActionAdmissionError) -> BTreeMap<String, serde_json::Value> {
    let mut value = serde_json::json!({
        "code": error.code(),
        "message": error.to_string(),
    });
    if let ActionAdmissionError::PrerequisiteUnsatisfied {
        component,
        operation,
        prerequisite,
        condition,
        state,
        reason,
    } = error
    {
        value["component"] = serde_json::json!(component);
        value["operation"] = serde_json::json!(operation);
        value["prerequisite"] = serde_json::json!(prerequisite);
        if let Some(condition) = condition {
            value["condition"] = serde_json::json!(condition);
        }
        if let Some(state) = state {
            value["state"] = serde_json::json!(state);
        }
        if let Some(reason) = reason {
            value["reason"] = serde_json::json!(reason);
        }
    }
    status_object(value)
}

async fn patch_action_status(
    action: &ProofstormCellAction,
    context: &Context,
    status: ProofstormCellActionStatus,
) -> Result<(), Error> {
    if action.status.as_ref() == Some(&status) {
        return Ok(());
    }
    enforce_status_budget("ProofstormCellAction", &status, MAX_ACTION_STATUS_BYTES)?;
    let namespace = action
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(action.name_any()))?;
    let actions = Api::<ProofstormCellAction>::namespaced(context.client.clone(), &namespace);
    let patch = serde_json::json!({
        "apiVersion": "proofstorm.dev/v1alpha1",
        "kind": "ProofstormCellAction",
        "status": status,
    });
    actions
        .patch_status(
            &action.name_any(),
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
    Ok(())
}

async fn reconcile(cell: Arc<ProofstormCell>, context: Arc<Context>) -> Result<Action, Error> {
    let namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let cells = Api::<ProofstormCell>::namespaced(context.client.clone(), &namespace);
    let Some(cell) = with_finalizer(&cells, cell).await? else {
        return Ok(Action::await_change());
    };
    finalizer(&cells, FINALIZER, cell, |event| async {
        match event {
            Event::Apply(cell) => {
                let result = match context
                    .revisions
                    .loaded(&context.client, (*cell).clone())
                    .await
                {
                    Ok(loaded) => {
                        let loaded = Arc::new(loaded);
                        if let Some(applied) = context.probes.applied(&loaded) {
                            observe_cell(loaded, &context, &applied).await
                        } else {
                            apply(loaded, &context).await
                        }
                    }
                    Err(error) => Err(error.into()),
                };
                if let Err(error) = &result
                    && let Some(status) = reconciliation_failure_status(&cell, error)
                {
                    context.probes.remove(&cell.spec.instance_key);
                    patch_status(&cell, &context, status).await?;
                }
                result
            }
            Event::Cleanup(cell) => cleanup(cell, &context).await,
        }
    })
    .await
    .map_err(|error| match error {
        kube::runtime::finalizer::Error::ApplyFailed(error)
        | kube::runtime::finalizer::Error::CleanupFailed(error) => error,
        kube::runtime::finalizer::Error::AddFinalizer(error)
        | kube::runtime::finalizer::Error::RemoveFinalizer(error) => Error::Kube(error),
        kube::runtime::finalizer::Error::UnnamedObject => {
            Error::ControllerInvariant("finalized object has no name")
        }
        kube::runtime::finalizer::Error::InvalidFinalizer => {
            Error::ControllerInvariant("invalid finalizer JSON patch")
        }
    })
}

/// The cell with the cleanup finalizer, so its first apply happens in this
/// reconcile. kube's helper adds the finalizer and waits for the watch to
/// report it, and a late watch event held new cells back for about five
/// minutes, until the watch relisted. The cached copy may be behind too, so
/// read the live cell before adding it. `None` once the cell is gone.
async fn with_finalizer(
    cells: &Api<ProofstormCell>,
    cell: Arc<ProofstormCell>,
) -> Result<Option<Arc<ProofstormCell>>, Error> {
    if finalizer_patch(&cell).is_none() {
        return Ok(Some(cell));
    }
    let Some(live) = cells.get_opt(&cell.name_any()).await? else {
        return Ok(None);
    };
    let Some(patch) = finalizer_patch(&live) else {
        return Ok(Some(Arc::new(live)));
    };
    let patched = cells
        .patch(
            &live.name_any(),
            &PatchParams::default(),
            &Patch::Merge(patch),
        )
        .await?;
    Ok(Some(Arc::new(patched)))
}

/// The merge patch that adds the cleanup finalizer to a live cell, fenced on
/// its resource version; `None` when it has the finalizer or is being deleted.
fn finalizer_patch(cell: &ProofstormCell) -> Option<serde_json::Value> {
    if cell.metadata.deletion_timestamp.is_some()
        || cell.finalizers().iter().any(|name| name == FINALIZER)
    {
        return None;
    }
    let mut finalizers = cell.finalizers().to_vec();
    finalizers.push(FINALIZER.to_owned());
    Some(serde_json::json!({"metadata": {
        "resourceVersion": cell.resource_version(),
        "finalizers": finalizers,
    }}))
}

fn reconciliation_failure_status(
    cell: &ProofstormCell,
    error: &Error,
) -> Option<ProofstormCellStatus> {
    let message = match error {
        Error::Adapter(error) => format!("Component rendering blocked: {error}"),
        Error::Kube(kube::Error::Api(error)) if matches!(error.code, 400 | 403 | 413 | 422) => {
            format!(
                "Reconciliation blocked (Kubernetes {}): {}",
                error.code, error.message
            )
        }
        Error::LiveExec(_)
        | Error::SecretContract(_)
        | Error::InvalidInstanceKey(_)
        | Error::ControllerInvariant(_) => format!("Reconciliation blocked: {error}"),
        _ => return None,
    };
    let mut status = cell.status.clone().unwrap_or_default();
    status.phase = CellPhase::Blocked;
    status.message = Some(message.chars().take(1024).collect());
    status.observed_generation = cell.metadata.generation;
    status
        .observed_revision_digest
        .clone_from(&cell.spec.revision_digest);
    status.observed_desired_generation = cell
        .annotations()
        .get("proofstorm.dev/desired-generation")
        .and_then(|g| g.parse().ok())
        .unwrap_or(1);
    // These observations may belong to an older revision. Preserve inventory
    // for cleanup, but never present old readiness as evidence for this failure.
    status.components.clear();
    status.protocol_freshness = None;
    Some(status)
}

#[allow(
    clippy::too_many_lines,
    reason = "one reconciliation pass visibly applies the complete bounded instance inventory"
)]
async fn apply(cell: Arc<LoadedCell>, context: &Context) -> Result<Action, Error> {
    validate_instance_key(&cell.spec.instance_key)?;
    private_transfer::expire(&cell)?;
    let previous = skippable(
        context.probes.last_applied(&cell),
        &probes::signature(&cell),
    );
    let mut documents = BTreeMap::new();
    let mut workloads = render_cell(
        &cell.spec.instance_key,
        &cell.spec.revision_digest,
        &cell.revision.cell,
        &cell.revision.lock,
    )?;
    for deployment in &mut workloads.deployments {
        if let Some(pod) = deployment
            .spec
            .as_mut()
            .and_then(|spec| spec.template.spec.as_mut())
        {
            proofstorm_kube::drivers::bind_image(pod, &context.probes.image);
        }
    }
    for stateful in &mut workloads.stateful_sets {
        if let Some(pod) = stateful
            .spec
            .as_mut()
            .and_then(|spec| spec.template.spec.as_mut())
        {
            proofstorm_kube::drivers::bind_image(pod, &context.probes.image);
        }
    }
    if let Some(worker) =
        proofstorm_kube::render_protocol_prober(&workloads.plans, &context.probes.image)?
    {
        workloads.deployments.push(worker);
    }
    context
        .probes
        .register(&cell, Arc::new(workloads.plans.clone()))?;
    // Persist the intended inventory before the first component write. If the process
    // stops halfway through, the next edit can still identify and prune partial additions.
    let desired_generation = cell
        .annotations()
        .get("proofstorm.dev/desired-generation")
        .and_then(|g| g.parse().ok())
        .unwrap_or(1);
    let mut cell = cell.as_ref().clone();
    if cell.status.as_ref().is_none_or(|s| {
        s.observed_revision_digest != cell.spec.revision_digest
            || s.observed_desired_generation != desired_generation
    }) {
        let mut status = cell.status.clone().unwrap_or_default();
        for resource in workloads.inventory() {
            if !status.inventory.contains(&resource) {
                status.inventory.push(resource);
            }
        }
        status.phase = CellPhase::Pending;
        patch_status(&cell, context, status.clone()).await?;
        cell.resource.status = Some(status);
    }
    let cell = Arc::new(cell);
    let rendered = render_security_spine(
        &cell.spec.instance_key,
        &cell.revision.cell.policy.resources,
    );
    let client = context.client.clone();
    let namespace_name = instance_namespace(&cell.spec.instance_key);
    let patch = PatchParams::apply(FIELD_MANAGER).force();

    Api::<Namespace>::all(client.clone())
        .patch(&namespace_name, &patch, &Patch::Apply(&rendered.namespace))
        .await?;
    let quotas = Api::<ResourceQuota>::namespaced(client.clone(), &namespace_name);
    if let Some(quota) = &rendered.quota {
        quotas
            .patch("proofstorm-instance-quota", &patch, &Patch::Apply(quota))
            .await?;
    } else {
        // A quota is the user's choice; remove one an earlier revision or version set.
        gone(
            quotas
                .delete("proofstorm-instance-quota", &DeleteParams::default())
                .await,
        )?;
    }
    Api::<LimitRange>::namespaced(client.clone(), &namespace_name)
        .patch(
            "proofstorm-container-limits",
            &patch,
            &Patch::Apply(&rendered.limits),
        )
        .await?;
    Api::<NetworkPolicy>::namespaced(client.clone(), &namespace_name)
        .patch(
            "default-deny-all",
            &patch,
            &Patch::Apply(&rendered.default_deny),
        )
        .await?;
    Api::<ServiceAccount>::namespaced(client.clone(), &namespace_name)
        .patch(
            "proofstorm-workload",
            &patch,
            &Patch::Apply(&rendered.service_account),
        )
        .await?;
    let client = context.client.clone();
    let secrets = Api::<Secret>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.secrets {
        let secret_kind = resource
            .string_data
            .as_ref()
            .and_then(|data| data.get("PROOFSTORM_SECRET_KIND"))
            .map(String::as_str);
        if unchanged(&mut documents, &previous, "Secret", resource) {
            continue;
        }
        match secret_kind {
            Some("bark-processor" | "open-ssp" | "spark-processor") => {
                let claims =
                    Api::<PersistentVolumeClaim>::namespaced(client.clone(), &namespace_name);
                generated_secrets::ensure_owned_identity(&secrets, &claims, resource).await?;
            }
            Some("mint-management-tls" | "payment-processor-tls") => {
                management_tls::ensure(&secrets, resource).await?;
            }
            Some("bark-cln-tls" | "bark-hold-tls") => {
                let claims: Api<PersistentVolumeClaim> =
                    Api::namespaced(client.clone(), &namespace_name);
                management_tls::ensure_bark(&secrets, &claims, resource).await?;
            }
            _ => generated_secrets::ensure(&secrets, resource).await?,
        }
    }
    let configs = Api::<ConfigMap>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.config_maps {
        if unchanged(&mut documents, &previous, "ConfigMap", resource) {
            continue;
        }
        configs
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let services = Api::<Service>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.services {
        if unchanged(&mut documents, &previous, "Service", resource) {
            continue;
        }
        services
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let claims = Api::<PersistentVolumeClaim>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.persistent_volume_claims {
        if unchanged(&mut documents, &previous, "PersistentVolumeClaim", resource) {
            continue;
        }
        claims
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    let stateful_sets = Api::<StatefulSet>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.stateful_sets {
        if unchanged(&mut documents, &previous, "StatefulSet", resource) {
            continue;
        }
        let resource = component_lifecycle::preserve(&stateful_sets, resource).await?;
        stateful_sets
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(&resource),
            )
            .await?;
    }
    let deployments = Api::<Deployment>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.deployments {
        if unchanged(&mut documents, &previous, "Deployment", resource) {
            continue;
        }
        let name = resource.metadata.name.as_deref().unwrap_or_default();
        clear_rolling_update_strategy(&deployments, resource).await?;
        let resource = component_lifecycle::preserve(&deployments, resource).await?;
        deployments
            .patch(
                name,
                &patch,
                &Patch::Apply(deployment_apply_document(&resource)),
            )
            .await?;
    }
    let policies = Api::<NetworkPolicy>::namespaced(client.clone(), &namespace_name);
    for resource in &workloads.network_policies {
        // The partition writer owns the shared component policy.
        if resource.metadata.name.as_deref() == Some(CELL_NETWORK_POLICY) {
            continue;
        }
        if unchanged(&mut documents, &previous, "NetworkPolicy", resource) {
            continue;
        }
        policies
            .patch(
                resource.metadata.name.as_deref().unwrap_or_default(),
                &patch,
                &Patch::Apply(resource),
            )
            .await?;
    }
    apply_network_fault_policies(&cell, None, context).await?;

    let (pruned, retained, retained_storage) = cell_updates::prune(&cell, context).await?;
    let mut inventory = workloads.inventory();
    inventory.extend(retained);
    let inventory_digest = proofstorm_core::digest_json(&inventory);
    let inventory_name = format!("proofstorm-inventory-{}", cell.spec.instance_key);
    let inventory_resource = serde_json::json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": inventory_name, "namespace": namespace_name,
            "labels": {"proofstorm.dev/instance": cell.spec.instance_key, "proofstorm.dev/inventory": "true"}},
        "data": {"inventory.json": serde_json::to_string(&inventory).expect("inventory serializes"),
            "inventoryDigest": inventory_digest}
    });
    let inventory_key = format!("Inventory/{inventory_name}");
    let inventory_document = proofstorm_core::digest_json(&inventory_resource);
    let inventory_unchanged = previous.get(&inventory_key) == Some(&inventory_document);
    documents.insert(inventory_key, inventory_document);
    if !inventory_unchanged
        && let Some(old) = configs.get_opt(&inventory_name).await?
        && old.immutable == Some(true)
        && old.labels().get("proofstorm.dev/instance") == Some(&cell.spec.instance_key)
    {
        configs
            .delete(
                &inventory_name,
                &DeleteParams {
                    preconditions: Some(kube::api::Preconditions {
                        uid: old.metadata.uid.clone(),
                        resource_version: old.metadata.resource_version.clone(),
                    }),
                    ..Default::default()
                },
            )
            .await?;
    }
    if !inventory_unchanged {
        configs
            .patch(&inventory_name, &patch, &Patch::Apply(inventory_resource))
            .await?;
    }

    let applied = probes::Applied {
        signature: probes::signature(&cell),
        at: std::time::Instant::now(),
        plans: Arc::new(workloads.plans),
        inventory,
        retained_storage,
        pruned,
        documents: Arc::new(documents),
    };
    context
        .probes
        .remember_applied(&cell.spec.instance_key, applied.clone());
    observe_cell(cell, context, &applied).await
}

/// The documents this apply may skip when unchanged. After a change, or while
/// a prune is still finishing, only changed documents are sent. The periodic
/// pass for an unchanged, fully pruned cell repairs drift, so it sends them all.
fn skippable(last: Option<probes::Applied>, signature: &str) -> Arc<BTreeMap<String, String>> {
    last.filter(|applied| applied.signature != signature || !applied.pruned)
        .map(|applied| applied.documents)
        .unwrap_or_default()
}

/// Record `resource`'s digest and report whether the previous apply sent the
/// same document, so this pass can skip it.
fn unchanged(
    documents: &mut BTreeMap<String, String>,
    previous: &BTreeMap<String, String>,
    kind: &str,
    resource: &(impl serde::Serialize + ResourceExt),
) -> bool {
    let key = format!("{kind}/{}", resource.name_any());
    let digest = proofstorm_core::digest_json(resource);
    let same = previous.get(&key) == Some(&digest);
    documents.insert(key, digest);
    same
}

async fn observe_cell(
    cell: Arc<LoadedCell>,
    context: &Context,
    applied: &probes::Applied,
) -> Result<Action, Error> {
    let namespace_name = instance_namespace(&cell.spec.instance_key);
    let snapshot = context.probes.snapshot(&cell.spec.instance_key);
    let observed_resources = snapshot.observed();
    let stored = stored_components(context, &cell.spec.instance_key).await?;
    // Statuses of an older revision or rollout never carry their condition
    // transitions over: the observation matches on both digests.
    let previous_components = stored
        .iter()
        .filter_map(|object| object.status.clone())
        .collect::<Vec<_>>();
    let stopped_components =
        component_lifecycle::observed_stops(&applied.plans, &observed_resources);
    let components = observe_component_statuses(
        &namespace_name,
        &applied.plans,
        &observed_resources,
        &previous_components,
        &stopped_components,
        now_unix(),
    );
    write_component_objects(&cell, context, &stored, &components).await?;
    let ready = applied.pruned
        && components
            .iter()
            .all(|component| component.ready || stopped_components.contains(&component.id));
    let blocked = components.iter().any(|component| {
        component.conditions.iter().any(|condition| {
            condition.state == proofstorm_core::ComponentConditionState::False
                && condition.reason.blocks_startup()
        })
    });
    let message = (!ready).then(|| if blocked {
        "component startup is blocked; inspect workload_ready conditions for error reasons and recovery steps".to_owned()
    } else {
        "waiting for protocol component readiness".to_owned()
    });

    patch_status(
        cell.as_ref(),
        context,
        ProofstormCellStatus {
            observed_desired_generation: cell
                .annotations()
                .get("proofstorm.dev/desired-generation")
                .and_then(|g| g.parse().ok())
                .unwrap_or(1),
            retained_storage: applied.retained_storage.clone(),
            last_converged_revision: if ready {
                Some(cell.spec.revision_digest.clone())
            } else {
                cell.status
                    .as_ref()
                    .and_then(|s| s.last_converged_revision.clone())
            },
            phase: if ready {
                CellPhase::Ready
            } else {
                CellPhase::Pending
            },
            instance_namespace: Some(namespace_name),
            observed_generation: cell.metadata.generation,
            observed_revision_digest: cell.spec.revision_digest.clone(),
            observed_protocol_probe_lease: None,
            protocol_freshness: proofstorm_kube::protocol_freshness(&components),
            components: Vec::new(),
            inventory: applied.inventory.clone(),
            inventory_digest: Some(proofstorm_core::digest_json(&applied.inventory)),
            teardown_receipt: None,
            message,
        },
    )
    .await?;
    Ok(if ready {
        jittered_requeue(&cell.spec.instance_key, 30, 10)
    } else {
        jittered_requeue(&cell.spec.instance_key, 3, 2)
    })
}

/// Apply cannot delete a defaulted field owned by another field manager. Clear
/// the old strategy atomically before applying the complete desired workload.
async fn clear_rolling_update_strategy(
    deployments: &Api<Deployment>,
    desired: &Deployment,
) -> Result<(), Error> {
    if desired
        .spec
        .as_ref()
        .and_then(|spec| spec.strategy.as_ref())
        .and_then(|strategy| strategy.type_.as_deref())
        != Some("Recreate")
    {
        return Ok(());
    }
    let name = desired.name_any();
    let Some(existing) = deployments.get_opt(&name).await? else {
        return Ok(());
    };
    if let Some(document) = recreate_strategy_transition(&existing) {
        if existing.metadata.resource_version.is_none() {
            return Err(Error::ControllerInvariant(
                "Deployment lacks resourceVersion",
            ));
        }
        deployments
            .patch(&name, &PatchParams::default(), &Patch::Merge(document))
            .await?;
    }
    Ok(())
}

fn recreate_strategy_transition(existing: &Deployment) -> Option<serde_json::Value> {
    let strategy = existing
        .spec
        .as_ref()
        .and_then(|spec| spec.strategy.as_ref());
    if strategy.is_some_and(|strategy| {
        strategy.type_.as_deref() == Some("Recreate") && strategy.rolling_update.is_none()
    }) {
        return None;
    }
    Some(serde_json::json!({
        "metadata": {"resourceVersion": existing.metadata.resource_version},
        "spec": {"strategy": {"type": "Recreate", "rollingUpdate": null}}
    }))
}

/// Explicitly omit rolling-update defaults from the desired Recreate document.
/// Existing defaults owned elsewhere are removed by the fenced merge above.
fn deployment_apply_document(deployment: &Deployment) -> serde_json::Value {
    let mut document = serde_json::json!(deployment);
    if document["spec"]["strategy"]["type"].as_str() == Some("Recreate") {
        document["spec"]["strategy"]["rollingUpdate"] = serde_json::Value::Null;
    }
    document
}

async fn cleanup(cell: Arc<ProofstormCell>, context: &Context) -> Result<Action, Error> {
    context.probes.remove(&cell.spec.instance_key);
    private_transfer::close(&cell)?;
    let instance_namespace = instance_namespace(&cell.spec.instance_key);
    patch_status(
        cell.as_ref(),
        context,
        ProofstormCellStatus {
            observed_desired_generation: cell
                .annotations()
                .get("proofstorm.dev/desired-generation")
                .and_then(|g| g.parse().ok())
                .unwrap_or(1),
            last_converged_revision: None,
            retained_storage: BTreeMap::new(),
            phase: CellPhase::Closing,
            instance_namespace: Some(instance_namespace.clone()),
            observed_generation: cell.metadata.generation,
            observed_revision_digest: cell.status.as_ref().map_or_else(String::new, |status| {
                status.observed_revision_digest.clone()
            }),
            observed_protocol_probe_lease: cell
                .status
                .as_ref()
                .and_then(|status| status.observed_protocol_probe_lease.clone()),
            // A closing cell vouches for no protocol evidence.
            protocol_freshness: None,
            components: Vec::new(),
            inventory: cell
                .status
                .as_ref()
                .map_or_else(Vec::new, |status| status.inventory.clone()),
            inventory_digest: cell
                .status
                .as_ref()
                .and_then(|status| status.inventory_digest.clone()),
            teardown_receipt: None,
            message: Some("deleting instance namespace and verifying absence".to_owned()),
        },
    )
    .await?;

    let control_namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let actions =
        Api::<ProofstormCellAction>::namespaced(context.client.clone(), &control_namespace);
    let runtime_actions = actions
        .list(&ListParams::default().labels(&format!(
            "proofstorm.dev/instance={}",
            cell.spec.instance_key
        )))
        .await?;
    if !runtime_actions.items.is_empty() {
        for action in runtime_actions.items {
            gone(
                actions
                    .delete(&action.name_any(), &DeleteParams::default())
                    .await,
            )?;
        }
        return Err(Error::ActionCleanupPending(cell.spec.instance_key.clone()));
    }

    let namespaces = Api::<Namespace>::all(context.client.clone());
    if namespaces.get_opt(&instance_namespace).await?.is_some() {
        match namespaces
            .delete(&instance_namespace, &DeleteParams::default())
            .await
        {
            Ok(_) => return Err(Error::CleanupPending(instance_namespace)),
            Err(kube::Error::Api(error)) if error.code == 404 => {}
            Err(error) => return Err(error.into()),
        }
    }

    private_transfer::remove_closed(&cell)?;
    write_teardown_receipt(cell.as_ref(), context, &instance_namespace).await?;
    Ok(Action::await_change())
}

/// Write the cell status: the full status when anything but the heartbeat
/// changed, and the protocol freshness heartbeat on its own. Component
/// statuses live in their own objects and are never written here.
async fn patch_status(
    cell: &ProofstormCell,
    context: &Context,
    mut status: ProofstormCellStatus,
) -> Result<(), Error> {
    let heartbeat = status.protocol_freshness.take();
    status.components.clear();
    let mut current = cell.status.clone();
    let current_heartbeat = current
        .as_mut()
        .and_then(|current| current.protocol_freshness.take());
    if let Some(current) = current.as_mut() {
        current.components.clear();
    }
    let update = cell_status_update_required(current.as_ref(), &status);
    if !update && current_heartbeat == heartbeat {
        return Ok(());
    }
    // Component count must not silently disable observations. The Kubernetes
    // API enforces its actual object-size limit; report that failure normally.
    let namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let cells = Api::<ProofstormCell>::namespaced(context.client.clone(), &namespace);
    if update {
        let patch = serde_json::json!({"apiVersion": "proofstorm.dev/v1alpha1", "kind": "ProofstormCell", "status": status});
        cells
            .patch_status(
                &cell.name_any(),
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(patch),
            )
            .await?;
    }
    if current_heartbeat != heartbeat {
        // An empty status removes the heartbeat this manager owns.
        let status = heartbeat.map_or_else(
            || serde_json::json!({}),
            |heartbeat| serde_json::json!({"protocolFreshness": heartbeat}),
        );
        cells
            .patch_status(
                &cell.name_any(),
                &PatchParams::apply(HEARTBEAT_FIELD_MANAGER).force(),
                &Patch::Apply(serde_json::json!({"apiVersion": "proofstorm.dev/v1alpha1", "kind": "ProofstormCell", "status": status})),
            )
            .await?;
    }
    Ok(())
}

/// Write each component object whose state changed, then delete the objects
/// of components the revision no longer has. A new probe alone never writes:
/// the cell's heartbeat carries probe times.
async fn write_component_objects(
    cell: &LoadedCell,
    context: &Context,
    stored: &[Arc<ProofstormComponent>],
    components: &[proofstorm_core::ComponentStatus],
) -> Result<(), Error> {
    let namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let api = Api::<ProofstormComponent>::namespaced(context.client.clone(), &namespace);
    let by_id = stored
        .iter()
        .map(|object| (object.spec.component_id.as_str(), object.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let owner = kube::Resource::controller_owner_ref(&cell.resource, &());
    let writes = components
        .iter()
        .filter(|status| {
            !by_id
                .get(status.id.as_str())
                .and_then(|object| object.status.as_ref())
                .is_some_and(|stored| proofstorm_kube::same_material(stored, status))
        })
        .map(|status| {
            let mut object = proofstorm_kube::component_object(
                &cell.spec.instance_key,
                &namespace,
                status,
                owner.clone(),
            );
            object.status = None;
            let exists = by_id.contains_key(status.id.as_str());
            let status_patch = serde_json::json!({"apiVersion": "proofstorm.dev/v1alpha1", "kind": "ProofstormComponent", "status": status});
            let api = api.clone();
            // Owned values only: the write futures run concurrently.
            async move {
                let name = object.name_any();
                let params = PatchParams::apply(FIELD_MANAGER).force();
                if !exists {
                    api.patch(&name, &params, &Patch::Apply(&object)).await?;
                }
                api.patch_status(&name, &params, &Patch::Apply(status_patch))
                    .await?;
                Ok::<_, Error>(())
            }
        })
        .collect::<Vec<_>>();
    futures::stream::iter(writes)
        .buffer_unordered(COMPONENT_WRITE_CONCURRENCY)
        .collect::<Vec<Result<(), Error>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<()>, Error>>()?;
    let desired = components
        .iter()
        .map(|status| status.id.as_str())
        .collect::<BTreeSet<_>>();
    let deletes = stored
        .iter()
        .filter(|object| !desired.contains(object.spec.component_id.as_str()))
        .map(|object| {
            let (api, name) = (api.clone(), object.name_any());
            async move { gone(api.delete(&name, &DeleteParams::default()).await) }
        })
        .collect::<Vec<_>>();
    futures::stream::iter(deletes)
        .buffer_unordered(COMPONENT_WRITE_CONCURRENCY)
        .collect::<Vec<Result<(), Error>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<()>, Error>>()?;
    Ok(())
}

fn cell_status_update_required(
    current: Option<&ProofstormCellStatus>,
    observed: &ProofstormCellStatus,
) -> bool {
    current != Some(observed)
}

fn enforce_status_budget<T: serde::Serialize>(
    kind: &'static str,
    status: &T,
    maximum: usize,
) -> Result<(), Error> {
    let actual = serde_json::to_vec(status)
        .map_err(|_| Error::ControllerInvariant("typed status did not serialize"))?
        .len();
    if actual <= maximum {
        Ok(())
    } else {
        Err(Error::StatusBudgetExceeded {
            kind,
            actual,
            maximum,
        })
    }
}

async fn write_teardown_receipt(
    cell: &ProofstormCell,
    context: &Context,
    instance_namespace: &str,
) -> Result<(), Error> {
    let namespace = cell
        .namespace()
        .ok_or_else(|| Error::MissingNamespace(cell.name_any()))?;
    let receipts = Api::<ConfigMap>::namespaced(context.client.clone(), &namespace);
    // Cleanup never waits on the revision: record its lock digest when it is still there.
    let lock_digest = context
        .revisions
        .load(&context.client, cell, &cell.spec.revision_digest)
        .await
        .map(|revision| revision.lock.digest.clone())
        .unwrap_or_default();
    let name = format!("proofstorm-teardown-{}", cell.spec.instance_key);
    let receipt = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "proofstorm.dev/receipt": "teardown",
                "proofstorm.dev/instance": cell.spec.instance_key
            }
        },
        "data": {
            "instanceId": cell.spec.instance_id,
            "cellName": cell.name_any(),
            "revisionDigest": cell.spec.revision_digest,
            "lockDigest": lock_digest,
            "inventoryDigest": cell.status.as_ref().and_then(|status| status.inventory_digest.clone()).unwrap_or_default(),
            "instanceNamespace": instance_namespace,
            "verifiedAbsent": "true"
        }
    });
    receipts
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(receipt),
        )
        .await?;
    Ok(())
}

fn validate_instance_key(key: &str) -> Result<(), Error> {
    let valid = (12..=32).contains(&key.len())
        && key.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && key.ends_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        && key.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidInstanceKey(key.to_owned()))
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "kube runtime requires an owned Arc in the error-policy callback signature"
)]
fn error_policy(cell: Arc<ProofstormCell>, error: &Error, context: Arc<Context>) -> Action {
    let action = retry_action(
        cell.as_ref(),
        error,
        &context.retries,
        &cell.spec.instance_key,
    );
    eprintln!("retryable controller error ({action:?}): {error}");
    action
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "kube runtime requires an owned Arc in the error-policy callback signature"
)]
fn action_error_policy(
    action: Arc<ProofstormCellAction>,
    error: &Error,
    context: Arc<Context>,
) -> Action {
    let next = retry_action(
        action.as_ref(),
        error,
        &context.retries,
        &action.spec.operation_id,
    );
    eprintln!("retryable action controller error ({next:?}): {error}");
    next
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "kube runtime requires an owned Arc in the error-policy callback signature"
)]
fn candidate_error_policy(
    build: Arc<ProofstormCandidateBuild>,
    error: &Error,
    context: Arc<Context>,
) -> Action {
    let next = retry_action(
        build.as_ref(),
        error,
        &context.retries,
        &build.spec.candidate_id,
    );
    eprintln!("retryable candidate build controller error ({next:?}): {error}");
    next
}

/// Cleanup waits are expected progress and keep their short jittered requeue.
/// Every other failure backs off exponentially per object until it succeeds.
fn retry_action<K>(object: &K, error: &Error, retries: &retry::Backoff, identity: &str) -> Action
where
    K: kube::Resource<DynamicType = ()>,
{
    if matches!(
        error,
        Error::CleanupPending(_) | Error::ActionCleanupPending(_)
    ) {
        return jittered_requeue(identity, 5, 4);
    }
    let (key, fingerprint) = retry::identity(object);
    Action::requeue(retries.failed(&key, &fingerprint, std::time::Instant::now()))
}

/// Skip work while an unchanged object is still backing off, whatever
/// triggered this reconcile; clear the backoff once a reconcile succeeds.
async fn with_backoff<K, F>(object: &K, retries: &retry::Backoff, work: F) -> Result<Action, Error>
where
    K: kube::Resource<DynamicType = ()>,
    F: std::future::Future<Output = Result<Action, Error>>,
{
    let (key, fingerprint) = retry::identity(object);
    if let Some(wait) = retries.remaining(&key, &fingerprint, std::time::Instant::now()) {
        return Ok(Action::requeue(wait));
    }
    let result = work.await;
    if result.is_ok() {
        retries.succeeded(&key);
    }
    result
}

async fn reconcile_cell(cell: Arc<ProofstormCell>, context: Arc<Context>) -> Result<Action, Error> {
    with_backoff(
        cell.as_ref(),
        &context.retries,
        Box::pin(reconcile(cell.clone(), context.clone())),
    )
    .await
}

async fn reconcile_action_with_backoff(
    action: Arc<ProofstormCellAction>,
    context: Arc<Context>,
) -> Result<Action, Error> {
    with_backoff(
        action.as_ref(),
        &context.retries,
        Box::pin(reconcile_action(action.clone(), context.clone())),
    )
    .await
}

async fn reconcile_candidate_build_with_backoff(
    build: Arc<ProofstormCandidateBuild>,
    context: Arc<Context>,
) -> Result<Action, Error> {
    with_backoff(
        build.as_ref(),
        &context.retries,
        Box::pin(reconcile_candidate_build(build.clone(), context.clone())),
    )
    .await
}

fn jittered_requeue(identity: &str, base_seconds: u64, spread_seconds: u64) -> Action {
    Action::requeue(Duration::from_secs(
        base_seconds + deterministic_jitter_seconds(identity, spread_seconds),
    ))
}

fn deterministic_jitter_seconds(identity: &str, spread_seconds: u64) -> u64 {
    let digest = proofstorm_core::digest_json(&identity);
    u64::from_str_radix(&digest["sha256:".len().."sha256:".len() + 8], 16).unwrap_or_default()
        % (spread_seconds + 1)
}

fn termination_message(pod: &Pod, target: &str) -> Option<String> {
    pod.status
        .as_ref()?
        .container_statuses
        .as_ref()?
        .iter()
        .find(|status| status.name == target)?
        .state
        .as_ref()?
        .terminated
        .as_ref()?
        .message
        .clone()
}

fn authentication_conformance_artifact(
    action: &ProofstormCellAction,
    cell: &LoadedCell,
    pods: &[Pod],
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let CellAction::AuthenticationConformance(request) = &action.spec.action else {
        return None;
    };
    let message = pods
        .iter()
        .find_map(|pod| termination_message(pod, "authentication"))?;
    let implementation = &cell
        .revision
        .cell
        .components
        .iter()
        .find(|component| component.id == request.mint)?
        .implementation;
    validate_authentication_conformance_result(request, implementation, &message)
}

fn validate_authentication_conformance_result(
    request: &proofstorm_kube::AuthenticationConformanceAction,
    implementation: &str,
    message: &str,
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let profile =
        proofstorm_driver::authentication_profile::AuthenticationProfile::for_implementation(
            implementation,
        )?;
    let result = serde_json::from_str::<AuthenticationConformanceResult>(message).ok()?;
    if result.contract != "proofstorm/authentication-conformance/v1"
        || result.mint != request.mint
        || result.identity_provider != request.identity_provider
        || (result.conformant
            && (!result.advertised_nut21
                || !result.advertised_nut22
                || !result.invalid_oidc_password_rejected
                || !result.missing_cat_rejected
                || result.invalid_cat_code != Some(profile.invalid_cat)
                || !result.missing_bat_rejected
                || result.invalid_bat_code != Some(profile.invalid_bat)
                || !result.oidc_login
                || !result.claims_match
                || !result.mint_accepted_cat
                || !result.bat_issued
                || !result.bat_dleq
                || result.bat_max_code != Some(profile.bat_maximum)
                || result.rate_limit_code != profile.cat_rate_limit
                || result.failure_stage.is_some()
                || result.failure_status.is_some()
                || result.failure_protocol_code.is_some()))
        || (!result.conformant && result.failure_stage.is_none())
    {
        return None;
    }
    Some(status_object(
        serde_json::to_value(result).expect("typed authentication result serializes"),
    ))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateAuthenticationProtectedSpendResult {
    contract: String,
    mint: String,
    identity_provider: String,
    bat_count: u32,
    bat_dleq: bool,
    protected_request: bool,
    conformant: bool,
    failure_stage: Option<AuthenticationSessionFailureStage>,
    failure_status: Option<u16>,
    failure_protocol_code: Option<u32>,
    spent_bat: Option<String>,
}

async fn authentication_protected_spend_artifact(
    action: &ProofstormCellAction,
    pods: &[Pod],
    context: &Context,
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, Error> {
    if !matches!(
        action.spec.action,
        CellAction::AuthenticationProtectedSpend(_)
    ) {
        return Ok(None);
    }
    let Some(message) = pods
        .iter()
        .find_map(|pod| termination_message(pod, "authentication"))
    else {
        return Ok(None);
    };
    let Some((public, spent_bat)) =
        validate_authentication_protected_spend_result(action, &message)
    else {
        return Ok(None);
    };
    if let Some(spent_bat) = spent_bat {
        persist_authentication_session(action, &spent_bat, context).await?;
    }
    Ok(Some(status_object(
        serde_json::to_value(public).expect("typed protected-spend result serializes"),
    )))
}

fn validate_authentication_protected_spend_result(
    action: &ProofstormCellAction,
    message: &str,
) -> Option<(AuthenticationProtectedSpendResult, Option<String>)> {
    let CellAction::AuthenticationProtectedSpend(request) = &action.spec.action else {
        return None;
    };
    let result = serde_json::from_str::<PrivateAuthenticationProtectedSpendResult>(message).ok()?;
    if result.contract != "proofstorm/authentication-protected-spend-private/v1"
        || result.mint != request.mint
        || result.identity_provider != request.identity_provider
    {
        return None;
    }

    let (session_operation_id, spent_bat) = if result.conformant {
        let spent_bat = result.spent_bat.as_deref()?;
        if result.bat_count != 3
            || !result.bat_dleq
            || !result.protected_request
            || result.failure_stage.is_some()
            || result.failure_status.is_some()
            || result.failure_protocol_code.is_some()
            || !spent_bat.starts_with("authA")
            || spent_bat.len() > 4_096
        {
            return None;
        }
        (
            Some(action.spec.operation_id.clone()),
            Some(spent_bat.to_owned()),
        )
    } else {
        if result.failure_stage.is_none() || result.spent_bat.is_some() {
            return None;
        }
        (None, None)
    };
    let public = AuthenticationProtectedSpendResult {
        contract: "proofstorm/authentication-protected-spend/v1".to_owned(),
        mint: result.mint,
        identity_provider: result.identity_provider,
        bat_count: result.bat_count,
        bat_dleq: result.bat_dleq,
        protected_request: result.protected_request,
        session_operation_id,
        conformant: result.conformant,
        failure_stage: result.failure_stage,
        failure_status: result.failure_status,
        failure_protocol_code: result.failure_protocol_code,
    };
    Some((public, spent_bat))
}

async fn persist_authentication_session(
    action: &ProofstormCellAction,
    spent_bat: &str,
    context: &Context,
) -> Result<(), Error> {
    let namespace = instance_namespace(&action.spec.instance_key);
    let name = format!("{}-auth-session", action.name_any());
    let secrets = Api::<Secret>::namespaced(context.client.clone(), &namespace);
    if let Some(existing) = secrets.get_opt(&name).await? {
        let matches = existing
            .data
            .as_ref()
            .and_then(|data| data.get("SPENT_BAT"))
            .is_some_and(|value| value.0 == spent_bat.as_bytes());
        if matches {
            return Ok(());
        }
        return Err(Error::SecretContract(format!(
            "authentication session Secret {name:?} does not match its source operation"
        )));
    }
    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "proofstormd",
                "proofstorm.dev/instance": action.spec.instance_key,
                "proofstorm.dev/authentication-session": "true"
            },
            "annotations": {
                "proofstorm.dev/source-operation": action.spec.operation_id
            }
        },
        "immutable": true,
        "type": "Opaque",
        "stringData": {"SPENT_BAT": spent_bat}
    });
    secrets
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(secret),
        )
        .await?;
    Ok(())
}

fn authentication_replay_artifact(
    action: &ProofstormCellAction,
    cell: &LoadedCell,
    pods: &[Pod],
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let CellAction::AuthenticationReplay(request) = &action.spec.action else {
        return None;
    };
    let message = pods
        .iter()
        .find_map(|pod| termination_message(pod, "authentication"))?;
    let implementation = &cell
        .revision
        .cell
        .components
        .iter()
        .find(|component| component.id == request.mint)?
        .implementation;
    validate_authentication_replay_result(request, implementation, &message)
}

fn validate_authentication_replay_result(
    request: &proofstorm_kube::AuthenticationReplayAction,
    implementation: &str,
    message: &str,
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let profile =
        proofstorm_driver::authentication_profile::AuthenticationProfile::for_implementation(
            implementation,
        )?;
    let result = serde_json::from_str::<AuthenticationReplayResult>(message).ok()?;
    if result.contract != "proofstorm/authentication-replay/v1"
        || result.mint != request.mint
        || result.identity_provider != request.identity_provider
        || result.source_operation_id != request.source_operation_id
        || (result.conformant
            && (result.spent_bat_replay_code != Some(profile.spent_bat)
                || result.fresh_bat_count != 3
                || !result.fresh_bat_dleq
                || !result.protected_request
                || result.failure_stage.is_some()
                || result.failure_status.is_some()
                || result.failure_protocol_code.is_some()))
        || (!result.conformant && result.failure_stage.is_none())
    {
        return None;
    }
    Some(status_object(
        serde_json::to_value(result).expect("typed authentication replay result serializes"),
    ))
}

/// Ready and total endpoint addresses across one Service's `EndpointSlice` set.
///
/// A `ready` condition that is absent means unknown, which the `EndpointSlice`
/// contract tells consumers to read as ready; only an explicit `false` is
/// not-ready.
fn endpoint_readiness(slices: &[EndpointSlice]) -> (usize, usize) {
    let mut ready = 0;
    let mut total = 0;
    for slice in slices {
        for endpoint in &slice.endpoints {
            let addresses = endpoint.addresses.len();
            total += addresses;
            if endpoint
                .conditions
                .as_ref()
                .and_then(|conditions| conditions.ready)
                != Some(false)
            {
                ready += addresses;
            }
        }
    }
    (ready, total)
}

/// Why a target could not be reached, when its endpoints say so.
///
/// Zero of zero and zero of some are different facts. A component that
/// publishes no Service endpoints serves nothing over the network by design; a
/// component whose endpoints all exist but are unready refuses connections
/// until it becomes ready, which is a state the caller can wait out.
const fn target_endpoint_diagnostic(
    ready: usize,
    total: usize,
) -> Option<(&'static str, &'static str)> {
    match (ready, total) {
        (0, 0) => Some((
            "no_service_endpoints",
            "the target component publishes no Service endpoints, so it cannot be reached over the network at all; a wallet, for example, is driven through its own CLI and data volume rather than a served port",
        )),
        (0, _) => Some((
            "no_ready_endpoints",
            "the target component's Service had endpoints but none ready, so connections to it are refused rather than timing out; this is a readiness state, not a missing listener or a blocked network policy",
        )),
        _ => None,
    }
}

/// Native execution reaches its target through that component's Service. While
/// the component is not Ready the Service has no ready endpoints and every
/// connection to it is refused immediately, which is indistinguishable inside
/// the pod from nothing listening. Recording the endpoint counts alongside the
/// output makes that difference legible without a second investigation.
async fn native_exec_target_readiness(
    endpoint_slices: &Api<EndpointSlice>,
    target_component: &str,
) -> Result<(usize, usize), kube::Error> {
    let slices = endpoint_slices
        .list(
            &ListParams::default()
                .labels(&format!("kubernetes.io/service-name={target_component}")),
        )
        .await?;
    Ok(endpoint_readiness(&slices.items))
}

async fn native_exec_artifact(
    pods: &Api<Pod>,
    endpoint_slices: &Api<EndpointSlice>,
    action: &ProofstormCellAction,
    observed: &[Pod],
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, kube::Error> {
    const LOG_LIMIT_BYTES: i64 = 20 * 1024;
    const ARTIFACT_TARGET_BYTES: usize = 30 * 1024;

    let CellAction::ComponentForensics(request) = &action.spec.action else {
        return Ok(None);
    };
    let Some((pod, metadata)) = observed.iter().find_map(|pod| {
        termination_message(pod, "exec")
            .and_then(|message| serde_json::from_str::<serde_json::Value>(&message).ok())
            .map(|metadata| (pod, metadata))
    }) else {
        return Ok(None);
    };
    let mut output = pods
        .logs(
            &pod.name_any(),
            &LogParams {
                container: Some("exec".into()),
                limit_bytes: Some(LOG_LIMIT_BYTES),
                ..LogParams::default()
            },
        )
        .await?;
    let mut truncated = output.len() >= usize::try_from(LOG_LIMIT_BYTES).unwrap_or(usize::MAX);
    let exit_code = metadata
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);

    let (ready_endpoints, total_endpoints) =
        native_exec_target_readiness(endpoint_slices, &request.target_component).await?;

    loop {
        let mut artifact = status_object(serde_json::json!({
            "component": request.component,
            "target_component": request.target_component,
            "exit_code": exit_code,
            "combined_output": output,
            "output_truncated": truncated,
            "target_ready_endpoints": ready_endpoints,
            "target_endpoints": total_endpoints,
        }));
        if let Some((code, message)) = target_endpoint_diagnostic(ready_endpoints, total_endpoints)
        {
            artifact.insert("target_diagnostic".to_owned(), serde_json::json!(code));
            artifact.insert(
                "target_diagnostic_message".to_owned(),
                serde_json::json!(message),
            );
        }
        if serde_json::to_vec(&artifact)
            .map_or(true, |encoded| encoded.len() <= ARTIFACT_TARGET_BYTES)
        {
            return Ok(Some(artifact));
        }
        truncated = true;
        let target = output.len().saturating_mul(3) / 4;
        let boundary = output
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= target)
            .last()
            .unwrap_or(0);
        output.truncate(boundary);
    }
}

fn container_failure(pod: &Pod) -> Option<serde_json::Value> {
    let status = pod.status.as_ref()?;
    status
        .init_container_statuses
        .iter()
        .chain(status.container_statuses.iter())
        .flatten()
        .filter_map(|container| {
            let terminated = container.state.as_ref()?.terminated.as_ref()?;
            (terminated.exit_code != 0).then(|| {
                let mut failure = serde_json::json!({
                    "code": "container_failed",
                    "container": container.name,
                    "exit_code": terminated.exit_code,
                    "reason": terminated.reason,
                });
                // With FallbackToLogsOnError a script that did not write its own
                // structured diagnostic leaves the native error here. Surfacing a
                // bounded tail turns an opaque container_failed into the reason
                // the command actually gave.
                if let Some(tail) = terminated
                    .message
                    .as_deref()
                    .filter(|message| serde_json::from_str::<serde_json::Value>(message).is_err())
                    .map(native_error_tail)
                    .filter(|tail| !tail.is_empty())
                {
                    failure["native_error_tail"] = serde_json::json!(tail);
                }
                if let Some(diagnostic) = terminated
                    .message
                    .as_deref()
                    .and_then(|message| serde_json::from_str::<serde_json::Value>(message).ok())
                    .filter(|diagnostic| {
                        diagnostic.get("code").and_then(serde_json::Value::as_str)
                            == Some("wallet_orchestration_failed")
                    })
                {
                    for (source, target) in [
                        ("code", "failure_code"),
                        ("stage", "stage"),
                        ("reason", "diagnostic_reason"),
                    ] {
                        if let Some(value) =
                            diagnostic.get(source).and_then(serde_json::Value::as_str)
                        {
                            failure[target] = serde_json::json!(value);
                        }
                    }
                }
                failure
            })
        })
        .max_by_key(container_failure_priority)
}

/// Last lines of a native error, bounded for the journal. Kubernetes already
/// caps a termination message, and an action artifact is small, so this keeps
/// the end of the output where the error itself is.
fn native_error_tail(message: &str) -> String {
    const MAX_LINES: usize = 20;
    const MAX_BYTES: usize = 2 * 1024;
    let lines = message
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let mut tail = lines
        .iter()
        .rev()
        .take(MAX_LINES)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.len() > MAX_BYTES {
        let boundary = tail
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= tail.len() - MAX_BYTES)
            .last()
            .unwrap_or(0);
        tail = tail.split_off(boundary);
    }
    tail
}

fn container_failure_priority(failure: &serde_json::Value) -> u8 {
    match failure
        .get("diagnostic_reason")
        .and_then(serde_json::Value::as_str)
    {
        None => 0,
        Some("payer_failed" | "wallet_failed" | "wallet_failed_after_payment") => 1,
        Some(_) => 2,
    }
}

fn now_unix() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

fn status_object(
    value: serde_json::Value,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object.into_iter().collect(),
        value => std::collections::BTreeMap::from([("value".to_owned(), value)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_cell_gets_its_finalizer_fenced_and_keeps_others() {
        let mut cell = ProofstormCell::new(
            "cell",
            proofstorm_kube::ProofstormCellSpec {
                workspace_id: "test".into(),
                instance_id: "test".into(),
                instance_key: "i0123456789012345678".into(),
                revision_digest: "sha256:current".into(),
            },
        );
        cell.metadata.resource_version = Some("612".into());
        assert_eq!(
            finalizer_patch(&cell),
            Some(
                serde_json::json!({"metadata": {"resourceVersion": "612", "finalizers": [FINALIZER]}})
            )
        );
        cell.metadata.finalizers = Some(vec!["example.com/other".into()]);
        assert_eq!(
            finalizer_patch(&cell).unwrap()["metadata"]["finalizers"],
            serde_json::json!(["example.com/other", FINALIZER])
        );
        cell.metadata.finalizers = Some(vec![FINALIZER.into()]);
        assert_eq!(finalizer_patch(&cell), None, "already has it");
        cell.metadata.finalizers = None;
        cell.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::UNIX_EPOCH,
            ));
        assert_eq!(finalizer_patch(&cell), None, "a deleted cell gets none");
    }

    #[tokio::test]
    async fn failing_cells_are_held_back_whatever_triggers_the_next_reconcile() {
        let spec = proofstorm_core::CellSpec {
            api_version: proofstorm_core::API_VERSION.into(),
            name: "backoff".into(),
            components: vec![],
            links: vec![],
            policy: proofstorm_core::CellPolicy::default(),
        };
        let lock =
            proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).expect("lock");
        let mut cell = proofstorm_kube::LoadedCell::from_spec(
            ProofstormCell::new(
                "backoff",
                proofstorm_kube::ProofstormCellSpec {
                    workspace_id: "test".into(),
                    instance_id: "test".into(),
                    instance_key: "i0123456789012345678".into(),
                    revision_digest: "sha256:current".into(),
                },
            ),
            spec,
            lock,
        );
        cell.metadata.namespace = Some("proofstorm-system".into());
        cell.metadata.generation = Some(1);
        let retries = retry::Backoff::default();
        let attempts = std::sync::atomic::AtomicU32::new(0);
        let failing = || async {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<Action, _>(Error::SecretContract("missing key".into()))
        };

        let error = with_backoff(&cell.resource, &retries, failing())
            .await
            .unwrap_err();
        assert_eq!(
            retry_action(&cell.resource, &error, &retries, "cell"),
            Action::requeue(Duration::from_secs(1))
        );
        // An immediate prober trigger or watch event does no work.
        let held = with_backoff(&cell.resource, &retries, failing())
            .await
            .expect("held");
        assert_ne!(
            held,
            Action::await_change(),
            "a held reconcile must requeue"
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);

        // An edit is reconciled at once.
        cell.metadata.generation = Some(2);
        assert!(
            with_backoff(&cell.resource, &retries, failing())
                .await
                .is_err()
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);

        // Cleanup progress keeps its short requeue and records no backoff.
        let pending = retry_action(
            &cell.resource,
            &Error::CleanupPending("namespace".into()),
            &retries,
            "cell",
        );
        assert_eq!(pending, jittered_requeue("cell", 5, 4));
    }

    fn authentication_request() -> proofstorm_kube::AuthenticationConformanceAction {
        proofstorm_kube::AuthenticationConformanceAction {
            mint: "mint".into(),
            identity_provider: "identity".into(),
        }
    }

    fn authentication_result(conformant: bool) -> serde_json::Value {
        serde_json::json!({
            "contract": "proofstorm/authentication-conformance/v1",
            "mint": "mint",
            "identity_provider": "identity",
            "advertised_nut21": true,
            "advertised_nut22": true,
            "invalid_oidc_password_rejected": true,
            "missing_cat_rejected": true,
            "invalid_cat_code": 30002,
            "missing_bat_rejected": true,
            "invalid_bat_code": 31002,
            "oidc_login": true,
            "claims_match": true,
            "mint_accepted_cat": true,
            "bat_issued": conformant,
            "bat_dleq": conformant,
            "bat_max_code": 31003,
            "rate_limit_code": if conformant { Some(31004) } else { None },
            "conformant": conformant,
            "failure_stage": if conformant { None } else { Some("bat_issuance") },
            "failure_status": if conformant { None } else { Some(400) },
            "failure_protocol_code": null
        })
    }

    fn protected_spend_action() -> ProofstormCellAction {
        ProofstormCellAction::new(
            "op-auth-spend-resource",
            proofstorm_kube::ProofstormCellActionSpec {
                access_scope: None,
                cell_name: "cell-auth".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                experiment_id: "experiment".into(),
                session_id: "session".into(),
                principal_id: "principal".into(),
                sequence: 1,
                operation_id: "auth-spend".into(),
                request_digest: "sha256:request".into(),
                capability: proofstorm_core::Capability::AuthenticationTest,
                accepted_at_unix: 1,
                action: CellAction::AuthenticationProtectedSpend(
                    proofstorm_kube::AuthenticationProtectedSpendAction {
                        mint: "mint".into(),
                        identity_provider: "identity".into(),
                    },
                ),
            },
        )
    }

    #[test]
    fn only_finished_one_off_actions_expire() {
        let finished = |mut action: ProofstormCellAction| {
            action.status = Some(ProofstormCellActionStatus {
                phase: ActionPhase::Succeeded,
                completed_at_unix: Some(100),
                ..ProofstormCellActionStatus::default()
            });
            action
        };
        let spend = protected_spend_action();
        assert_eq!(finished_action_expiry(&spend), None);
        assert_eq!(
            finished_action_expiry(&finished(spend.clone())),
            Some(100 + FINISHED_ACTION_RETENTION_SECONDS)
        );
        let mut exec = spend.clone();
        exec.spec.action =
            CellAction::ComponentExecLive(proofstorm_kube::ComponentExecLiveAction {
                private_payload: None,
                component: "wallet".into(),
                script: "true".into(),
                argv: vec![],
                timeout_seconds: 30,
                output: proofstorm_core::native::NativeOutput::default(),
            });
        assert!(finished_action_expiry(&finished(exec.clone())).is_some());
        let mut child = exec;
        child
            .annotations_mut()
            .insert(workspace_bridge::PARENT_ANNOTATION.into(), "parent".into());
        assert_eq!(finished_action_expiry(&finished(child)), None);
        let mut partition = spend;
        partition.spec.action =
            CellAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                from_component: "a".into(),
                to_component: "b".into(),
            });
        assert_eq!(finished_action_expiry(&finished(partition)), None);
    }

    #[test]
    fn recreate_transition_uses_resource_version_and_merge_deletion() {
        let deployment: Deployment = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "mint", "resourceVersion": "123"},
            "spec": {"selector": {"matchLabels": {"app": "mint"}},
                "template": {}, "strategy": {"type": "RollingUpdate", "rollingUpdate": {"maxSurge": "25%"}}}
        })).unwrap();
        assert_eq!(
            recreate_strategy_transition(&deployment),
            Some(serde_json::json!({
                "metadata": {"resourceVersion": "123"},
                "spec": {"strategy": {"type": "Recreate", "rollingUpdate": null}}
            }))
        );
        let mut recreated = deployment;
        let strategy = recreated.spec.as_mut().unwrap().strategy.as_mut().unwrap();
        strategy.type_ = Some("Recreate".into());
        strategy.rolling_update = None;
        assert!(recreate_strategy_transition(&recreated).is_none());
    }

    #[test]
    fn recreate_apply_explicitly_removes_rolling_update_defaults() {
        let deployment: Deployment = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "mint"},
            "spec": {"selector": {"matchLabels": {"app": "mint"}},
                "template": {}, "strategy": {"type": "Recreate"}}
        }))
        .unwrap();
        let document = deployment_apply_document(&deployment);
        assert_eq!(document["spec"]["strategy"]["type"], "Recreate");
        assert!(
            document["spec"]["strategy"]
                .as_object()
                .unwrap()
                .contains_key("rollingUpdate")
        );
        assert!(document["spec"]["strategy"]["rollingUpdate"].is_null());
        let mut rolling = deployment;
        rolling
            .spec
            .as_mut()
            .unwrap()
            .strategy
            .as_mut()
            .unwrap()
            .type_ = Some("RollingUpdate".into());
        assert!(
            !deployment_apply_document(&rolling)["spec"]["strategy"]
                .as_object()
                .unwrap()
                .contains_key("rollingUpdate")
        );
    }

    #[test]
    fn authentication_artifact_accepts_only_the_secret_free_typed_shape() {
        let request = authentication_request();
        let valid = authentication_result(true);
        assert!(
            validate_authentication_conformance_result(&request, "nutshell", &valid.to_string())
                .is_some()
        );

        let finding = authentication_result(false);
        assert!(
            validate_authentication_conformance_result(&request, "nutshell", &finding.to_string())
                .is_some()
        );

        let mut leaked = finding;
        leaked["password"] = serde_json::json!("sentinel-secret");
        assert!(
            validate_authentication_conformance_result(&request, "nutshell", &leaked.to_string())
                .is_none(),
            "unknown fields, including leaked credentials, fail closed"
        );

        let mut wrong_identity = authentication_result(false);
        wrong_identity["identity_provider"] = serde_json::json!("other");
        assert!(
            validate_authentication_conformance_result(
                &request,
                "nutshell",
                &wrong_identity.to_string()
            )
            .is_none()
        );
    }

    #[test]
    fn cdk_conformance_uses_its_own_codes_and_has_no_cat_rate_limit() {
        let request = authentication_request();
        let mut result = authentication_result(true);
        assert!(
            validate_authentication_conformance_result(&request, "cdk", &result.to_string())
                .is_none()
        );
        result["invalid_cat_code"] = serde_json::json!(30002);
        result["invalid_bat_code"] = serde_json::json!(10001);
        result["bat_max_code"] = serde_json::json!(11006);
        result["rate_limit_code"] = serde_json::Value::Null;
        assert!(
            validate_authentication_conformance_result(&request, "cdk", &result.to_string())
                .is_some()
        );
        assert!(
            validate_authentication_conformance_result(&request, "nutshell", &result.to_string())
                .is_none()
        );
        assert!(
            validate_authentication_conformance_result(&request, "unknown", &result.to_string())
                .is_none()
        );
        result["rate_limit_code"] = serde_json::json!(31004);
        assert!(
            validate_authentication_conformance_result(&request, "cdk", &result.to_string())
                .is_none()
        );
    }

    #[test]
    fn protected_spend_and_replay_strip_or_reject_bearer_material() {
        let action = protected_spend_action();
        let private = serde_json::json!({
            "contract": "proofstorm/authentication-protected-spend-private/v1",
            "mint": "mint",
            "identity_provider": "identity",
            "bat_count": 3,
            "bat_dleq": true,
            "protected_request": true,
            "conformant": true,
            "failure_stage": null,
            "failure_status": null,
            "failure_protocol_code": null,
            "spent_bat": "authAprivate-bearer"
        });
        let (public, token) =
            validate_authentication_protected_spend_result(&action, &private.to_string())
                .expect("valid private protected-spend result");
        assert_eq!(token.as_deref(), Some("authAprivate-bearer"));
        let public = serde_json::to_string(&public).expect("public result");
        assert!(!public.contains("private-bearer"));
        assert!(!public.contains("spent_bat"));

        let mut leaked = private;
        leaked["password"] = serde_json::json!("sentinel-secret");
        assert!(
            validate_authentication_protected_spend_result(&action, &leaked.to_string()).is_none()
        );

        let replay_request = proofstorm_kube::AuthenticationReplayAction {
            mint: "mint".into(),
            identity_provider: "identity".into(),
            session_secret: "op-auth-spend-resource-auth-session".into(),
            source_operation_id: "auth-spend".into(),
        };
        let replay = serde_json::json!({
            "contract": "proofstorm/authentication-replay/v1",
            "mint": "mint",
            "identity_provider": "identity",
            "source_operation_id": "auth-spend",
            "spent_bat_replay_code": 31002,
            "fresh_bat_count": 3,
            "fresh_bat_dleq": true,
            "protected_request": true,
            "conformant": true,
            "failure_stage": null,
            "failure_status": null,
            "failure_protocol_code": null
        });
        assert!(
            validate_authentication_replay_result(&replay_request, "nutshell", &replay.to_string())
                .is_some()
        );
        let mut cdk_replay = replay.clone();
        cdk_replay["spent_bat_replay_code"] = serde_json::json!(11001);
        assert!(
            validate_authentication_replay_result(&replay_request, "cdk", &cdk_replay.to_string())
                .is_some()
        );
        assert!(
            validate_authentication_replay_result(
                &replay_request,
                "nutshell",
                &cdk_replay.to_string()
            )
            .is_none()
        );
        assert!(
            validate_authentication_replay_result(
                &replay_request,
                "unknown",
                &cdk_replay.to_string()
            )
            .is_none()
        );
        let mut leaked_replay = replay;
        leaked_replay["spent_bat"] = serde_json::json!("authAprivate-bearer");
        assert!(
            validate_authentication_replay_result(
                &replay_request,
                "nutshell",
                &leaked_replay.to_string()
            )
            .is_none()
        );
    }

    #[test]
    fn native_error_tail_keeps_the_end_of_the_output_within_bounds() {
        assert_eq!(native_error_tail(""), "");
        assert_eq!(
            native_error_tail("only line\n"),
            "only line",
            "a single line survives whole"
        );
        let many = (1..=40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = native_error_tail(&many);
        assert!(
            tail.starts_with("line 21") && tail.ends_with("line 40"),
            "the last twenty lines are kept in order: {tail}"
        );
        let noisy = format!("blank lines are dropped\n\n\n{}", "x".repeat(9_000));
        let bounded = native_error_tail(&noisy);
        assert!(bounded.len() <= 2 * 1024, "bounded to the artifact budget");
        assert!(bounded.is_char_boundary(0));
    }

    #[test]
    fn a_failed_container_reports_the_native_error_it_logged() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "op-boot-abc", "namespace": "instance"},
            "status": {
                "initContainerStatuses": [{
                    "image": "lnd:test",
                    "imageID": "lnd@test",
                    "name": "channel-open",
                    "ready": false,
                    "restartCount": 0,
                    "state": {"terminated": {
                        "exitCode": 1,
                        "message": "[lncli] rpc error: code = Unknown desc = not enough witness outputs to create funding transaction, need 0.06003043 BTC only have 0.06000000 BTC",
                        "reason": "Error"
                    }}
                }]
            }
        }))
        .expect("pod");
        let failure = container_failure(&pod).expect("failure");
        assert_eq!(failure["code"], "container_failed");
        assert!(
            failure["native_error_tail"]
                .as_str()
                .expect("native error tail")
                .contains("not enough witness outputs"),
            "an opaque container failure carries the reason the command gave"
        );
    }

    #[test]
    fn endpoint_readiness_counts_addresses_and_reads_unknown_as_ready() {
        let slice = |value: serde_json::Value| -> EndpointSlice {
            serde_json::from_value(value).expect("endpoint slice")
        };
        assert_eq!(
            endpoint_readiness(&[]),
            (0, 0),
            "no slices means no endpoint"
        );
        let not_ready = slice(serde_json::json!({
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {"name": "mint-lnd-abc", "namespace": "instance"},
            "addressType": "IPv4",
            "endpoints": [{"addresses": ["10.0.0.2"], "conditions": {"ready": false}}]
        }));
        assert_eq!(
            endpoint_readiness(std::slice::from_ref(&not_ready)),
            (0, 1),
            "an unready component leaves its Service with no ready endpoint"
        );
        let mixed = slice(serde_json::json!({
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {"name": "mint-lnd-def", "namespace": "instance"},
            "addressType": "IPv4",
            "endpoints": [
                {"addresses": ["10.0.0.3"], "conditions": {"ready": true}},
                {"addresses": ["10.0.0.4"]},
                {"addresses": ["10.0.0.5"], "conditions": {"ready": false}}
            ]
        }));
        assert_eq!(
            endpoint_readiness(&[not_ready, mixed]),
            (2, 4),
            "an unknown ready condition counts as ready and slices aggregate"
        );
    }

    #[test]
    fn exec_target_diagnostics_separate_serving_from_unready() {
        for (ready, total, expected) in [
            (0_usize, 0_usize, Some("no_service_endpoints")),
            (0, 1, Some("no_ready_endpoints")),
            (0, 3, Some("no_ready_endpoints")),
            (1, 1, None),
            (2, 4, None),
        ] {
            assert_eq!(
                target_endpoint_diagnostic(ready, total).map(|(code, _)| code),
                expected,
                "ready {ready} of {total}"
            );
        }
        let (_, message) =
            target_endpoint_diagnostic(0, 1).expect("an unready target is diagnosed");
        assert!(
            message.contains("refused rather than timing out"),
            "the message must name the symptom the caller actually sees"
        );
    }

    #[test]
    fn instance_keys_are_bounded_dns_labels() {
        assert!(validate_instance_key("01abcxyz7890").is_ok());
        assert!(validate_instance_key("too-short").is_err());
        assert!(validate_instance_key("UPPERCASE-KEY-1").is_err());
        assert!(validate_instance_key("-invalid-start").is_err());
    }

    #[test]
    fn deadline_exceeded_has_a_stable_terminal_error() {
        let status = JobStatus {
            conditions: Some(vec![k8s_openapi::api::batch::v1::JobCondition {
                type_: "Failed".into(),
                status: "True".into(),
                reason: Some("DeadlineExceeded".into()),
                ..Default::default()
            }]),
            ..JobStatus::default()
        };
        assert_eq!(
            action_failure(&status, &[]),
            serde_json::json!({"code": "action_deadline_exceeded"})
        );
        assert!(is_terminal_action(ActionPhase::Cancelled));
    }

    #[test]
    fn sanitized_stage_diagnostic_precedes_the_outer_deadline() {
        let status = JobStatus {
            conditions: Some(vec![k8s_openapi::api::batch::v1::JobCondition {
                type_: "Failed".into(),
                status: "True".into(),
                reason: Some("DeadlineExceeded".into()),
                ..Default::default()
            }]),
            ..JobStatus::default()
        };
        let pod = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "wallet-fund"},
            "status": {
                "containerStatuses": [{
                    "image": "wallet:test",
                    "imageID": "wallet@test",
                    "name": "wallet",
                    "ready": false,
                    "restartCount": 0,
                    "state": {"terminated": {
                        "exitCode": 1,
                        "message": "{\"code\":\"wallet_orchestration_failed\",\"stage\":\"invoice\",\"reason\":\"invoice_exited_before_payment\",\"private\":\"not-forwarded\"}",
                        "reason": "Error"
                    }}
                }, {
                    "image": "lnd:test",
                    "imageID": "lnd@test",
                    "name": "payer",
                    "ready": false,
                    "restartCount": 0,
                    "state": {"terminated": {
                        "exitCode": 1,
                        "message": "{\"code\":\"wallet_orchestration_failed\",\"stage\":\"invoice\",\"reason\":\"wallet_failed\"}",
                        "reason": "Error"
                    }}
                }]
            }
        }))
        .expect("pod");
        assert_eq!(
            action_failure(&status, &[pod]),
            serde_json::json!({
                "code": "container_failed",
                "container": "wallet",
                "diagnostic_reason": "invoice_exited_before_payment",
                "exit_code": 1,
                "failure_code": "wallet_orchestration_failed",
                "reason": "Error",
                "stage": "invoice"
            })
        );
    }

    #[test]
    fn execution_evidence_fences_missing_job_replay() {
        assert!(!action_execution_started(None));
        assert!(!action_execution_started(Some(
            &ProofstormCellActionStatus::default()
        )));

        let running = ProofstormCellActionStatus {
            phase: ActionPhase::Running,
            job_name: Some("action-1".into()),
            started_at_unix: Some(42),
            ..ProofstormCellActionStatus::default()
        };
        assert!(action_execution_started(Some(&running)));
    }

    #[test]
    fn pod_watch_routes_only_the_matching_instance_namespace() {
        let mut pod = Pod::default();
        pod.metadata.labels = Some(BTreeMap::from([(INSTANCE_LABEL.into(), "demo".into())]));
        pod.metadata.namespace = Some("proofstorm-demo".into());
        assert!(pod_belongs_to_instance(&pod, "demo"));
        assert!(!pod_belongs_to_instance(&pod, "other"));
        pod.metadata.namespace = Some("unrelated".into());
        assert!(!pod_belongs_to_instance(&pod, "demo"));
        pod.metadata.labels = None;
        assert!(!pod_belongs_to_instance(&pod, "demo"));
    }

    #[test]
    fn applies_send_only_changed_documents_except_the_drift_pass() {
        let applied = |signature: &str, pruned: bool| probes::Applied {
            signature: signature.into(),
            at: std::time::Instant::now(),
            plans: Arc::default(),
            inventory: Vec::new(),
            retained_storage: BTreeMap::new(),
            pruned,
            documents: Arc::new(BTreeMap::from([("ConfigMap/a".into(), "d".into())])),
        };
        // A changed cell, or a prune still finishing, skips what didn't change.
        assert_eq!(skippable(Some(applied("old", true)), "new").len(), 1);
        assert_eq!(skippable(Some(applied("same", false)), "same").len(), 1);
        // The periodic pass over an unchanged cell, and a first apply, send everything.
        assert!(skippable(Some(applied("same", true)), "same").is_empty());
        assert!(skippable(None, "same").is_empty());

        let mut config: ConfigMap = serde_json::from_value(serde_json::json!({
            "metadata": {"name": "a"}, "data": {"k": "v"}
        }))
        .unwrap();
        let mut sent = BTreeMap::new();
        assert!(!unchanged(
            &mut sent,
            &BTreeMap::new(),
            "ConfigMap",
            &config
        ));
        let mut next = BTreeMap::new();
        assert!(unchanged(&mut next, &sent, "ConfigMap", &config));
        config.data = Some(BTreeMap::from([("k".into(), "changed".into())]));
        assert!(!unchanged(
            &mut BTreeMap::new(),
            &sent,
            "ConfigMap",
            &config
        ));
        // The same name under another kind is a different document.
        assert!(!unchanged(&mut BTreeMap::new(), &sent, "Service", &config));
    }

    #[test]
    fn controller_policy_is_bounded_deterministic_and_desynchronized() {
        assert_eq!(CELL_CONTROLLER_CONCURRENCY, 8);
        assert_eq!(
            proofstorm_core::ControllerLimits::default().action_slots,
            16
        );
        let first = deterministic_jitter_seconds("instance-one", 10);
        assert_eq!(first, deterministic_jitter_seconds("instance-one", 10));
        assert!(first <= 10);
        let offsets = (0..32)
            .map(|index| deterministic_jitter_seconds(&format!("instance-{index}"), 10))
            .collect::<BTreeSet<_>>();
        assert!(offsets.len() > 1, "cell requeues must not synchronize");
    }

    #[test]
    fn rendering_failure_reports_the_current_revision_and_keeps_cleanup_inventory() {
        let spec = proofstorm_core::CellSpec {
            api_version: proofstorm_core::API_VERSION.into(),
            name: "failure".into(),
            components: vec![],
            links: vec![],
            policy: proofstorm_core::CellPolicy::default(),
        };
        let lock =
            proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).expect("lock");
        let mut cell = proofstorm_kube::LoadedCell::from_spec(
            ProofstormCell::new(
                "failure",
                proofstorm_kube::ProofstormCellSpec {
                    workspace_id: "test".into(),
                    instance_id: "test".into(),
                    instance_key: "i0123456789012345678".into(),
                    revision_digest: "sha256:current".into(),
                },
            ),
            spec,
            lock,
        );
        cell.metadata.generation = Some(4);
        cell.metadata.annotations = Some(BTreeMap::from([(
            "proofstorm.dev/desired-generation".into(),
            "3".into(),
        )]));
        cell.status = Some(ProofstormCellStatus {
            phase: CellPhase::Ready,
            observed_revision_digest: "sha256:old".into(),
            inventory_digest: Some("sha256:cleanup".into()),
            ..ProofstormCellStatus::default()
        });
        let status = reconciliation_failure_status(
            &cell,
            &Error::Adapter(AdapterError::InvalidPlan("missing backend binding".into())),
        )
        .expect("visible failure");
        assert_eq!(status.phase, CellPhase::Blocked);
        assert_eq!(status.observed_generation, Some(4));
        assert_eq!(status.observed_desired_generation, 3);
        assert_eq!(status.observed_revision_digest, "sha256:current");
        assert_eq!(status.inventory_digest.as_deref(), Some("sha256:cleanup"));
        assert!(
            status
                .message
                .expect("reason")
                .contains("missing backend binding")
        );
    }

    #[test]
    fn cell_status_writes_are_semantic_at_large_scale() {
        use proofstorm_core::{
            ComponentCondition, ComponentConditionReason, ComponentConditionState,
            ComponentConditionType, ComponentKind, ComponentStatus, InventoryEntry,
        };

        let condition_types = [
            ComponentConditionType::WorkloadReady,
            ComponentConditionType::StorageReady,
            ComponentConditionType::CredentialsReady,
            ComponentConditionType::ServiceReady,
            ComponentConditionType::ProtocolReady,
            ComponentConditionType::DependenciesReady,
            ComponentConditionType::ComponentReady,
            ComponentConditionType::ExperimentControllable,
        ];
        let components = (0..150)
            .map(|index| ComponentStatus {
                protocol_observation: None,
                id: format!("wallet-{index}"),
                kind: ComponentKind::Wallet,
                observed_revision_digest: format!("sha256:{:064x}", index + 1),
                observed_rollout_digest: format!("sha256:{:064x}", index + 65),
                conditions: condition_types
                    .iter()
                    .map(|condition_type| ComponentCondition {
                        condition_type: *condition_type,
                        state: ComponentConditionState::Unknown,
                        reason: ComponentConditionReason::NotObserved,
                        message: "bounded readiness observation awaiting controller evidence"
                            .into(),
                        last_transition_unix: 1,
                    })
                    .collect(),
                ready: false,
                service: format!("wallet-{index}.instance.svc"),
                ports: BTreeMap::from([("http".into(), 3_338)]),
            })
            .collect::<Vec<_>>();
        let inventory = (0..150)
            .flat_map(|index| {
                (0..6).map(move |resource| InventoryEntry {
                    api_version: "apps/v1".into(),
                    kind: "StatefulSet".into(),
                    namespace: "proofstorm-i0123456789012345678".into(),
                    name: format!("wallet-{index}-resource-{resource}"),
                })
            })
            .collect();
        let status = ProofstormCellStatus {
            observed_desired_generation: 1,
            last_converged_revision: None,
            retained_storage: BTreeMap::new(),
            phase: CellPhase::Pending,
            instance_namespace: Some("proofstorm-i0123456789012345678".into()),
            observed_generation: Some(1),
            observed_revision_digest: "sha256:revision".into(),
            observed_protocol_probe_lease: Some("lease-current".into()),
            components,
            inventory,
            inventory_digest: Some(format!("sha256:{}", "a".repeat(64))),
            message: Some("waiting for protocol component readiness".into()),
            ..ProofstormCellStatus::default()
        };

        let encoded = serde_json::to_vec(&status).expect("large status");
        assert!(
            encoded.len() > 256 * 1024,
            "exercise the former cell-status ceiling"
        );
        assert_eq!(
            serde_json::from_slice::<ProofstormCellStatus>(&encoded).unwrap(),
            status
        );
        assert!(!cell_status_update_required(Some(&status), &status));
        let mut changed = status.clone();
        changed.phase = CellPhase::Ready;
        assert!(cell_status_update_required(Some(&status), &changed));

        let oversized = "x".repeat(MAX_ACTION_STATUS_BYTES);
        assert!(matches!(
            enforce_status_budget("ProofstormCellAction", &oversized, MAX_ACTION_STATUS_BYTES),
            Err(Error::StatusBudgetExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn a_probe_only_change_writes_just_the_heartbeat() {
        let requests = Arc::new(std::sync::Mutex::new(
            Vec::<(String, serde_json::Value)>::new(),
        ));
        let client = Client::new(
            tower::service_fn({
                let requests = requests.clone();
                move |request: http::Request<kube::client::Body>| {
                    let requests = requests.clone();
                    async move {
                        let uri = request.uri().to_string();
                        let body = request.into_body().collect_bytes().await.unwrap();
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        requests.lock().unwrap().push((uri, body));
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .header("content-type", "application/json")
                                .body(kube::client::Body::from(
                                    serde_json::to_vec(&ProofstormCell::new(
                                        "cell",
                                        proofstorm_kube::ProofstormCellSpec {
                                            workspace_id: "w".into(),
                                            instance_id: "i".into(),
                                            instance_key: "ikey".into(),
                                            revision_digest: "r".into(),
                                        },
                                    ))
                                    .unwrap(),
                                ))
                                .unwrap(),
                        )
                    }
                }
            }),
            "system",
        );
        let (probes, _receive) = probes::Manager::new(client.clone(), "prober".into());
        let context = Context::for_tests(client, probes);
        let heartbeat = |at| proofstorm_kube::ProtocolFreshness {
            observed_at_unix: at,
            expires_at_unix: at + 30,
        };
        let mut cell = ProofstormCell::new(
            "cell",
            proofstorm_kube::ProofstormCellSpec {
                workspace_id: "w".into(),
                instance_id: "i".into(),
                instance_key: "ikey".into(),
                revision_digest: "r".into(),
            },
        );
        cell.metadata.namespace = Some("system".into());
        cell.status = Some(ProofstormCellStatus {
            phase: CellPhase::Ready,
            protocol_freshness: Some(heartbeat(100)),
            ..ProofstormCellStatus::default()
        });
        let mut next = cell.status.clone().unwrap();
        next.protocol_freshness = Some(heartbeat(130));
        patch_status(&cell, &context, next.clone()).await.unwrap();
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].0.contains("fieldManager=proofstormd-heartbeat"));
            assert_eq!(
                requests[0].1["status"],
                serde_json::json!({"protocolFreshness": {"observedAtUnix": 130, "expiresAtUnix": 160}})
            );
        }
        // A real change writes the status without the heartbeat, which keeps
        // its own manager; an unchanged status writes nothing.
        next.phase = CellPhase::Pending;
        next.protocol_freshness = Some(heartbeat(100));
        patch_status(&cell, &context, next).await.unwrap();
        patch_status(&cell, &context, cell.status.clone().unwrap())
            .await
            .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].0.contains("fieldManager=proofstormd")
                && !requests[1].0.contains("proofstormd-heartbeat")
        );
        assert!(requests[1].1["status"].get("protocolFreshness").is_none());
        assert_eq!(requests[1].1["status"]["phase"], "Pending");
    }

    #[test]
    fn lifecycle_state_is_preserved_only_for_the_same_backend_state_identity() {
        let workload = |revision: &str, backend: &str, state_contract: &str| {
            serde_json::from_value::<StatefulSet>(serde_json::json!({
                "apiVersion": "apps/v1",
                "kind": "StatefulSet",
                "metadata": {
                    "name": "chain",
                    "annotations": {
                        BACKEND_ID_ANNOTATION: backend,
                        EXECUTION_STATE_CONTRACT_ANNOTATION: state_contract,
                        proofstorm_kube::REVISION_DIGEST_ANNOTATION: revision,
                    }
                },
                "spec": {
                    "selector": {"matchLabels": {"app": "chain"}},
                    "serviceName": "chain",
                    "template": {
                        "metadata": {"labels": {"app": "chain"}},
                        "spec": {"containers": [{"name": "component", "image": "example.invalid/image"}]}
                    }
                }
            }))
            .expect("stateful set")
        };
        let existing = workload("sha256:old", "bitcoin-core", "proofstorm/bitcoin-state/v1");
        let revised = workload("sha256:new", "bitcoin-core", "proofstorm/bitcoin-state/v1");
        assert!(same_lifecycle_identity(&existing, &revised));

        let replaced = workload("sha256:new", "bitcoin-core", "proofstorm/bitcoin-state/v2");
        assert!(!same_lifecycle_identity(&existing, &replaced));
        let backend_changed =
            workload("sha256:new", "other-bitcoin", "proofstorm/bitcoin-state/v1");
        assert!(!same_lifecycle_identity(&existing, &backend_changed));
    }

    #[test]
    fn live_exec_exit_status_is_preserved_as_experiment_data() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Status, StatusCause, StatusDetails};

        assert_eq!(
            exec_exit_code(Some(&Status {
                status: Some("Success".into()),
                ..Status::default()
            })),
            0
        );
        assert_eq!(
            exec_exit_code(Some(&Status {
                status: Some("Failure".into()),
                details: Some(StatusDetails {
                    causes: Some(vec![StatusCause {
                        message: Some("command terminated with exit code 42".into()),
                        ..StatusCause::default()
                    }]),
                    ..StatusDetails::default()
                }),
                ..Status::default()
            })),
            42
        );
        assert_eq!(exec_exit_code(None), 1);
    }

    fn network_action(
        sequence: u64,
        operation_id: &str,
        action: CellAction,
        phase: ActionPhase,
    ) -> ProofstormCellAction {
        let capability = match action {
            CellAction::NetworkPartition(_) => proofstorm_core::Capability::NetworkPartition,
            CellAction::NetworkHeal(_) => proofstorm_core::Capability::NetworkHeal,
            _ => panic!("network action"),
        };
        let mut resource = ProofstormCellAction::new(
            &format!("action-{sequence}"),
            proofstorm_kube::ProofstormCellActionSpec {
                access_scope: None,
                cell_name: "cell-1".into(),
                workspace_id: "workspace".into(),
                instance_id: "instance".into(),
                instance_key: "i0123456789012345678".into(),
                experiment_id: "experiment".into(),
                session_id: "session".into(),
                principal_id: "principal".into(),
                sequence,
                operation_id: operation_id.into(),
                request_digest: "sha256:request".into(),
                capability,
                accepted_at_unix: 1,
                action,
            },
        );
        resource.status = Some(ProofstormCellActionStatus {
            phase,
            ..ProofstormCellActionStatus::default()
        });
        resource
    }

    #[test]
    fn network_fault_state_is_ordered_composable_and_ignores_cancelled_actions() {
        let actions = vec![
            network_action(
                3,
                "partition-two",
                CellAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "mint".into(),
                    to_component: "wallet".into(),
                }),
                ActionPhase::Succeeded,
            ),
            network_action(
                1,
                "partition-one",
                CellAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "mint".into(),
                    to_component: "lightning".into(),
                }),
                ActionPhase::Succeeded,
            ),
            network_action(
                2,
                "cancelled-partition",
                CellAction::NetworkPartition(proofstorm_kube::NetworkPartitionAction {
                    from_component: "chain".into(),
                    to_component: "lightning".into(),
                }),
                ActionPhase::Cancelled,
            ),
            network_action(
                4,
                "heal-one",
                CellAction::NetworkHeal(proofstorm_kube::NetworkHealAction {
                    partition_operation_id: "partition-one".into(),
                }),
                ActionPhase::Succeeded,
            ),
        ];
        let before_heal = active_network_partitions(&actions, Some(3));
        assert!(before_heal.contains_key("partition-one"));
        assert!(before_heal.contains_key("partition-two"));
        assert!(!before_heal.contains_key("cancelled-partition"));
        let after_heal = active_network_partitions(&actions, None);
        assert!(!after_heal.contains_key("partition-one"));
        assert_eq!(
            after_heal.get("partition-two"),
            Some(&("mint".into(), "wallet".into()))
        );
    }
}
