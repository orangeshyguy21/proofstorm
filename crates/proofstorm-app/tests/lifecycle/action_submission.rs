use super::*;
use proofstorm_app::cell::{
    ComponentExecRequest, ComponentLogsRequest, NetworkHealRequest, NetworkPartitionRequest,
    NetworkProbeRequest, PrivateTransferRequest,
};
use proofstorm_core::{CellOperation, OperationKind};

fn fixture() -> (Cells, String, Arc<Mutex<Cluster>>) {
    let store = Store::memory().unwrap();
    seed(&store);
    for capability in [
        Capability::ComponentLogs,
        Capability::ComponentForensics,
        Capability::NetworkPartition,
        Capability::NetworkHeal,
        Capability::OracleRun,
    ] {
        store.grant("local", "developer", capability).unwrap();
    }
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store, cluster.clone());
    (cells, "legacy-run".into(), cluster)
}

async fn start(cells: &Cells, run: &str) -> String {
    let mut spec: CellSpec =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    let entry = proofstorm_core::default_catalog()
        .entries
        .iter()
        .find(|entry| entry.id == "nutshell-wallet")
        .unwrap();
    for id in ["wallet-a", "wallet-b"] {
        spec.components.push(ComponentSpec {
            id: id.into(),
            kind: entry.kind,
            implementation: entry.id.clone(),
            version: Some(entry.version.clone()),
            config_version: entry.config_version.clone(),
            control: ControlClass::Cell,
            config: BTreeMap::new(),
        });
    }
    let instance = cells.up("demo", &spec).await.unwrap().cell.instance_id;
    cells
        .store
        .create_experiment("local", "developer", run, &instance, run)
        .unwrap();
    instance
}

