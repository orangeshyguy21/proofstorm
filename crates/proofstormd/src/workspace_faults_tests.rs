use super::*;
use proofstorm_kube::{NetworkPartitionAction, ProofstormCellActionSpec, ProofstormCellSpec};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Cluster {
    cell: ProofstormCell,
    actions: BTreeMap<String, ProofstormCellAction>,
    policies: BTreeMap<String, Value>,
    fail_policy_once: bool,
}

fn client(cluster: Arc<Mutex<Cluster>>) -> kube::Client {
    kube::Client::new(
        tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let cluster = cluster.clone();
            async move {
                let route = request.uri().path().to_owned();
                let method = request.method().clone();
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut cluster = cluster.lock().unwrap();
                let name = route.split('/').next_back().unwrap();
                let mut code = 200;
                let body = if route.ends_with("/proofstormcells/cell") {
                    json!(cluster.cell)
                } else if route.ends_with("/proofstormcellactions") {
                    json!({"apiVersion":"proofstorm.dev/v1alpha1","kind":"ProofstormCellActionList","metadata":{},"items":cluster.actions.values().collect::<Vec<_>>()})
                } else if route.contains("/networkpolicies") {
                    let (status, response) = policy_request(&mut cluster, &method, name, &bytes);
                    code = status;
                    response
                } else if route.ends_with("/status") {
                    let action_name = route.split('/').rev().nth(1).unwrap();
                    let patch: Value = serde_json::from_slice(&bytes).unwrap();
                    let action = cluster.actions.get_mut(action_name).unwrap();
                    action.status = Some(serde_json::from_value(patch["status"].clone()).unwrap());
                    json!(action)
                } else if let Some(action) = cluster.actions.get_mut(name) {
                    if method == http::Method::PATCH {
                        let patch: Value = serde_json::from_slice(&bytes).unwrap();
                        for (key, value) in patch["metadata"]["annotations"].as_object().unwrap() {
                            action
                                .annotations_mut()
                                .insert(key.clone(), value.as_str().unwrap().into());
                        }
                    }
                    json!(action)
                } else {
                    code = 404;
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"missing","code":404})
                };
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(kube::client::Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "system",
    )
}

fn policy_request(
    cluster: &mut Cluster,
    method: &http::Method,
    name: &str,
    bytes: &[u8],
) -> (u16, Value) {
    if *method == http::Method::GET {
        return cluster.policies.get(name).map_or_else(|| (404,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"missing policy","code":404})), |policy| (200,policy.clone()));
    }
    let mut policy: Value = serde_json::from_slice(bytes).unwrap();
    let name = policy["metadata"]["name"].as_str().unwrap().to_owned();
    if cluster.fail_policy_once && name == "scripts" {
        cluster.fail_policy_once = false;
        return (
            503,
            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unavailable","message":"fixture partial policy write","code":503}),
        );
    }
    if *method == http::Method::PATCH {
        assert_eq!(
            policy["metadata"]["resourceVersion"],
            cluster.policies[&name]["metadata"]["resourceVersion"],
            "every policy write must fence its earlier read"
        );
    } else {
        assert!(!cluster.policies.contains_key(&name));
    }
    let next = cluster
        .policies
        .get(&name)
        .and_then(|old| old["metadata"]["resourceVersion"].as_str())
        .map_or(1, |version| version.parse::<u64>().unwrap() + 1);
    policy["metadata"]["resourceVersion"] = json!(next.to_string());
    cluster.policies.insert(name, policy.clone());
    (200, policy)
}

fn context(cluster: &Arc<Mutex<Cluster>>) -> Context {
    let client = client(cluster.clone());
    Context {
        probes: crate::probes::Manager::new(client.clone(), "fixture".into()).0,
        client,
    }
}

fn fixture() -> (Arc<Mutex<Cluster>>, Context) {
    let spec: proofstorm_core::CellSpec =
        serde_json::from_str(include_str!("../../../examples/workspace/cell.json")).unwrap();
    let lock = proofstorm_core::resolve_lock(&spec, proofstorm_core::default_catalog()).unwrap();
    let mut cell = ProofstormCell::new(
        "cell",
        ProofstormCellSpec {
            workspace_id: "local".into(),
            instance_id: "instance".into(),
            instance_key: "i0123456789012345678".into(),
            revision_digest: "revision".into(),
            lock,
            cell: spec,
        },
    );
    cell.metadata.namespace = Some("system".into());
    let mut actions = BTreeMap::new();
    for name in ["one", "two"] {
        let mut action = ProofstormCellAction::new(
            name,
            ProofstormCellActionSpec {
                access_scope: None,
                cell_name: cell.name_any(),
                workspace_id: cell.spec.workspace_id.clone(),
                instance_id: cell.spec.instance_id.clone(),
                instance_key: cell.spec.instance_key.clone(),
                experiment_id: String::new(),
                session_id: String::new(),
                principal_id: "owner".into(),
                sequence: 1,
                operation_id: name.into(),
                request_digest: "request".into(),
                capability: proofstorm_core::Capability::NetworkPartition,
                accepted_at_unix: now_unix(),
                action: CellAction::NetworkPartition(NetworkPartitionAction {
                    from_component: "chain".into(),
                    to_component: "scripts".into(),
                }),
            },
        );
        action.metadata.namespace = Some("system".into());
        action.annotations_mut().extend(BTreeMap::from([
            (
                super::super::workspace_bridge::PARENT_ANNOTATION.into(),
                "parent".into(),
            ),
            (EXPIRES_ANNOTATION.into(), (now_unix() + 60).to_string()),
            ("proofstorm.dev/action-revision".into(), "revision".into()),
        ]));
        action.status = Some(ProofstormCellActionStatus {
            phase: ActionPhase::Succeeded,
            ..Default::default()
        });
        actions.insert(name.into(), action);
    }
    let mut parent = actions["one"].clone();
    parent.metadata.name = Some("parent".into());
    parent.spec.action = CellAction::ComponentStart(proofstorm_kube::ComponentControlAction {
        component: "scripts".into(),
    });
    actions.insert("parent".into(), parent);
    let cluster = Arc::new(Mutex::new(Cluster {
        cell,
        actions,
        policies: BTreeMap::new(),
        fail_policy_once: false,
    }));
    let context = context(&cluster);
    (cluster, context)
}

