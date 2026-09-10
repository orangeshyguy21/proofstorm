//! Cross-interface contracts use the same entry points as the native MCP and named CLI adapters.
use super::*;
use proofstorm_core::InstancePhase;

async fn native(labs: &Labs, id: &str) -> proofstorm_app::lab::AppliedLab {
    let plan = format!("{id}-plan");
    let spec = spec();
    labs.store
        .create_draft("local", "developer", &plan, &spec, &plan)
        .unwrap();
    labs.apply_plan(
        id,
        &plan,
        &proofstorm_core::digest_json(&spec),
        &format!("{id}-apply"),
    )
    .await
    .unwrap()
}

fn connection_target(cluster: &Arc<Mutex<Cluster>>, instance: &proofstorm_core::LabInstance) {
    let ns = proofstorm_kube::instance_namespace(&instance.instance_key);
    let mut api = cluster.lock().unwrap();
    api.objects.insert(format!("/api/v1/namespaces/{ns}/services/chain"), json!({
        "apiVersion":"v1","kind":"Service","metadata":{"name":"chain","labels":{"proofstorm.dev/instance":instance.instance_key}},"spec":{"ports":[{"port":18443}]}}));
    api.objects.insert(format!("/api/v1/namespaces/{ns}/pods"), json!({
        "apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"chain-0"},"status":{"conditions":[{"type":"Ready","status":"True"}]}}]}));
}

#[tokio::test]
async fn native_lab_supports_named_inspection_edit_connection_and_close_without_adoption() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let applied = native(&labs, "native-lab").await;
    assert!(
        store
            .lab_handle("local", "developer", "native-lab")
            .is_err()
    );
    let inspected = labs.inspect("native-lab", 0).await.unwrap();
    assert_eq!(inspected.runtime.unwrap().instance, applied.instance);
    assert!(inspected.run.is_none());
    assert!(
        store
            .lab_handle("local", "developer", "native-lab")
            .is_err(),
        "reads never create an alias or run"
    );
    connection_target(&cluster, &applied.instance);
    let connection = labs.connect("native-lab", "chain", "rpc", 0).await.unwrap();
    assert_eq!(connection.descriptor.lab, "native-lab");
    drop(connection);
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let edited = labs.up("native-lab", &changed).await.unwrap();
    let instance = edited.runtime.unwrap().instance;
    assert_eq!(instance.instance_key, applied.instance.instance_key);
    assert_eq!(instance.generation, 2);
    assert_eq!(
        cluster
            .lock()
            .unwrap()
            .objects
            .values()
            .filter(|v| v["kind"] == "ProofstormLab")
            .count(),
        1
    );
    let closed = labs.down("native-lab", 2).await.unwrap();
    assert!(
        closed
            .runtime
            .unwrap()
            .teardown_receipt
            .unwrap()
            .verified_absent
    );
    assert!(store.instance("local", "developer", "native-lab").is_err());
}

#[tokio::test]
async fn named_lab_accepts_native_apply_and_close_with_incarnation_fencing() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster);
    let initial = labs
        .up("named-lab", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    assert_eq!(labs.status("named-lab").await.unwrap().instance, initial);
    assert_eq!(labs.status(&initial.id).await.unwrap().instance, initial);
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let plan = labs.plan_edit("named-lab", &changed, false, &[]).unwrap();
    labs.store
        .save_update_plan("local", "developer", "native-edit", &plan)
        .unwrap();
    let edit = labs
        .apply_plan("named-lab", "native-edit", &plan.digest, "native-edit")
        .await
        .unwrap();
    assert_eq!(edit.instance.generation, 2);
    assert_eq!(edit.instance.instance_key, initial.instance_key);
    assert_eq!(
        labs.close("named-lab", "wrong-key")
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "stale_incarnation"
    );
    assert!(
        !labs
            .store
            .update_state("local", "developer", &initial.id)
            .unwrap()
            .closing
    );
    labs.close("named-lab", &initial.instance_key)
        .await
        .unwrap();
    let closed = labs
        .wait(proofstorm_app::lab::WaitRequest {
            reference: &initial.id,
            expected_instance_key: Some(&initial.instance_key),
            expected_generation: None,
            target_phase: InstancePhase::Closed,
            timeout_seconds: 2,
        })
        .await
        .unwrap();
    assert!(closed.reached);
    assert!(
        labs.store
            .lab_handle("local", "developer", "named-lab")
            .is_err()
    );
    let replacement = labs
        .up("named-lab", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    assert_eq!(
        labs.close("named-lab", &initial.instance_key)
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "stale_incarnation"
    );
    assert_eq!(
        labs.status("named-lab")
            .await
            .unwrap()
            .instance
            .instance_key,
        replacement.instance_key
    );
}

#[tokio::test]
async fn shutdown_uses_workspace_authority_and_collects_other_actors_results() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let instance = native(&labs, "shared-lab").await.instance;
    let op = labs
        .exec("shared-lab", "chain", command(), "actor-work")
        .await
        .unwrap();
    let action = cluster
        .lock()
        .unwrap()
        .objects
        .keys()
        .find(|p| p.contains("/proofstormlabactions/"))
        .unwrap()
        .clone();
    cluster.lock().unwrap().objects.get_mut(&action).unwrap()["status"] =
        json!({"phase":"Succeeded","artifact":{"exit_code":0,"cleanup_verified":true}});
    store.put_principal("closer").unwrap();
    store
        .grant("local", "closer", Capability::LabStatus)
        .unwrap();
    let closer = Labs::new(
        store.clone(),
        labs.runtime.clone(),
        "local".into(),
        "closer".into(),
    );
    assert!(
        closer
            .close("shared-lab", &instance.instance_key)
            .await
            .is_err()
    );
    assert!(
        !store
            .update_state("local", "developer", "shared-lab")
            .unwrap()
            .closing
    );
    store
        .grant("local", "closer", Capability::LabClose)
        .unwrap();
    closer
        .close("shared-lab", &instance.instance_key)
        .await
        .unwrap();
    assert_eq!(
        store.operation("local", "developer", &op.id).unwrap().phase,
        OperationPhase::Succeeded
    );
    assert_eq!(
        store
            .session("local", "developer", &op.session_id)
            .unwrap()
            .phase,
        proofstorm_core::SessionPhase::Finished
    );
    assert!(
        labs.exec("shared-lab", "chain", command(), "late-work")
            .await
            .is_err()
    );
    closer
        .close("shared-lab", &instance.instance_key)
        .await
        .unwrap();
}

#[tokio::test]
async fn wrong_cluster_never_latches_closing_or_deletes_the_lab() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let instance = native(&labs, "cluster-bound").await.instance;
    let other = Arc::new(Mutex::new(Cluster::default()));
    other.lock().unwrap().objects.insert("/api/v1/namespaces/kube-system".into(), json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"kube-system","uid":"another-cluster"}}));
    let error = service(store.clone(), other.clone())
        .close("cluster-bound", &instance.instance_key)
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "lab_cluster_mismatch");
    assert!(
        !store
            .update_state("local", "developer", "cluster-bound")
            .unwrap()
            .closing
    );
    assert!(
        other
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, _)| method == "GET")
    );
}

