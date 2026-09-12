use super::*;
use proofstorm_kube::{INSTANCE_LABEL, PROTOCOL_PROBER_NAME, instance_namespace};
use proofstorm_view::ReplicaPolicy;

#[tokio::test]
async fn scheduled_prober_reports_live_scale_without_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db")).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap();
    let key = cell.instance_key.unwrap();
    let namespace = instance_namespace(&key);
    let path = format!("/apis/apps/v1/namespaces/{namespace}/deployments/{PROTOCOL_PROBER_NAME}");
    let reader = observer(&cells);
    let read_start = cluster.lock().unwrap().requests.len();
    for scale in [None, Some(1), Some(0)] {
        if let Some(scale) = scale {
            cluster.lock().unwrap().objects.insert(
                path.clone(),
                json!({
                    "apiVersion": "apps/v1", "kind": "Deployment",
                    "metadata": {"name": PROTOCOL_PROBER_NAME, "namespace": namespace,
                        "labels": {INSTANCE_LABEL: key}, "generation": 5},
                    "spec": {"replicas": scale, "selector": {}, "template": {"metadata": {}}},
                    "status": {"observedGeneration": 5, "replicas": scale, "readyReplicas": scale}
                }),
            );
        }
        let view = reader
            .environment(&EnvironmentQuery::default())
            .await
            .unwrap();
        let resources = view.cells.items[0].resources.as_ref().unwrap();
        let prober = resources
            .workloads
            .iter()
            .find(|w| w.name == PROTOCOL_PROBER_NAME)
            .unwrap();
        assert_eq!(prober.kind, "Deployment");
        assert_eq!(prober.replica_policy, ReplicaPolicy::ControllerScheduled);
        assert_eq!(prober.replicas, scale);
        assert_eq!(
            prober.observation.as_ref().and_then(|o| o.ready_replicas),
            scale
        );
        let chain = resources
            .workloads
            .iter()
            .find(|w| w.component.as_deref() == Some("chain"))
            .unwrap();
        assert_eq!(chain.replicas, Some(1));
        assert_eq!(chain.replica_policy, ReplicaPolicy::Fixed);
        assert!(chain.observation.is_none());
    }
    cluster.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["labels"][INSTANCE_LABEL] =
        json!("foreign");
    let view = reader
        .environment(&EnvironmentQuery::default())
        .await
        .unwrap();
    let prober = view.cells.items[0]
        .resources
        .as_ref()
        .unwrap()
        .workloads
        .iter()
        .find(|w| w.name == PROTOCOL_PROBER_NAME)
        .unwrap();
    assert!(prober.replicas.is_none());
    assert!(prober.observation.is_none());
    assert!(
        cluster.lock().unwrap().requests[read_start..]
            .iter()
            .all(|(method, _)| method == "GET")
    );
}
