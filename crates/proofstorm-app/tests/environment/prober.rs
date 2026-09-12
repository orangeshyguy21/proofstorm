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

#[tokio::test]
async fn large_prober_containers_follow_component_pages_without_losing_counts() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db")).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster);
    let mut fleet = spec();
    let chain = fleet
        .components
        .iter()
        .find(|c| c.id == "chain")
        .unwrap()
        .clone();
    fleet.components = (0..150)
        .map(|i| {
            let mut component = chain.clone();
            component.id = format!("chain-{i:03}");
            component
        })
        .collect();
    fleet.links.clear();
    let cell = cells.up("fleet", &fleet).await.unwrap();
    let reader = observer(&cells);
    let mut query = EnvironmentQuery {
        instance_id: Some(cell.cell.instance_id),
        limit: 20,
        ..EnvironmentQuery::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let mut view = reader.environment(&query).await.unwrap();
        proofstorm_app::environment::bound_page_bytes(&mut view, 8 * 1024).unwrap();
        let cell = &view.cells.items[0];
        let prober = cell
            .resources
            .as_ref()
            .unwrap()
            .workloads
            .iter()
            .find(|w| w.name == PROTOCOL_PROBER_NAME)
            .unwrap();
        assert_eq!(prober.containers.len(), cell.components.items.len());
        assert_eq!(
            prober.containers.len() + prober.omitted_container_count,
            150
        );
        for container in &prober.containers {
            assert!(seen.insert(container.name.clone()));
        }
        if let Some(cursor) = &cell.components.next_cursor {
            query.component_cursor.clone_from(cursor);
        } else {
            break;
        }
    }
    assert_eq!(seen.len(), 150);
}
