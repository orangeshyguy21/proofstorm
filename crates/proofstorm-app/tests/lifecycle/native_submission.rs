use super::*;
use proofstorm_app::cell::NativeExecutionRequest;
use proofstorm_core::private_io::{InputBinding, PayloadBinding};
use proofstorm_core::{OperationKind, PrivateReceiveCommand, PrivateTransferScope};

pub(super) fn request(
    instance: &str,
    component: &str,
    command: NativeCommand,
    id: &str,
) -> NativeExecutionRequest {
    NativeExecutionRequest {
        instance_id: instance.into(),
        experiment_id: String::new(),
        session_id: String::new(),
        operation_id: id.into(),
        idempotency_key: id.into(),
        component: component.into(),
        private_payload: None,
        script: command.script,
        argv: command.argv,
        timeout_seconds: command.timeout_seconds,
        output: command.output,
    }
}

#[tokio::test]
async fn both_legacy_request_shapes_resume_without_changing_identity() {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let cell = cells.up("demo", &spec()).await.unwrap().cell;
    store
        .create_experiment(
            "local",
            "developer",
            "explicit-run",
            &cell.instance_id,
            "run",
        )
        .unwrap();
    for (id, run) in [
        ("application", ""),
        ("mcp-implicit", ""),
        ("mcp-explicit", "explicit-run"),
    ] {
        // These are the two persisted shapes from before the shared submitter.
        // Do not derive the expected payload from the new request serialization.
        let payload = if id == "application" {
            json!({"component":"chain","script":"","argv":["bitcoin-cli","-help"],"timeout_seconds":10,"output":{"mode":"private","fields":[]}})
        } else {
            json!({"name":cell.instance_id,"run_id":run,"request_id":id,"component":"chain","script":"","argv":["bitcoin-cli","-help"],"timeout_seconds":10,"output":{"mode":"private","fields":[]}})
        };
        let previous = store
            .create_operation(
                "local",
                "developer",
                &cell.instance_id,
                run,
                "",
                id,
                OperationKind::ComponentExecLive,
                &payload,
                id,
                Capability::ComponentExecLive,
            )
            .unwrap();
        let mut explicit = request(&cell.instance_id, "chain", command(), id);
        explicit.experiment_id = run.into();
        let submitted = if id == "application" {
            cells.exec("demo", "chain", command(), id).await.unwrap()
        } else {
            // MCP native submission needs no general artifact-reading authority.
            store
                .revoke("local", "developer", Capability::ArtifactRead)
                .unwrap();
            let result = cells.execute_native(explicit.clone()).await.unwrap();
            store
                .grant("local", "developer", Capability::ArtifactRead)
                .unwrap();
            result
        };
        assert_eq!(submitted.phase, OperationPhase::Running);
        assert_eq!(submitted.request, previous.request);
        assert_eq!(submitted.request_digest, previous.request_digest);
        assert_eq!(submitted.resource_name, previous.resource_name);
        assert_eq!(submitted.session_id, previous.session_id);
        assert_eq!(submitted.experiment_id, previous.experiment_id);
        let terminal = store
            .record_operation_result(
                "local",
                id,
                OperationPhase::Succeeded,
                json!({"exit_code":0,"cleanup_verified":true}),
            )
            .unwrap();
        let requests_before = cluster.lock().unwrap().requests.len();
        let replay = if id == "application" {
            cells.exec("demo", "chain", command(), id).await.unwrap()
        } else {
            cells.execute_native(explicit.clone()).await.unwrap()
        };
        assert_eq!(replay, terminal);
        assert_eq!(cluster.lock().unwrap().requests.len(), requests_before);
        explicit.argv.push("changed".into());
        assert!(cells.execute_native(explicit).await.is_err());
        assert_eq!(cluster.lock().unwrap().requests.len(), requests_before);
    }
}