#[tokio::test]
async fn a_reused_native_name_cannot_be_closed_with_an_old_inspection() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster);
    let original = native(&labs, "reused-native").await.instance;
    labs.down("reused-native", 2).await.unwrap();
    let replacement = native(&labs, "reused-native").await.instance;
    assert_eq!(original.id, replacement.id);
    assert_ne!(original.instance_key, replacement.instance_key);
    let error = labs
        .down_checked("reused-native", 2, Some(&original.instance_key))
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "stale_incarnation");
    assert!(
        !labs
            .store
            .update_state("local", "developer", &replacement.id)
            .unwrap()
            .closing
    );
}

#[tokio::test]
async fn accepted_edit_during_outage_reports_admission_and_recovers_without_another_generation() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store, cluster.clone());
    let initial = labs
        .up("interrupted-edit", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    cluster.lock().unwrap().fail_reads = true;
    let error = labs
        .edit("interrupted-edit", &changed, false, &[])
        .await
        .unwrap_err();
    assert!(error.message.contains("Edit accepted at generation 2"));
    let details = error.details.unwrap();
    assert_eq!(details["accepted"], true);
    assert_eq!(details["generation"], 2);
    cluster.lock().unwrap().fail_reads = false;
    let recovered = labs
        .up("interrupted-edit", &changed)
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    assert_eq!(recovered.instance_key, initial.instance_key);
    assert_eq!(recovered.generation, 2);
}

#[tokio::test]
async fn ambiguous_names_fail_without_selecting_or_mutating_either_lab() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let labs = service(store.clone(), cluster.clone());
    let named = labs
        .up("ambiguous", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    // A legacy/raw store caller can create an ID that shadows an existing alias.
    let other = store
        .materialize(
            "local",
            "developer",
            "ambiguous",
            &named.revision_digest,
            "raw-shadow",
        )
        .unwrap();
    assert_ne!(other.id, named.id);
    let before = cluster.lock().unwrap().requests.len();
    let error = labs
        .close("ambiguous", &other.instance_key)
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "lab_reference_ambiguous");
    assert_eq!(cluster.lock().unwrap().requests.len(), before);
    assert!(
        !store
            .update_state("local", "developer", &other.id)
            .unwrap()
            .closing
    );
    assert!(
        !store
            .update_state("local", "developer", &named.id)
            .unwrap()
            .closing
    );
}

mod components;
