use super::*;
use proofstorm_app::cell::WorkspaceCaptureRequest;
use proofstorm_core::EvidenceBundle;
use proofstorm_core::workspace::evidence::{
    CapturedFile, TaskCapture, WorkspaceEvidence, WorkspaceEvidenceContent,
};

fn fixture() -> (
    Store,
    WorkspaceCaptureRequest,
    WorkspaceEvidence,
    CellInstance,
) {
    let store = tests::seeded_store();
    for cap in [
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::ComponentExecLive,
        Capability::ArtifactRead,
    ] {
        store.grant("alpha", "designer", cap).unwrap();
    }
    let spec: CellSpec =
        serde_json::from_str(include_str!("../../../examples/workspace/cell.json")).unwrap();
    store
        .create_draft("alpha", "designer", "capture", &spec, "draft")
        .unwrap();
    let revision = store
        .publish("alpha", "designer", "capture", 1, "publish")
        .unwrap();
    let instance = store
        .materialize(
            "alpha",
            "designer",
            "capture-instance",
            &revision.digest,
            "materialize",
        )
        .unwrap();
    store
        .create_experiment("alpha", "designer", "capture-run", &instance.id, "run")
        .unwrap();
    let request:WorkspaceCaptureRequest=serde_json::from_value(serde_json::json!({"name":"capture-instance","component":"scripts","run_id":"capture-run","request_id":"first-capture","selection":{"task_id":"miner","output_paths":["binary.dat"]}})).unwrap();
    let capture_id = request.capture_id("alpha", "designer");
    let start =
        serde_json::from_value(serde_json::json!({"task_id":"miner","script":"sleep 60"})).unwrap();
    let snapshot = TaskCapture {
        capture_id: capture_id.clone(),
        capture_request_digest: digest_json(&request),
        selection: request.selection.clone(),
        observed_at_unix: 1,
        task: serde_json::json!({"task_id":"miner","phase":"running","request_digest":digest_json(&start),"source_digest":digest_json(&serde_json::json!({}))}),
        request: start,
        files: vec![CapturedFile::from_bytes(
            "output/binary.dat".into(),
            0,
            &[0, 255, 128],
        )],
    };
    let evidence=WorkspaceEvidence::new(WorkspaceEvidenceContent{capture_id,run_id:request.run_id.clone(),principal_id:"designer".into(),instance_id:instance.id.clone(),instance_key:instance.instance_key.clone(),revision_digest:revision.digest,component:"scripts".into(),workspace_pod_uid:"pod".into(),snapshot,controller_observed_at_unix:2,controller_actions:vec![serde_json::json!({"action_id":"partition","status":{"phase":"Succeeded","artifact":{"cleanup_verified":true}}})]}).unwrap();
    (store, request, evidence, instance)
}

