//! Regression coverage for removal completing concurrently with background cleanup.
//! The real CLI removal/reconciliation code runs against the controlled Kubernetes fixture.
use super::*;

#[tokio::test]
async fn remove_reports_success_when_background_sweep_finishes_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let store = Store::open(&database).unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let instance = cells
        .up("remove-sweep-race", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    // A second database handle exercises the same cross-client lifecycle guard
    // used by a running GUI/MCP recovery loop.
    let observer_store = Store::open(&database).unwrap();
    let waiting = tokio::sync::Notify::new();
    let progress = |label: &str| {
        if label == "Waiting for workloads and storage to disappear" {
            waiting.notify_one();
        }
    };
    let remove = cells.down_with_progress("remove-sweep-race", 5, &progress);
    let reconcile = async {
        waiting.notified().await;
        proofstorm_app::lifecycle::sweep(&cells.runtime, &observer_store, "local", "developer", "")
            .await
            .unwrap();
        assert!(
            observer_store
                .instance("local", "developer", &instance.id)
                .is_err()
        );
        eprintln!(
            "REPRO sweep: background reconciliation verified absence and purged the local record before the next removal poll"
        );
    };
    let (removed, ()) = Box::pin(tokio::time::timeout(
        std::time::Duration::from_secs(10),
        async { tokio::join!(remove, reconcile) },
    ))
    .await
    .expect("reproduction must finish without a stalled interleaving");
    let absent = cells.runtime.verify_absent(instance.clone()).await.unwrap();
    assert!(absent.teardown_receipt.unwrap().verified_absent);
    assert!(store.instance("local", "developer", &instance.id).is_err());
    eprintln!(
        "REPRO sweep: original runtime resource and namespace are absent; removal error = {:?}",
        removed.as_ref().err()
    );
    let removed = removed.expect(
        "verified cleanup by another control client must make the original removal succeed",
    );
    assert_removed(removed, &instance);
}

#[tokio::test]
async fn remove_reports_success_when_finalizer_wins_repeated_delete() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let instance = cells
        .up("remove-finalizer-race", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    {
        let mut api = cluster.lock().unwrap();
        api.deletion_race = Some(DeletionRace::DeferFirstDelete);
    }
    // One user request. The first internal DELETE is accepted and leaves the CR
    // terminating. The controller completes between GET and the second DELETE.
    let removed = cells.down("remove-finalizer-race", 5).await;
    let resource_path = format!(
        "/apis/proofstorm.dev/v1alpha1/namespaces/system/proofstormcells/{}",
        instance.resource_name
    );
    {
        let api = cluster.lock().unwrap();
        assert_eq!(
            api.requests
                .iter()
                .filter(|(method, path)| method == "DELETE" && path == &resource_path)
                .count(),
            2,
            "one removal call internally repeats the Kubernetes DELETE"
        );
        assert!(api.deletion_race.is_none());
        assert!(!api.objects.contains_key(&resource_path));
    }
    let absent = cells.runtime.verify_absent(instance.clone()).await.unwrap();
    assert!(absent.teardown_receipt.unwrap().verified_absent);
    eprintln!(
        "REPRO finalizer: original runtime resource and namespace are absent after two internal DELETEs; removal error = {:?}",
        removed.as_ref().err()
    );
    proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
        .await
        .unwrap();
    assert!(store.instance("local", "developer", &instance.id).is_err());
    let removed = removed.expect(
        "a finalizer completing before repeated DELETE must make the original removal succeed",
    );
    assert_removed(removed, &instance);
}

fn assert_removed(view: proofstorm_app::cell::CellView, instance: &proofstorm_core::CellInstance) {
    assert_eq!(view.cell.phase, CellHandlePhase::Closed);
    let status = view.runtime.unwrap();
    assert_eq!(status.instance, *instance);
    assert_eq!(status.phase, InstancePhase::Closed);
    assert!(status.teardown_receipt.unwrap().verified_absent);
    assert!(view.run.is_none() && view.sessions.sessions.is_empty() && view.activity.is_empty());
}

fn namespace_path(instance: &proofstorm_core::CellInstance) -> String {
    format!(
        "/api/v1/namespaces/{}",
        proofstorm_kube::instance_namespace(&instance.instance_key)
    )
}

fn keep_namespace(api: &mut Cluster, instance: &proofstorm_core::CellInstance) {
    api.objects.insert(
        namespace_path(instance),
        json!({
            "apiVersion":"v1", "kind":"Namespace", "metadata":{
                "name":proofstorm_kube::instance_namespace(&instance.instance_key),
                "uid":"namespace-still-deleting", "deletionTimestamp":"2026-09-14T00:00:00Z"
            }
        }),
    );
    // A removed CR alone does not imply that its namespace has been collected.
    api.objects.remove(&format!(
        "/api/v1/namespaces/system/configmaps/proofstorm-teardown-{}",
        instance.instance_key
    ));
}

#[tokio::test]
async fn remove_404_keeps_waiting_for_remaining_namespace_and_then_recovers() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let instance = cells
        .up("namespace-pending", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    {
        let mut api = cluster.lock().unwrap();
        api.deletion_race = Some(DeletionRace::DeferFirstDelete);
        let tracked = instance.clone();
        api.after_cell_delete = Some(Box::new(move |api| keep_namespace(api, &tracked)));
    }
    let error = cells.down("namespace-pending", 1).await.unwrap_err();
    assert_eq!(error.details.unwrap()["code"], "cell_close_pending");
    assert!(store.instance("local", "developer", &instance.id).is_ok());
    assert!(
        store
            .update_state("local", "developer", &instance.id)
            .unwrap()
            .closing
    );
    let status = cells.runtime.close(instance.clone()).await.unwrap();
    assert_eq!(status.phase, InstancePhase::Closing);
    assert!(status.teardown_receipt.is_none());
    assert_eq!(
        cells
            .runtime
            .verify_absent(instance.clone())
            .await
            .unwrap_err()
            .details
            .unwrap()["code"],
        "cleanup_unverified"
    );
    assert!(
        cluster
            .lock()
            .unwrap()
            .objects
            .remove(&namespace_path(&instance))
            .is_some()
    );
    assert_removed(cells.down("namespace-pending", 2).await.unwrap(), &instance);
    assert!(store.instance("local", "developer", &instance.id).is_err());
}

