//! The controller schedules this Deployment independently of the zero-scaled template.
use k8s_openapi::api::apps::v1::Deployment;
use kube::{Api, Client};
use proofstorm_kube::{INSTANCE_LABEL, PROTOCOL_PROBER_NAME, instance_namespace};
use proofstorm_view::WorkloadObservation;
use std::time::Duration;

pub(super) async fn observe(
    client: Client,
    instance_key: &str,
) -> Option<(i32, WorkloadObservation)> {
    observe_with_timeout(client, instance_key, Duration::from_secs(3)).await
}

async fn observe_with_timeout(
    client: Client,
    instance_key: &str,
    timeout: Duration,
) -> Option<(i32, WorkloadObservation)> {
    let deployments = Api::<Deployment>::namespaced(client, &instance_namespace(instance_key));
    let deployment = tokio::time::timeout(timeout, deployments.get_opt(PROTOCOL_PROBER_NAME))
        .await
        .ok()?
        .ok()??;
    project(&deployment, instance_key)
}

fn project(deployment: &Deployment, instance_key: &str) -> Option<(i32, WorkloadObservation)> {
    if deployment.metadata.name.as_deref() != Some(PROTOCOL_PROBER_NAME)
        || deployment.metadata.namespace.as_deref() != Some(&instance_namespace(instance_key))
        || deployment.metadata.labels.as_ref()?.get(INSTANCE_LABEL)? != instance_key
        || deployment.metadata.deletion_timestamp.is_some()
    {
        return None;
    }
    let status = deployment.status.as_ref();
    Some((
        deployment.spec.as_ref()?.replicas.unwrap_or(1),
        WorkloadObservation {
            generation: deployment.metadata.generation,
            observed_generation: status.and_then(|s| s.observed_generation),
            replicas: status.and_then(|s| s.replicas),
            ready_replicas: status.and_then(|s| s.ready_replicas),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deployment() -> Deployment {
        serde_json::from_value(json!({
            "metadata": {"name": PROTOCOL_PROBER_NAME, "namespace": instance_namespace("demo"),
                "labels": {INSTANCE_LABEL: "demo"}, "generation": 3},
            "spec": {"replicas": 1, "selector": {}, "template": {"metadata": {}}},
            "status": {"observedGeneration": 2, "replicas": 1, "readyReplicas": 1}
        }))
        .unwrap()
    }

    #[test]
    fn reports_live_scale_and_preserves_status_freshness() {
        let mut deployment = deployment();
        let (desired, observed) = project(&deployment, "demo").unwrap();
        assert_eq!(desired, 1);
        assert_eq!(observed.replicas, Some(1));
        assert_eq!(observed.ready_replicas, Some(1));
        assert_ne!(observed.generation, observed.observed_generation);
        deployment.spec.as_mut().unwrap().replicas = Some(0);
        deployment.status = None;
        let (desired, observed) = project(&deployment, "demo").unwrap();
        assert_eq!(desired, 0);
        assert_eq!(observed.replicas, None);
        assert_eq!(observed.ready_replicas, None);
    }

    #[test]
    fn ignores_foreign_or_malformed_deployments() {
        assert!(project(&deployment(), "another-cell").is_none());
        let mut deployment = deployment();
        deployment.metadata.namespace = Some("foreign".into());
        assert!(project(&deployment, "demo").is_none());
        deployment.metadata.namespace = Some(instance_namespace("demo"));
        deployment.metadata.labels = None;
        assert!(project(&deployment, "demo").is_none());
    }

    #[tokio::test]
    async fn missing_denied_failed_and_timed_out_reads_stay_unknown() {
        use http::{Request, Response};
        use kube::client::Body;
        use std::{convert::Infallible, future::pending};
        for status in [Some(404), Some(403), Some(503), None] {
            let client = Client::new(
                tower::service_fn(move |request: Request<Body>| async move {
                    assert_eq!(request.method(), "GET");
                    assert_eq!(
                        request.uri().path(),
                        format!(
                            "/apis/apps/v1/namespaces/{}/deployments/{PROTOCOL_PROBER_NAME}",
                            instance_namespace("demo")
                        )
                    );
                    let Some(status) = status else {
                        return pending().await;
                    };
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(Body::from(
                                json!({"apiVersion":"v1","kind":"Status","status":"Failure",
                        "code":status,"reason":"Unavailable","message":"private diagnostic"})
                                .to_string()
                                .into_bytes(),
                            ))
                            .unwrap(),
                    )
                }),
                "default",
            );
            assert!(
                observe_with_timeout(client, "demo", Duration::from_millis(20))
                    .await
                    .is_none()
            );
        }
    }
}
