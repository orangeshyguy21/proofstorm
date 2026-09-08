//! Logs follow the container blocking startup, while retaining rollout context.
use std::{collections::BTreeMap, future::Future};

use k8s_openapi::api::core::v1::{ContainerState, ContainerStatus, Pod};
use kube::{Api, ResourceExt, api::LogParams};
use serde_json::{Value, json};

const LOG_LIMIT_BYTES: i64 = 12 * 1024;
const ARTIFACT_TARGET_BYTES: usize = 30 * 1024;

pub(super) async fn component_log_artifact(
    pods: &Api<Pod>,
    component: &str,
    tail_lines: u32,
    observed: Vec<Pod>,
) -> Result<BTreeMap<String, Value>, kube::Error> {
    collect_logs(component, tail_lines, observed, |name, params| async move {
        pods.logs(&name, &params).await
    })
    .await
}

fn state_summary(state: Option<&ContainerState>) -> Value {
    let Some(state) = state else {
        return Value::Null;
    };
    if let Some(waiting) = &state.waiting {
        json!({"state": "waiting", "reason": waiting.reason.as_deref().map(bounded_message),
            "message": waiting.message.as_deref().map(bounded_message)})
    } else if let Some(terminated) = &state.terminated {
        json!({"state": "terminated", "reason": terminated.reason.as_deref().map(bounded_message),
            "message": terminated.message.as_deref().map(bounded_message),
            "exit_code": terminated.exit_code, "signal": terminated.signal,
            "started_at": terminated.started_at, "finished_at": terminated.finished_at})
    } else if let Some(running) = &state.running {
        json!({"state": "running", "started_at": running.started_at})
    } else {
        Value::Null
    }
}

fn bounded_message(message: &str) -> String {
    message.chars().take(512).collect()
}

fn blocking_init(status: &ContainerStatus) -> bool {
    // Successful one-shot initializers remain unready forever; that is normal.
    !status.ready
        && status
            .state
            .as_ref()
            .and_then(|state| state.terminated.as_ref())
            .is_none_or(|terminated| terminated.exit_code != 0)
}

fn selected_container(pod: &Pod) -> (Option<String>, Option<&ContainerStatus>, bool) {
    let status = pod.status.as_ref();
    let init = status
        .and_then(|status| status.init_container_statuses.as_ref())
        .and_then(|statuses| statuses.iter().find(|status| blocking_init(status)));
    if let Some(init) = init {
        return (Some(init.name.clone()), Some(init), true);
    }
    let main = status
        .and_then(|status| status.container_statuses.as_ref())
        .and_then(|statuses| {
            statuses
                .iter()
                .find(|status| !status.ready)
                .or_else(|| statuses.first())
        });
    let name = main.map(|status| status.name.clone()).or_else(|| {
        pod.spec
            .as_ref()
            .and_then(|spec| spec.containers.first())
            .map(|container| container.name.clone())
    });
    (name, main, false)
}

fn pod_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

fn bound_artifact(artifact: &mut BTreeMap<String, Value>) {
    // JSON escaping can multiply the byte count. Bound the encoded body, not
    // just the raw API log, so diagnostics cannot stall action reconciliation.
    while serde_json::to_vec(artifact).is_ok_and(|bytes| bytes.len() > ARTIFACT_TARGET_BYTES) {
        let field = ["log", "previous_log"]
            .into_iter()
            .max_by_key(|field| {
                artifact
                    .get(*field)
                    .and_then(Value::as_str)
                    .map_or(0, str::len)
            })
            .expect("two log fields");
        let Some(Value::String(log)) = artifact.get_mut(field) else {
            break;
        };
        if log.is_empty() {
            break;
        }
        let start = log
            .char_indices()
            .map(|(index, _)| index)
            .find(|index| *index >= log.len().div_ceil(4))
            .unwrap_or(log.len());
        *log = log[start..].to_owned();
        artifact.insert(format!("{field}_truncated"), json!(true));
    }
}