#[tokio::test]
async fn remove_preserves_non_404_delete_errors_and_the_cell() {
    for code in [403, 503] {
        let store = Store::memory().unwrap();
        seed(&store);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        let cells = service(store.clone(), cluster.clone());
        let instance = cells
            .up("delete-error", &spec())
            .await
            .unwrap()
            .runtime
            .unwrap()
            .instance;
        cluster.lock().unwrap().delete_failure = Some(code);
        let error = cells.down("delete-error", 1).await.unwrap_err();
        assert_eq!(error.kind, proofstorm_app::ErrorKind::Failure);
        assert_eq!(error.details.unwrap()["http_status"], code);
        assert!(store.instance("local", "developer", &instance.id).is_ok());
        let api = cluster.lock().unwrap();
        assert!(
            api.objects
                .values()
                .any(|v| v["metadata"]["name"] == instance.resource_name)
        );
        assert_eq!(
            api.requests
                .iter()
                .filter(|(method, _)| method == "DELETE")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn remove_404_does_not_hide_failed_verification_reads() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let instance = cells
        .up("unavailable-after-delete", &spec())
        .await
        .unwrap()
        .runtime
        .unwrap()
        .instance;
    {
        let mut api = cluster.lock().unwrap();
        api.deletion_race = Some(DeletionRace::DeferFirstDelete);
        api.after_cell_delete = Some(Box::new(|api| api.fail_reads = true));
    }
    let error = cells.down("unavailable-after-delete", 2).await.unwrap_err();
    assert_eq!(error.kind, proofstorm_app::ErrorKind::Failure);
    assert_eq!(error.details.unwrap()["http_status"], 503);
    assert!(store.instance("local", "developer", &instance.id).is_ok());
}

#[tokio::test]
async fn remove_missing_record_requires_verified_absence() {
    for failed_reads in [false, true] {
        let store = Store::memory().unwrap();
        seed(&store);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        let cells = service(store.clone(), cluster.clone());
        let instance = cells
            .up("missing-record", &spec())
            .await
            .unwrap()
            .runtime
            .unwrap()
            .instance;
        let progress = |label: &str| {
            if label == "Waiting for workloads and storage to disappear" {
                // Inject a missing record while runtime verification is still needed.
                store.purge_cell(&instance).unwrap();
                let mut api = cluster.lock().unwrap();
                keep_namespace(&mut api, &instance);
                api.fail_reads = failed_reads;
            }
        };
        let error = cells
            .down_with_progress("missing-record", 1, &progress)
            .await
            .unwrap_err();
        let details = error.details.unwrap();
        if failed_reads {
            assert_eq!(error.kind, proofstorm_app::ErrorKind::Failure);
            assert_eq!(details["http_status"], 503);
        } else {
            assert_eq!(details["code"], "cell_close_pending");
        }
        assert!(
            cluster
                .lock()
                .unwrap()
                .objects
                .contains_key(&namespace_path(&instance))
        );
    }
}

#[tokio::test]
async fn remove_does_not_touch_a_replacement_created_between_polls() {
    // Named cells get a new ID when recreated; native cells can reuse the ID.
    for native_id in [false, true] {
        let store = Store::memory().unwrap();
        seed(&store);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        let cells = service(store.clone(), cluster.clone());
        let name = "replaced-during-removal";
        let original = if native_id {
            native(&cells, name).await.instance
        } else {
            cells
                .up(name, &spec())
                .await
                .unwrap()
                .runtime
                .unwrap()
                .instance
        };
        let waiting = tokio::sync::Notify::new();
        let progress = |label: &str| {
            if label == "Waiting for workloads and storage to disappear" {
                waiting.notify_one();
            }
        };
        let remove = cells.down_with_progress(name, 5, &progress);
        let replace = async {
            waiting.notified().await;
            proofstorm_app::lifecycle::sweep(&cells.runtime, &store, "local", "developer", "")
                .await
                .unwrap();
            if native_id {
                native(&cells, name).await.instance
            } else {
                cells
                    .up(name, &spec())
                    .await
                    .unwrap()
                    .runtime
                    .unwrap()
                    .instance
            }
        };
        let (removed, replacement) = Box::pin(tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async { tokio::join!(remove, replace) },
        ))
        .await
        .unwrap();
        assert_ne!(replacement.instance_key, original.instance_key);
        if native_id {
            assert_eq!(
                removed.unwrap_err().details.unwrap()["code"],
                "stale_incarnation"
            );
        } else {
            assert_removed(removed.unwrap(), &original);
        }
        assert_eq!(cells.resolve_instance(name).unwrap(), replacement);
        assert!(
            !store
                .update_state("local", "developer", &replacement.id)
                .unwrap()
                .closing
        );
        let api = cluster.lock().unwrap();
        assert!(
            api.objects
                .values()
                .any(|v| v["metadata"]["name"] == replacement.resource_name)
        );
        assert!(
            api.requests
                .iter()
                .filter(|(method, _)| method == "DELETE")
                .all(|(_, path)| !path.ends_with(&replacement.resource_name))
        );
    }
}
