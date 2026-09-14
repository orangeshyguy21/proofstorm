use super::*;

#[tokio::test]
async fn fenced_edits_reject_stale_writes_and_replay_without_rolling_back() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let original = cells
        .up_accepted("fenced", &spec(), None, None)
        .await
        .unwrap();
    let key = original.applied.instance.instance_key;
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let accepted = cells
        .up_accepted("fenced", &changed, Some(1), Some(&key))
        .await
        .unwrap();
    assert_eq!(accepted.applied.generation, 2);
    let view = cells.inspect("fenced", 0).await.unwrap();
    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json["cell"]["incarnation_generation"], 1);
    assert!(json["cell"].get("generation").is_none());
    assert_eq!(json["desired_generation"], 2);
    let mut legacy = json["cell"].clone();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("incarnation_generation");
    legacy["generation"] = json!(1);
    assert_eq!(
        serde_json::from_value::<proofstorm_store::CellHandle>(legacy).unwrap(),
        view.cell
    );
    let request_count = cluster.lock().unwrap().requests.len();
    let error = cells
        .up_accepted("fenced", &spec(), Some(1), Some(&key))
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "cell_update_conflict");
    assert_eq!(
        cluster.lock().unwrap().requests.len(),
        request_count,
        "stale edits cannot touch runtime"
    );
    let retry = cells
        .up_accepted("fenced", &changed, Some(1), Some(&key))
        .await
        .unwrap();
    assert_eq!(retry.applied.generation, 2);
    assert_eq!(retry.applied.instance.generation, 2);
    let latest = cells
        .up_accepted("fenced", &spec(), Some(2), Some(&key))
        .await
        .unwrap();
    assert_eq!(latest.applied.generation, 3);
    let replay = cells
        .up_accepted("fenced", &changed, Some(1), Some(&key))
        .await
        .unwrap();
    assert_eq!(replay.applied.generation, 2);
    assert_eq!(replay.applied.instance.generation, 3);
    assert_eq!(
        replay.applied.instance.revision_digest,
        latest.applied.instance.revision_digest
    );
}

#[tokio::test]
async fn accepted_up_survives_runtime_outage_and_preconditions_fence_replacement() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    let original = cells
        .up_accepted("outage", &spec(), None, None)
        .await
        .unwrap();
    let key = original.applied.instance.instance_key;
    let mut changed = spec();
    changed.components[0]
        .config
        .insert("txindex".into(), json!(false));
    cluster.lock().unwrap().fail_reads = true;
    let accepted = cells
        .up_accepted("outage", &changed, Some(1), Some(&key))
        .await
        .unwrap();
    assert_eq!(accepted.applied.generation, 2);
    assert!(accepted.applied.reconciliation_error.is_some());
    assert!(accepted.activity_ready);
    cluster.lock().unwrap().fail_reads = false;
    let retry = cells
        .up_accepted("outage", &changed, Some(1), Some(&key))
        .await
        .unwrap();
    assert_eq!(retry.applied.generation, 2);
    assert!(retry.applied.reconciliation_error.is_none());
    cells.down("outage", 2).await.unwrap();
    let replacement = cells
        .up_accepted("outage", &spec(), None, None)
        .await
        .unwrap();
    // Verified teardown deletes the handle, so its counter can start over.
    // Only the instance key identifies the replacement reliably.
    assert_ne!(replacement.applied.instance.instance_key, key);
    assert_eq!(replacement.applied.generation, 1);
    let request_count = cluster.lock().unwrap().requests.len();
    let error = cells
        .up_accepted("outage", &changed, Some(1), Some(&key))
        .await
        .unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "stale_incarnation");
    assert_eq!(cluster.lock().unwrap().requests.len(), request_count);
}
