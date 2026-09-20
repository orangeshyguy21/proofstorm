use super::*;

#[tokio::test]
async fn edit_preparation_resumes_existing_publication_receipts_and_rechecks_permissions() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let handle = cells.up("prepare-edit", &spec()).await.unwrap().cell;
    let mut desired = spec();
    desired.components[0]
        .config
        .insert("txindex".into(), json!(false));
    let id = format!(
        "edit-{}",
        &proofstorm_core::digest_json(&(
            &handle.instance_id,
            1_u64,
            &desired,
            true,
            Vec::<String>::new()
        ))[7..39]
    );

    // Seed the exact stage receipts used before shared preparation. An interruption
    // after publication must resume this revision even if the draft later changes.
    store
        .create_draft("local", "developer", &id, &desired, &format!("{id}:draft"))
        .unwrap();
    let published = store
        .publish("local", "developer", &id, 1, &format!("{id}:publish"))
        .unwrap();
    store
        .edit_draft("local", "developer", &id, 1, &spec(), "later-draft-edit")
        .unwrap();
    let request_count = cluster.lock().unwrap().requests.len();
    store
        .revoke("local", "developer", Capability::CellPublish)
        .unwrap();
    let denied = cells
        .plan_edit("prepare-edit", &desired, true, &[])
        .unwrap_err();
    assert_eq!(denied.details.unwrap()["code"], "access_denied");
    assert!(
        store
            .update_plan("local", "developer", &id)
            .unwrap()
            .is_none()
    );

    store
        .grant("local", "developer", Capability::CellPublish)
        .unwrap();
    let plan = cells
        .plan_edit("prepare-edit", &desired, true, &[])
        .unwrap();
    assert_eq!(plan.target_revision, published.digest);
    assert_eq!(plan.target.instance_id, handle.instance_id);
    assert_eq!(plan.target.expected_generation, 1);
    assert!(plan.target.delete_data);
    assert_eq!(
        store.update_plan("local", "developer", &id).unwrap(),
        Some(plan)
    );
    assert_eq!(
        store.read_draft("local", "developer", &id).unwrap().version,
        2
    );
    assert_eq!(
        store
            .instance("local", "developer", &handle.instance_id)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(cluster.lock().unwrap().requests.len(), request_count);
}