fn cases() -> [(OperationKind, Capability, &'static str, Value); 6] {
    [
        (
            OperationKind::ComponentLogs,
            Capability::ComponentLogs,
            "logs",
            json!({"component":"chain","tail_lines":20}),
        ),
        (
            OperationKind::ComponentForensics,
            Capability::ComponentForensics,
            "forensics",
            json!({"component":"chain","script":"true","timeout_seconds":10}),
        ),
        (
            OperationKind::NetworkPartition,
            Capability::NetworkPartition,
            "partition",
            json!({"from_component":"chain","to_component":"mint"}),
        ),
        (
            OperationKind::ReachabilityOracle,
            Capability::OracleRun,
            "probe",
            json!({"from_component":"chain","to_component":"mint","service":"http","timeout_seconds":2,"attempts":3}),
        ),
        (
            OperationKind::NetworkHeal,
            Capability::NetworkHeal,
            "heal",
            json!({"partition_operation_id":"partition"}),
        ),
        (
            OperationKind::PrivateTransfer,
            Capability::ComponentExecLive,
            "transfer",
            json!({"transfer":{"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536}}),
        ),
    ]
}

fn scoped(instance: &str, run: &str, id: &str, mut payload: Value) -> Value {
    payload.as_object_mut().unwrap().extend(
        json!({"name":instance,"run_id":run,"request_id":id})
            .as_object()
            .unwrap()
            .clone(),
    );
    payload
}

async fn submit(
    cells: &Cells,
    kind: OperationKind,
    payload: Value,
) -> Result<CellOperation, proofstorm_app::Error> {
    // Transport normalization sets this skipped Rust field before calling the service.
    macro_rules! request {
        ($ty:ty) => {{
            let mut request: $ty = serde_json::from_value(payload.clone()).unwrap();
            request.idempotency_key = payload["request_id"].as_str().unwrap().into();
            request
        }};
    }
    match kind {
        OperationKind::ComponentLogs => cells.component_logs(request!(ComponentLogsRequest)).await,
        OperationKind::ComponentForensics => {
            cells
                .component_forensics(request!(ComponentExecRequest))
                .await
        }
        OperationKind::NetworkPartition => {
            cells
                .network_partition(request!(NetworkPartitionRequest))
                .await
        }
        OperationKind::ReachabilityOracle => {
            cells.network_probe(request!(NetworkProbeRequest)).await
        }
        OperationKind::NetworkHeal => cells.network_heal(request!(NetworkHealRequest)).await,
        OperationKind::PrivateTransfer => {
            cells
                .private_transfer(request!(PrivateTransferRequest))
                .await
        }
        _ => panic!("unexpected fixture kind"),
    }
}

#[tokio::test]
async fn legacy_actions_resume_with_identical_requests_and_do_not_reapply_on_replay() {
    let (cells, run, cluster) = fixture();
    let instance = start(&cells, &run).await;
    for (kind, capability, id, parameters) in cases() {
        // Explicit pre-extraction payloads, independent of the moved DTO serializer.
        let payload = scoped(&instance, &run, id, parameters);
        let previous = cells
            .store
            .create_operation(
                "local",
                "developer",
                &instance,
                &run,
                "",
                id,
                kind,
                &payload,
                id,
                capability,
            )
            .unwrap();
        let submitted = submit(&cells, kind, payload.clone()).await.unwrap();
        assert_eq!(submitted.request, previous.request);
        assert_eq!(submitted.request_digest, previous.request_digest);
        assert_eq!(submitted.resource_name, previous.resource_name);
        assert_eq!(submitted.session_id, previous.session_id);
        assert_eq!(submitted.revision_digest, previous.revision_digest);
        assert_eq!(submitted.phase, OperationPhase::Running);
        let before = cluster.lock().unwrap().requests.len();
        assert_eq!(
            submit(&cells, kind, payload.clone()).await.unwrap(),
            submitted
        );
        assert_eq!(cluster.lock().unwrap().requests.len(), before);
        let terminal = cells
            .store
            .record_operation_result(
                "local",
                id,
                OperationPhase::Succeeded,
                json!({"fixture":true}),
            )
            .unwrap();
        assert_eq!(submit(&cells, kind, payload).await.unwrap(), terminal);
        assert_eq!(cluster.lock().unwrap().requests.len(), before);
    }
    let actions = cluster
        .lock()
        .unwrap()
        .objects
        .values()
        .filter(|v| v["kind"] == "ProofstormCellAction")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(actions.len(), 6);
    let forensics = actions
        .iter()
        .find(|v| v["spec"]["operationId"] == "forensics")
        .unwrap();
    let forensics: proofstorm_kube::ProofstormCellAction =
        serde_json::from_value(forensics.clone()).unwrap();
    let proofstorm_kube::CellAction::ComponentForensics(action) = forensics.spec.action else {
        panic!("forensics action")
    };
    assert_eq!(action.target_component, "chain");
}

#[tokio::test]
async fn permissions_and_invalid_inputs_fail_before_admission_or_runtime_access() {
    let (cells, run, cluster) = fixture();
    let instance = start(&cells, &run).await;
    let before = cluster.lock().unwrap().requests.len();
    for (kind, capability, id, parameters) in cases() {
        cells
            .store
            .revoke("local", "developer", capability)
            .unwrap();
        let error = submit(&cells, kind, scoped(&instance, &run, id, parameters))
            .await
            .unwrap_err();
        assert_eq!(error.details.unwrap()["code"], "access_denied");
        cells.store.grant("local", "developer", capability).unwrap();
        assert!(cells.store.operation("local", "developer", id).is_err());
    }
    for (kind, id, parameters) in [
        (
            OperationKind::ComponentLogs,
            "bad-logs",
            json!({"component":"chain","tail_lines":2001}),
        ),
        (
            OperationKind::ComponentForensics,
            "bad-forensics",
            json!({"component":"chain","target_component":"missing","script":"true","timeout_seconds":10}),
        ),
        (
            OperationKind::NetworkPartition,
            "bad-partition",
            json!({"from_component":"chain","to_component":"chain"}),
        ),
        (
            OperationKind::ReachabilityOracle,
            "bad-probe",
            json!({"from_component":"chain","to_component":"mint","service":"not-a-service","timeout_seconds":2,"attempts":3}),
        ),
        (
            OperationKind::PrivateTransfer,
            "bad-transfer",
            json!({"transfer":{"transferMethod":"prepare","component":"wallet-a","destinationComponent":"chain","maximumBytes":65536}}),
        ),
    ] {
        assert!(
            submit(&cells, kind, scoped(&instance, &run, id, parameters))
                .await
                .is_err()
        );
        assert!(cells.store.operation("local", "developer", id).is_err());
    }
    assert_eq!(cluster.lock().unwrap().requests.len(), before);
}

#[tokio::test]
async fn healing_requires_a_readable_succeeded_partition_in_the_same_cell_and_run() {
    let (cells, run, cluster) = fixture();
    let instance = start(&cells, &run).await;
    cells
        .store
        .create_experiment("local", "developer", "other-run", &instance, "other-run")
        .unwrap();
    let foreign = cells
        .up("other-cell", &spec())
        .await
        .unwrap()
        .cell
        .instance_id;
    let before = cluster.lock().unwrap().requests.len();
    for id in [
        "pending",
        "other-run",
        "other-cell",
        "wrong-kind",
        "unreadable",
    ] {
        let (source, source_run) = match id {
            "other-run" => (&instance, "other-run"),
            "other-cell" => (&foreign, ""),
            _ => (&instance, run.as_str()),
        };
        let (kind, capability) = if id == "wrong-kind" {
            (OperationKind::ComponentLogs, Capability::ComponentLogs)
        } else {
            (
                OperationKind::NetworkPartition,
                Capability::NetworkPartition,
            )
        };
        cells
            .store
            .create_operation(
                "local",
                "developer",
                source,
                source_run,
                "",
                id,
                kind,
                &json!({}),
                &format!("partition-fixture-{id}"),
                capability,
            )
            .unwrap();
        if id != "pending" {
            cells
                .store
                .record_operation_result("local", id, OperationPhase::Succeeded, json!({}))
                .unwrap();
        }
        if id == "unreadable" {
            cells
                .store
                .revoke("local", "developer", Capability::ArtifactRead)
                .unwrap();
        }
        let heal_id = format!("heal-{id}");
        let error = submit(
            &cells,
            OperationKind::NetworkHeal,
            scoped(
                &instance,
                &run,
                &heal_id,
                json!({"partition_operation_id":id}),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.details.unwrap()["code"],
            if id == "unreadable" {
                "access_denied"
            } else {
                "invalid_operation"
            }
        );
        cells
            .store
            .grant("local", "developer", Capability::ArtifactRead)
            .unwrap();
        assert!(
            cells
                .store
                .operation("local", "developer", &heal_id)
                .is_err()
        );
    }
    assert_eq!(cluster.lock().unwrap().requests.len(), before);
}

#[tokio::test]
async fn cancellation_and_revocation_during_grant_publication_prevent_action_submission() {
    for native in [false, true] {
        for revoke in [false, true] {
            let (recipient, cluster, request, grant) = native_submission::delegated_fixture().await;
            let store = recipient.store.clone();
            cluster.lock().unwrap().after_private_access = Some(Box::new(move || {
                if revoke {
                    store
                        .revoke_private_access("local", "developer", &grant.id)
                        .unwrap();
                } else {
                    store
                        .record_operation_result(
                            "local",
                            "receive",
                            OperationPhase::Cancelled,
                            json!({"cancelled":true}),
                        )
                        .unwrap();
                }
            }));
            let result = if native {
                recipient.execute_native(request).await
            } else {
                submit(
                    &recipient,
                    OperationKind::PrivateTransfer,
                    scoped(
                        &request.instance_id,
                        "",
                        "receive",
                        json!({"transfer":{"transferMethod":"status","component":"wallet","reference":"opaque-custody"}}),
                    ),
                )
                .await
            };
            if revoke {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().phase, OperationPhase::Cancelled);
            }
            let runtime = cluster.lock().unwrap();
            assert!(
                runtime.after_private_access.is_none(),
                "publication reached"
            );
            assert!(
                !runtime
                    .requests
                    .iter()
                    .any(|(_, path)| path.contains("/proofstormcellactions/"))
            );
        }
    }
}