#[tokio::test]
async fn delegated_native_submission_preserves_custody_and_publishes_access_before_action() {
    let (recipient, cluster, mut native, grant) = delegated_fixture().await;
    let store = &recipient.store;
    let submitted = recipient.execute_native(native.clone()).await.unwrap();
    let resource_name = store
        .instance("local", "developer", &grant.instance_id)
        .unwrap()
        .resource_name;
    assert_private_action(
        &cluster.lock().unwrap(),
        &submitted.id,
        &grant,
        native.private_payload.as_ref(),
        &resource_name,
    );
    store
        .revoke_private_access("local", "developer", "receive-access")
        .unwrap();
    native.operation_id = "receive-revoked".into();
    native.idempotency_key = native.operation_id.clone();
    let requests_before = cluster.lock().unwrap().requests.len();
    assert!(recipient.execute_native(native).await.is_err());
    assert_eq!(cluster.lock().unwrap().requests.len(), requests_before);
}

pub(super) async fn delegated_fixture() -> (
    Cells,
    Arc<Mutex<Cluster>>,
    NativeExecutionRequest,
    proofstorm_core::PrivateAccessGrant,
) {
    let store = Store::memory().unwrap();
    seed(&store);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let cells = service(store.clone(), cluster.clone());
    let mut spec: CellSpec =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    let entry = proofstorm_core::default_catalog()
        .entries
        .iter()
        .find(|entry| entry.id == "nutshell-wallet")
        .unwrap()
        .clone();
    spec.components.push(ComponentSpec {
        id: "wallet".into(),
        kind: ComponentKind::Wallet,
        implementation: entry.id,
        version: Some(entry.version),
        config_version: entry.config_version,
        control: ControlClass::Cell,
        config: BTreeMap::new(),
    });
    let cell = cells.up("demo", &spec).await.unwrap().cell;
    store.put_principal("recipient").unwrap();
    store
        .grant("local", "recipient", Capability::ComponentExecLive)
        .unwrap();
    let approved = PrivateReceiveCommand {
        script: String::new(),
        argv: vec!["cashu".into(), "receive".into()],
        timeout_seconds: 10,
        input: InputBinding::Stdin,
    };
    let grant = store
        .issue_private_access(
            "local",
            "developer",
            "recipient",
            "receive-access",
            &cell.instance_id,
            &PrivateTransferScope {
                issuer_principal_id: "developer".into(),
                component: "wallet".into(),
                mint: "mint".into(),
                reference: "opaque-custody".into(),
                receive_command_digest: approved.digest(),
            },
            "receive-access",
        )
        .unwrap();
    let recipient = Cells::new(
        store.clone(),
        cells.runtime.clone(),
        "local".into(),
        "recipient".into(),
    );
    let mut native = request(
        &cell.instance_id,
        "wallet",
        NativeCommand {
            private_io: None,
            script: approved.script,
            argv: approved.argv,
            timeout_seconds: approved.timeout_seconds,
            output: NativeOutput::default(),
        },
        "receive",
    );
    native.private_payload = Some(PayloadBinding::Consume {
        reference: "opaque-custody".into(),
        input: InputBinding::Stdin,
    });
    (recipient, cluster, native, grant)
}

fn assert_private_action(
    runtime: &Cluster,
    operation_id: &str,
    grant: &proofstorm_core::PrivateAccessGrant,
    payload: Option<&PayloadBinding>,
    resource_name: &str,
) {
    let action = runtime
        .objects
        .values()
        .find(|object| object["spec"]["operationId"] == operation_id)
        .unwrap();
    assert_eq!(action["spec"]["accessScope"], json!(grant));
    let action: proofstorm_kube::ProofstormCellAction =
        serde_json::from_value(action.clone()).unwrap();
    let proofstorm_kube::CellAction::ComponentExecLive(action) = action.spec.action else {
        panic!("native action required")
    };
    assert_eq!(action.private_payload.as_ref(), payload);
    let publication = runtime
        .requests
        .iter()
        .rposition(|(method, path)| {
            method == "PATCH" && path.contains("/proofstormcells/") && path.ends_with(resource_name)
        })
        .unwrap();
    let submission = runtime
        .requests
        .iter()
        .position(|(method, path)| method == "PATCH" && path.contains("/proofstormcellactions/"))
        .unwrap();
    assert!(publication < submission);
}