#[tokio::test]
async fn partial_cleanup_recovers_without_replaying_or_removing_another_tasks_partition() {
    let (cluster, ctx) = fixture();
    let (cell, first) = {
        let cluster = cluster.lock().unwrap();
        (cluster.cell.clone(), cluster.actions["one"].clone())
    };
    super::super::apply_network_fault_policies(&cell, None, &ctx)
        .await
        .unwrap();
    cluster.lock().unwrap().fail_policy_once = true;
    assert!(release(&first, &cell, &ctx, "task_ended").await.is_err());
    {
        let cluster = cluster.lock().unwrap();
        assert!(
            cluster.actions["one"]
                .annotations()
                .contains_key(RELEASED_ANNOTATION)
        );
        assert!(needs_cleanup(&cluster.actions["one"]));
        assert!(is_active(&cluster.actions["two"], now_unix()));
    }
    // A fresh controller continues from the durable release marker.
    let ctx = context(&cluster);
    reconcile(&first, &ctx).await.unwrap();
    let cluster = cluster.lock().unwrap();
    assert!(!needs_cleanup(&cluster.actions["one"]));
    assert!(needs_cleanup(&cluster.actions["two"]));
    assert_eq!(
        cluster.policies["chain"]["spec"]["egress"][0]["to"][0]["podSelector"]["matchExpressions"]
            [0]["values"],
        json!(["scripts"])
    );
}

#[tokio::test]
async fn expiry_and_missing_owner_heal_without_workspace_access_and_stale_snapshots_cannot_restore()
{
    let (cluster, ctx) = fixture();
    let (cell, snapshot) = {
        let cluster = cluster.lock().unwrap();
        (
            cluster.cell.clone(),
            cluster.actions.values().cloned().collect::<Vec<_>>(),
        )
    };
    super::super::apply_network_fault_policies(&cell, None, &ctx)
        .await
        .unwrap();
    {
        let mut cluster = cluster.lock().unwrap();
        cluster
            .actions
            .get_mut("one")
            .unwrap()
            .annotations_mut()
            .insert(EXPIRES_ANNOTATION.into(), (now_unix() - 1).to_string());
        cluster.actions.remove("parent");
    }
    for name in ["one", "two"] {
        let action = cluster.lock().unwrap().actions[name].clone();
        reconcile(&action, &ctx).await.unwrap();
    }
    // Pre-release snapshots still contain both leases. Policy writers must read
    // the latest journal themselves instead of applying the caller's snapshot.
    assert_eq!(
        super::super::active_network_partitions(&snapshot, None).len(),
        2
    );
    assert!(
        super::super::apply_network_fault_policies(&cell, None, &ctx)
            .await
            .unwrap()
            .is_empty()
    );
    let cluster = cluster.lock().unwrap();
    for name in ["one", "two"] {
        assert!(!needs_cleanup(&cluster.actions[name]));
    }
    assert!(
        cluster.policies["chain"]["spec"]["egress"][0]["to"][0]["podSelector"]
            .get("matchExpressions")
            .is_none()
    );
    assert_eq!(
        cluster.actions["one"].annotations()[RELEASED_ANNOTATION],
        "expired"
    );
    assert_eq!(
        cluster.actions["two"].annotations()[RELEASED_ANNOTATION],
        "owner_missing"
    );
}

#[tokio::test]
async fn cancelling_before_activation_does_not_partition_and_cleanup_preserves_ordinary_faults() {
    let (cluster, ctx) = fixture();
    {
        let mut cluster = cluster.lock().unwrap();
        cluster.actions.remove("two");
        let mut ordinary = cluster.actions["one"].clone();
        ordinary.metadata.name = Some("ordinary".into());
        ordinary.spec.operation_id = "ordinary".into();
        ordinary.metadata.annotations = None;
        cluster.actions.insert("ordinary".into(), ordinary);
        let first = cluster.actions.get_mut("one").unwrap();
        first.status = None;
        first
            .annotations_mut()
            .insert(ACTION_CANCEL_ANNOTATION.into(), "stop".into());
    }
    let first = cluster.lock().unwrap().actions["one"].clone();
    reconcile(&first, &ctx).await.unwrap();
    let cluster = cluster.lock().unwrap();
    assert_eq!(
        cluster.actions["one"].status.as_ref().unwrap().phase,
        ActionPhase::Cancelled
    );
    assert!(!needs_cleanup(&cluster.actions["one"]));
    let active = super::super::active_network_partitions(
        &cluster.actions.values().cloned().collect::<Vec<_>>(),
        None,
    );
    assert_eq!(active.len(), 1);
    assert!(active.contains_key("ordinary"));
}