#[tokio::test]
async fn workspace_captures_export_offline_with_binary_bodies_and_exact_retry_identity() {
    let (store, request, evidence, instance) = fixture();
    store
        .record_workspace_capture("alpha", "designer", &digest_json(&request), &evidence)
        .unwrap();
    let mut later = evidence.content.clone();
    later.controller_observed_at_unix = 999;
    assert_eq!(
        store
            .record_workspace_capture(
                "alpha",
                "designer",
                &digest_json(&request),
                &WorkspaceEvidence::new(later).unwrap()
            )
            .unwrap(),
        evidence
    );
    store
        .close_experiment("alpha", "designer", "capture-run", "finish")
        .unwrap();
    let gateway = ProofstormMcp::new(store.clone(), "alpha", "designer")
        .unwrap()
        .offline();
    let receipt = gateway
        .proofstorm_workspace_capture(Parameters(request.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(receipt.digest, evidence.digest);
    let export = EvidenceExportRequest {
        experiment_id: "capture-run".into(),
        include_oracle_artifacts: false,
        artifact_operation_ids: vec![],
        include_content: false,
    };
    let bundle = gateway.build_evidence_bundle(&export).unwrap();
    assert_eq!(bundle.content.workspace_captures, vec![evidence.clone()]);
    assert_eq!(bundle.content.revisions.len(), 1);
    assert!(
        bundle.content.journal.is_empty(),
        "the continuing task does not become a run-owned active action"
    );
    let manifest = gateway
        .proofstorm_evidence_export(Parameters(export.clone()))
        .unwrap()
        .0;
    assert_eq!(manifest.workspace_capture_count, 1);
    assert_eq!(manifest.artifact_count, 0);
    assert_eq!(
        gateway
            .proofstorm_evidence_export(Parameters(export))
            .unwrap()
            .0,
        manifest
    );
    let section = gateway
        .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
            experiment_id: "capture-run".into(),
            include_oracle_artifacts: false,
            artifact_operation_ids: vec![],
            section: EvidenceSection::WorkspaceCapture,
            pointer: "/content/snapshot/files/0/sha256".into(),
            operation_id: None,
            capture_id: Some(evidence.content.capture_id.clone()),
            after_sequence: 0,
            limit: 20,
        }))
        .unwrap()
        .0;
    assert_eq!(section.data, evidence.content.snapshot.files[0].sha256);
    assert_eq!(section.evidence_digest, bundle.digest);
    let encoded = serde_json::to_vec(&bundle).unwrap();
    let guard = store.try_lifecycle_guard().unwrap().unwrap();
    store.purge_cell(&instance).unwrap();
    drop(guard);
    assert!(
        store
            .experiment("alpha", "designer", "capture-run")
            .is_err()
    );
    let downloaded: EvidenceBundle = serde_json::from_slice(&encoded).unwrap();
    let retained = &downloaded.content.workspace_captures[0];
    retained.validate().unwrap();
    assert_eq!(
        retained.content.snapshot.files[0].bytes().unwrap(),
        [0, 255, 128]
    );
    assert_eq!(downloaded.digest, digest_json(&downloaded.content));
}

#[test]
fn workspace_capture_admission_refuses_corruption_wrong_cells_and_late_or_changed_attachments() {
    let (store, request, evidence, _) = fixture();
    let request_digest = digest_json(&request);
    let mut corrupt = evidence.clone();
    corrupt.content.snapshot.files[0].content_base64 = "changed".into();
    assert!(
        store
            .record_workspace_capture("alpha", "designer", &request_digest, &corrupt)
            .is_err()
    );
    let mut wrong = evidence.content.clone();
    wrong.instance_key = "another-cell".into();
    assert!(
        store
            .record_workspace_capture(
                "alpha",
                "designer",
                &request_digest,
                &WorkspaceEvidence::new(wrong).unwrap()
            )
            .is_err()
    );
    store
        .record_workspace_capture("alpha", "designer", &request_digest, &evidence)
        .unwrap();
    assert!(
        store
            .record_workspace_capture("alpha", "designer", "different-request", &evidence)
            .is_err()
    );
    store
        .close_experiment("alpha", "designer", "capture-run", "finish")
        .unwrap();
    let mut late = evidence.content.clone();
    late.capture_id = "late".into();
    late.snapshot.capture_id = "late".into();
    let late_digest = digest_json(&"late-request");
    late.snapshot.capture_request_digest = late_digest.clone();
    assert!(
        store
            .record_workspace_capture(
                "alpha",
                "designer",
                &late_digest,
                &WorkspaceEvidence::new(late).unwrap()
            )
            .is_err()
    );
    assert_eq!(
        store
            .workspace_captures("alpha", "designer", "capture-run")
            .unwrap(),
        vec![evidence]
    );
    store
        .revoke("alpha", "designer", Capability::ArtifactRead)
        .unwrap();
    assert!(
        store
            .workspace_captures("alpha", "designer", "capture-run")
            .is_err()
    );
}
