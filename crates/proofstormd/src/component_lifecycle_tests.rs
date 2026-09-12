use super::*;
use http::{Request, Response};
use kube::{Client, client::Body};
use proofstorm_kube::render_cell;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

struct Cluster {
    workload: Value,
    action: ProofstormCellAction,
    patches: Vec<Value>,
    pods: Vec<Value>,
    conflict: bool,
}
fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            merge(target.entry(key).or_insert(Value::Null), value);
        }
    } else {
        *target = patch.clone();
    }
}
fn client(cluster: Arc<Mutex<Cluster>>) -> Client {
    Client::new(
        tower::service_fn(move |request: Request<Body>| {
            let cluster = cluster.clone();
            async move {
                let route = request.uri().path().to_owned();
                let method = request.method().clone();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut c = cluster.lock().unwrap();
                let mut status = 200;
                let body = if route.ends_with("/status") {
                    let patch: Value = serde_json::from_slice(&bytes).unwrap();
                    c.action.status =
                        Some(serde_json::from_value(patch["status"].clone()).unwrap());
                    json!(c.action)
                } else if route.ends_with("/pods") {
                    json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":c.pods})
                } else if method == http::Method::PATCH {
                    let patch: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        patch["metadata"]["resourceVersion"],
                        c.workload["metadata"]["resourceVersion"]
                    );
                    c.patches.push(patch.clone());
                    if c.conflict {
                        c.conflict = false;
                        c.workload["metadata"]["resourceVersion"] = json!("newer");
                        c.workload["metadata"]["annotations"][LIFECYCLE_SEQUENCE_ANNOTATION] =
                            json!("20");
                        c.workload["metadata"]["annotations"][LIFECYCLE_STATE_ANNOTATION] =
                            json!("stopped");
                        c.workload["spec"]["replicas"] = json!(0);
                        status = 409;
                        json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","message":"another actor stopped the component","code":409})
                    } else {
                        merge(&mut c.workload, &patch);
                        c.workload["metadata"]["generation"] = json!(5);
                        c.workload["metadata"]["resourceVersion"] = json!("mutated");
                        c.workload.clone()
                    }
                } else {
                    c.workload.clone()
                };
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "system",
    )
}
fn fixture(
    deployment: bool,
    control: Control,
) -> (
    proofstorm_core::ComponentPlanContract,
    Arc<Mutex<Cluster>>,
    Context,
) {
    let spec: proofstorm_core::CellSpec =
        serde_json::from_str(include_str!("../../../examples/developer-cell.json")).unwrap();
    let lock = proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
    let key = "i0123456789012345678";
    let rendered = render_cell(key, "revision", &spec, &lock).unwrap();
    let component = if deployment { "mint" } else { "chain" };
    let mut workload = if deployment {
        json!(
            rendered
                .deployments
                .iter()
                .find(|w| w.name_any() == component)
                .unwrap()
        )
    } else {
        json!(
            rendered
                .stateful_sets
                .iter()
                .find(|w| w.name_any() == component)
                .unwrap()
        )
    };
    workload["metadata"]["resourceVersion"] = json!("42");
    workload["metadata"]["generation"] = json!(4);
    let mut action = ProofstormCellAction::new(
        "action",
        proofstorm_kube::ProofstormCellActionSpec {
            access_scope: None,
            cell_name: "cell".into(),
            workspace_id: "local".into(),
            instance_id: "cell".into(),
            instance_key: key.into(),
            experiment_id: "run".into(),
            session_id: "session".into(),
            principal_id: "actor".into(),
            sequence: 10,
            operation_id: "control".into(),
            request_digest: "digest".into(),
            capability: proofstorm_core::Capability::ComponentControl,
            accepted_at_unix: 1,
            action: match control {
                Control::Start => {
                    CellAction::ComponentStart(proofstorm_kube::ComponentControlAction {
                        component: component.into(),
                    })
                }
                Control::Stop => {
                    CellAction::ComponentStop(proofstorm_kube::ComponentControlAction {
                        component: component.into(),
                    })
                }
                Control::Restart => {
                    CellAction::ComponentRestart(proofstorm_kube::ComponentControlAction {
                        component: component.into(),
                    })
                }
            },
        },
    );
    action.metadata.namespace = Some("system".into());
    action.status = Some(ProofstormCellActionStatus {
        phase: ActionPhase::Running,
        started_at_unix: Some(1),
        ..Default::default()
    });
    let cluster = Arc::new(Mutex::new(Cluster {
        workload,
        action,
        patches: vec![],
        pods: vec![],
        conflict: false,
    }));
    let context = Context {
        client: client(cluster.clone()),
    };
    (
        rendered
            .plans
            .into_iter()
            .find(|p| p.component_id == component)
            .unwrap(),
        cluster,
        context,
    )
}
async fn tick(
    plan: &proofstorm_core::ComponentPlanContract,
    cluster: &Arc<Mutex<Cluster>>,
    context: &Context,
    control: Control,
) {
    let action = cluster.lock().unwrap().action.clone();
    let namespace = instance_namespace(&action.spec.instance_key);
    match plan.workload.kind {
        WorkloadControllerKind::Deployment => reconcile_workload(
            &Api::<Deployment>::namespaced(context.client.clone(), &namespace),
            &action,
            plan,
            control,
            context,
        )
        .await
        .unwrap(),
        WorkloadControllerKind::StatefulSet => reconcile_workload(
            &Api::<StatefulSet>::namespaced(context.client.clone(), &namespace),
            &action,
            plan,
            control,
            context,
        )
        .await
        .unwrap(),
    };
}
fn stopped_status(
    plan: &proofstorm_core::ComponentPlanContract,
    cluster: &Arc<Mutex<Cluster>>,
) -> BTreeSet<String> {
    let c = cluster.lock().unwrap();
    let deployments = if plan.workload.kind == WorkloadControllerKind::Deployment {
        vec![serde_json::from_value(c.workload.clone()).unwrap()]
    } else {
        vec![]
    };
    let stateful_sets = if plan.workload.kind == WorkloadControllerKind::StatefulSet {
        vec![serde_json::from_value(c.workload.clone()).unwrap()]
    } else {
        vec![]
    };
    let pods = c
        .pods
        .iter()
        .cloned()
        .map(|mut pod| {
            pod["metadata"]["labels"] = json!({COMPONENT_LABEL: plan.component_id});
            serde_json::from_value(pod).unwrap()
        })
        .collect::<Vec<_>>();
    observed_stops(
        std::slice::from_ref(plan),
        &ComponentObservationResources {
            deployments: &deployments,
            stateful_sets: &stateful_sets,
            pods: &pods,
            persistent_volume_claims: &[],
            services: &[],
            endpoint_slices: &[],
        },
    )
}

#[tokio::test]
async fn interrupted_stops_wait_for_pods_and_never_delete_storage() {
    for deployment in [false, true] {
        let (plan, cluster, context) = fixture(deployment, Control::Stop);
        tick(&plan, &cluster, &context, Control::Stop).await;
        {
            let mut c = cluster.lock().unwrap();
            assert_eq!(c.workload["spec"]["replicas"], 0);
            assert_eq!(c.patches.len(), 1);
            assert!(
                c.patches[0]["spec"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .all(|k| k == "replicas")
            );
            c.workload["status"] = json!({"observedGeneration":5,"replicas":0});
            c.pods = vec![
                json!({"metadata":{"name":"terminating","deletionTimestamp":"2026-09-08T00:00:00Z"}}),
            ];
        }
        // A new reconciler recovers from the persisted annotation without another mutation.
        tick(
            &plan,
            &cluster,
            &Context {
                client: client(cluster.clone()),
            },
            Control::Stop,
        )
        .await;
        assert_eq!(
            cluster
                .lock()
                .unwrap()
                .action
                .status
                .as_ref()
                .unwrap()
                .phase,
            ActionPhase::Running
        );
        assert!(
            stopped_status(&plan, &cluster).is_empty(),
            "observation must not report stopped while pods remain"
        );
        cluster.lock().unwrap().pods.clear();
        assert!(stopped_status(&plan, &cluster).contains(&plan.component_id));
        tick(&plan, &cluster, &context, Control::Stop).await;
        let c = cluster.lock().unwrap();
        assert_eq!(
            c.action.status.as_ref().unwrap().phase,
            ActionPhase::Succeeded
        );
        assert_eq!(c.patches.len(), 1);
    }
}
#[tokio::test]
async fn competing_control_conflicts_cannot_overwrite_a_newer_stop() {
    for deployment in [false, true] {
        let (plan, cluster, context) = fixture(deployment, Control::Restart);
        cluster.lock().unwrap().conflict = true;
        tick(&plan, &cluster, &context, Control::Restart).await;
        tick(&plan, &cluster, &context, Control::Restart).await;
        let c = cluster.lock().unwrap();
        assert_eq!(c.workload["spec"]["replicas"], 0);
        assert_eq!(c.patches.len(), 1);
        assert_eq!(
            c.action.status.as_ref().unwrap().error.as_ref().unwrap()["code"],
            "lifecycle_action_superseded"
        );
    }
}
#[tokio::test]
async fn stopped_components_require_start_and_replayed_restarts_do_not_roll_again() {
    for deployment in [false, true] {
        let (plan, cluster, context) = fixture(deployment, Control::Restart);
        cluster.lock().unwrap().workload["spec"]["replicas"] = json!(0);
        tick(&plan, &cluster, &context, Control::Restart).await;
        assert_eq!(
            cluster
                .lock()
                .unwrap()
                .action
                .status
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()["code"],
            "component_not_running"
        );
        assert!(cluster.lock().unwrap().patches.is_empty());
        let (plan, cluster, context) = fixture(deployment, Control::Restart);
        tick(&plan, &cluster, &context, Control::Restart).await;
        tick(&plan, &cluster, &context, Control::Restart).await;
        assert_eq!(cluster.lock().unwrap().patches.len(), 1);
        cluster.lock().unwrap().workload["status"] = json!({"observedGeneration":5,"replicas":1,"readyReplicas":1,"updatedReplicas":1,"currentRevision":"new","updateRevision":"new"});
        tick(&plan, &cluster, &context, Control::Restart).await;
        assert_eq!(
            cluster
                .lock()
                .unwrap()
                .action
                .status
                .as_ref()
                .unwrap()
                .phase,
            ActionPhase::Succeeded
        );
    }
}

fn edit_stopped<K: LifecycleWorkload>(value: Value) -> Value {
    let existing: K = serde_json::from_value(value).unwrap();
    let mut desired = existing.clone();
    desired
        .meta_mut()
        .annotations
        .as_mut()
        .unwrap()
        .remove(LIFECYCLE_STATE_ANNOTATION);
    desired.set_replicas(Some(1));
    desired
        .template_mut()
        .unwrap()
        .metadata
        .as_mut()
        .unwrap()
        .annotations
        .as_mut()
        .unwrap()
        .insert(
            proofstorm_kube::ROLLOUT_DIGEST_ANNOTATION.into(),
            "edited".into(),
        );
    json!(preserve_observed(&existing, &desired))
}

#[tokio::test]
async fn stop_then_edit_then_start_retains_storage_and_uses_the_new_configuration() {
    for deployment in [false, true] {
        let (mut plan, cluster, context) = fixture(deployment, Control::Stop);
        tick(&plan, &cluster, &context, Control::Stop).await;
        {
            let mut c = cluster.lock().unwrap();
            let original_volumes = c.workload["spec"]["template"]["spec"]["volumes"].clone();
            c.workload = if deployment {
                edit_stopped::<Deployment>(c.workload.clone())
            } else {
                edit_stopped::<StatefulSet>(c.workload.clone())
            };
            assert_eq!(
                c.workload["spec"]["replicas"], 0,
                "editing must not resume a stopped workload"
            );
            assert_eq!(
                c.workload["spec"]["template"]["spec"]["volumes"],
                original_volumes
            );
            plan.rollout_digest = "edited".into();
            c.action.spec.sequence = 11;
            c.action.spec.operation_id = "start-after-edit".into();
            c.action.spec.action =
                CellAction::ComponentStart(proofstorm_kube::ComponentControlAction {
                    component: plan.component_id.clone(),
                });
        }
        tick(&plan, &cluster, &context, Control::Start).await;
        {
            let mut c = cluster.lock().unwrap();
            assert_eq!(c.workload["spec"]["replicas"], 1);
            assert_eq!(
                c.workload["metadata"]["annotations"][LIFECYCLE_STATE_ANNOTATION],
                "running"
            );
            assert_eq!(
                c.workload["spec"]["template"]["metadata"]["annotations"]
                    [proofstorm_kube::ROLLOUT_DIGEST_ANNOTATION],
                "edited"
            );
            c.workload["status"] = json!({"observedGeneration":5,"replicas":1,"readyReplicas":1,"updatedReplicas":1,"currentRevision":"edited","updateRevision":"edited"});
        }
        tick(&plan, &cluster, &context, Control::Start).await;
        assert_eq!(
            cluster
                .lock()
                .unwrap()
                .action
                .status
                .as_ref()
                .unwrap()
                .phase,
            ActionPhase::Succeeded
        );
    }
}