/// Kubelet may finish a successful HTTP stream with this exact runtime error
/// when the previous container has already been garbage-collected. It is not
/// application output. Match only the complete, known runtime-ID form.
fn runtime_log_error(log: &str) -> bool {
    let Some(container) = log
        .trim()
        .strip_prefix("unable to retrieve container logs for ")
    else {
        return false;
    };
    ["containerd://", "docker://", "cri-o://"]
        .iter()
        .any(|prefix| {
            container
                .strip_prefix(prefix)
                .is_some_and(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
}

async fn read_log<F, Fut>(
    fetch: &mut F,
    name: &str,
    container: Option<&str>,
    tail_lines: u32,
    previous: bool,
) -> Result<(String, Option<Value>), kube::Error>
where
    F: FnMut(String, LogParams) -> Fut,
    Fut: Future<Output = Result<String, kube::Error>>,
{
    match fetch(
        name.to_owned(),
        LogParams {
            container: container.map(str::to_owned),
            tail_lines: Some(i64::from(tail_lines)),
            limit_bytes: Some(LOG_LIMIT_BYTES),
            previous,
            ..LogParams::default()
        },
    )
    .await
    {
        Ok(log) if runtime_log_error(&log) => Ok((
            String::new(),
            Some(json!({
                "code": "log_unavailable", "reason": "ContainerLogsUnavailable",
                "message": bounded_message(&log), "http_status": 200
            })),
        )),
        Ok(log) => Ok((log, None)),
        Err(kube::Error::Api(error)) => Ok((
            String::new(),
            Some(json!({"code": "log_unavailable", "http_status": error.code,
                "reason": bounded_message(&error.reason), "message": bounded_message(&error.message)})),
        )),
        Err(error) => Ok((
            String::new(),
            Some(json!({"code": "log_unavailable",
            "message": bounded_message(&error.to_string())})),
        )),
    }
}

async fn collect_logs<F, Fut>(
    component: &str,
    tail_lines: u32,
    mut observed: Vec<Pod>,
    mut fetch: F,
) -> Result<BTreeMap<String, Value>, kube::Error>
where
    F: FnMut(String, LogParams) -> Fut,
    Fut: Future<Output = Result<String, kube::Error>>,
{
    observed.sort_by(|left, right| {
        // Prefer a current rollout target to a newer pod being deleted.
        left.metadata
            .deletion_timestamp
            .is_some()
            .cmp(&right.metadata.deletion_timestamp.is_some())
            .then_with(|| right.creation_timestamp().cmp(&left.creation_timestamp()))
            .then_with(|| right.name_any().cmp(&left.name_any()))
    });
    let Some(pod) = observed.first() else {
        return Ok(super::status_object(
            json!({"component": component, "pod": null, "log": "",
            "log_truncated": false, "log_available": false, "diagnostic": "no_pod",
            "diagnostic_message": "the component currently has no Pod, so it has no log to read"}),
        ));
    };
    let (container, status, init) = selected_container(pod);
    let (log, unavailable) = read_log(
        &mut fetch,
        &pod.name_any(),
        container.as_deref(),
        tail_lines,
        false,
    )
    .await?;
    let has_previous = status.is_some_and(|status| {
        status.restart_count > 0
            || status
                .last_state
                .as_ref()
                .is_some_and(|state| state.terminated.is_some())
    });
    let (previous, previous_unavailable) = if has_previous {
        read_log(
            &mut fetch,
            &pod.name_any(),
            container.as_deref(),
            tail_lines,
            true,
        )
        .await?
    } else {
        (String::new(), None)
    };
    let summaries: Vec<_> = observed
        .iter()
        .take(8)
        .map(|pod| {
            json!({"pod": pod.name_any(),
        "ready": pod_ready(pod), "terminating": pod.metadata.deletion_timestamp.is_some(),
        "phase": pod.status.as_ref().and_then(|status| status.phase.as_ref())})
        })
        .collect();
    let mut artifact = super::status_object(json!({
        "component": component, "pod": pod.name_any(), "container": container,
        "container_kind": if init { "init" } else { "app" },
        "pod_phase": pod.status.as_ref().and_then(|status| status.phase.as_ref()),
        "pod_ready": pod_ready(pod), "container_ready": status.map(|status| status.ready),
        "restart_count": status.map(|status| status.restart_count),
        "container_state": state_summary(status.and_then(|status| status.state.as_ref())),
        "previous_container_state": state_summary(status.and_then(|status| status.last_state.as_ref())),
        "tail_lines": tail_lines, "log_truncated": log.len() >= usize::try_from(LOG_LIMIT_BYTES).unwrap_or(usize::MAX),
        "log": log, "log_available": unavailable.is_none(), "log_diagnostic": unavailable,
        "previous_log": previous, "previous_log_requested": has_previous,
        "previous_log_truncated": previous.len() >= usize::try_from(LOG_LIMIT_BYTES).unwrap_or(usize::MAX),
        "previous_log_available": has_previous && previous_unavailable.is_none(),
        "previous_log_diagnostic": previous_unavailable,
        "observed_pods": summaries, "observed_pod_count": observed.len(),
        "ready_pod_count": observed.iter().filter(|pod| pod_ready(pod) && pod.metadata.deletion_timestamp.is_none()).count(),
    }));
    bound_artifact(&mut artifact);
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn pod(name: &str, timestamp: &str, init: Value, main: Value) -> Pod {
        let mut document = json!({"metadata": {"name": name, "creationTimestamp": timestamp},
            "spec": {"containers": [{"name": "mint", "image": "mint"}]},
            "status": {"phase": "Pending"}});
        document["status"]["initContainerStatuses"] = init;
        document["status"]["containerStatuses"] = main;
        serde_json::from_value(document).unwrap()
    }

    fn container(name: &str, ready: bool, state: Value, previous: Value) -> Value {
        let mut document = json!({"name": name, "ready": ready,
            "restartCount": if previous.is_null() {0} else {3}, "image": "mint", "imageID": "mint"});
        document["state"] = state;
        document["lastState"] = previous;
        document
    }

    #[tokio::test]
    async fn failing_init_reads_previous_logs_and_reports_serving_rollout_pod() {
        let init = container(
            "configure",
            false,
            json!({"waiting": {"reason": "CrashLoopBackOff"}}),
            json!({"terminated": {"exitCode": 1, "reason": "Error", "message": "config rejected"}}),
        );
        let main = container(
            "mint",
            false,
            json!({"waiting": {"reason": "PodInitializing"}}),
            Value::Null,
        );
        let failed = pod("new", "2026-09-07T00:00:01Z", json!([init]), json!([main]));
        let mut old = pod("old", "2026-09-07T00:00:00Z", json!([]), json!([]));
        old.status.as_mut().unwrap().conditions = Some(vec![
            serde_json::from_value(json!({"type": "Ready", "status": "True"})).unwrap(),
        ]);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let result = collect_logs("mint", 50, vec![old, failed], move |name, params| {
            captured
                .lock()
                .unwrap()
                .push((name, params.container.clone(), params.previous));
            async move {
                Ok(if params.previous {
                    "config rejected"
                } else {
                    ""
                }
                .to_owned())
            }
        })
        .await
        .unwrap();
        assert_eq!(result["container"], "configure");
        assert_eq!(result["container_kind"], "init");
        assert_eq!(result["container_state"]["reason"], "CrashLoopBackOff");
        assert_eq!(result["previous_container_state"]["exit_code"], 1);
        assert_eq!(result["restart_count"], 3);
        assert_eq!(result["previous_log"], "config rejected");
        assert_eq!(result["ready_pod_count"], 1);
        assert_eq!(result["pod_ready"], false);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                ("new".into(), Some("configure".into()), false),
                ("new".into(), Some("configure".into()), true)
            ]
        );
    }

    #[tokio::test]
    async fn unavailable_logs_preserve_api_reason_and_state() {
        for code in [400, 403, 404, 500] {
            let init = container(
                "configure",
                false,
                json!({"waiting": {"reason": "ImagePullBackOff", "message": "image does not exist"}}),
                Value::Null,
            );
            let result = collect_logs(
                "mint",
                50,
                vec![pod("new", "2026-09-07T00:00:01Z", json!([init]), json!([]))],
                move |_, _| async move {
                    Err(kube::Error::Api(Box::new(
                        kube::core::Status::failure("container waiting to start", "BadRequest")
                            .with_code(code),
                    )))
                },
            )
            .await
            .unwrap();
            assert_eq!(result["log"], "");
            assert_eq!(result["log_available"], false);
            assert_eq!(result["log_diagnostic"]["http_status"], code);
            assert_eq!(
                result["log_diagnostic"]["message"],
                "container waiting to start"
            );
            assert_eq!(result["container_state"]["reason"], "ImagePullBackOff");
        }
    }

    #[tokio::test]
    async fn runtime_error_inside_successful_http_stream_is_unavailable() {
        let message = format!(
            "unable to retrieve container logs for containerd://{}",
            "d".repeat(64)
        );
        let captured = message.clone();
        let result = collect_logs(
            "mint",
            50,
            vec![pod("new", "2026-09-07T00:00:01Z", json!([]), json!([]))],
            move |_, _| {
                let message = captured.clone();
                async move { Ok(message) }
            },
        )
        .await
        .unwrap();
        assert_eq!(result["log_available"], false);
        assert_eq!(result["log_diagnostic"]["http_status"], 200);
        assert_eq!(result["log_diagnostic"]["message"], message);
        assert!(!runtime_log_error(
            "application error: failed to retrieve logs"
        ));
        assert!(!runtime_log_error(&format!(
            "application output\n{message}"
        )));
        assert!(!runtime_log_error(
            "unable to retrieve container logs for some application"
        ));
    }

    #[test]
    fn successful_initializers_do_not_hide_main_container_failures() {
        let done = container(
            "configure",
            false,
            json!({"terminated": {"exitCode": 0}}),
            Value::Null,
        );
        let failed = container(
            "mint",
            false,
            json!({"terminated": {"exitCode": 137, "reason": "OOMKilled"}}),
            Value::Null,
        );
        let pod = pod(
            "new",
            "2026-09-07T00:00:01Z",
            json!([done]),
            json!([failed]),
        );
        let (name, status, init) = selected_container(&pod);
        assert_eq!(name.as_deref(), Some("mint"));
        assert!(!init);
        assert_eq!(
            state_summary(status.unwrap().state.as_ref())["reason"],
            "OOMKilled"
        );
    }

    #[tokio::test]
    async fn no_pod_does_not_attempt_log_fetch() {
        let result = collect_logs("mint", 50, vec![], |_, _| async {
            panic!("no log request")
        })
        .await
        .unwrap();
        assert_eq!(result["diagnostic"], "no_pod");
    }

    #[test]
    fn fuzz_log_content_stays_within_encoded_artifact_budget() {
        for text in ["\0", "🦀", "\"", "\\", "\n", "ordinary log line\n"] {
            for length in [0, 1, 12_288, 40_000] {
                let mut artifact = super::super::status_object(json!({"log": text.repeat(length),
                    "previous_log": text.repeat(length), "container_state": {"reason": "Error"}}));
                bound_artifact(&mut artifact);
                assert!(serde_json::to_vec(&artifact).unwrap().len() <= ARTIFACT_TARGET_BYTES);
                assert_eq!(artifact["container_state"]["reason"], "Error");
            }
        }
    }
}
