use super::*;
use crate::{
    serialized_size,
    tests::{cell, seeded_store},
};
use proofstorm_core::PublishedRevision;
use proofstorm_store::Store;
use rmcp::handler::server::wrapper::Parameters;

#[test]
fn evidence_pointer_error_is_self_correcting_without_structured_error_data() {
    let error = evidence_pointer(
        serde_json::json!({"cell": {}, "components": []}),
        "/missing",
        "lock",
    )
    .expect_err("unknown pointer must fail");
    let message = error.message.to_string();
    assert!(message.contains("[evidence_pointer_not_found]"));
    assert!(message.contains("no changes were made"));
    assert!(message.contains("Recovery:"));
    assert!(message.contains("/components"));
    assert!(message.contains("/cell"));
    let error = evidence_pointer(
        serde_json::json!({"artifact": {"content": {"exit_code": 0}, "digest": "abc"}}),
        "/artifact/exit_code",
        "artifact",
    )
    .unwrap_err();
    assert!(error.message.contains("/artifact/content"));
    assert!(error.message.contains("/artifact/digest"));
}

#[test]
fn artifact_export_agent_schema_cannot_request_bulk_content() {
    let schema = schemars::schema_for!(EvidenceExportRequest);
    let encoded = serde_json::to_string(&schema).expect("artifact export schema");
    assert!(!encoded.contains("include_content"));
    assert!(encoded.contains("artifact_operation_ids"));
    assert!(!encoded.contains("\"maxItems\""));
    assert!(encoded.contains("Do not enumerate"));
}

fn closed_archive_fixture() -> (Store, PublishedRevision) {
    let store = seeded_store();
    for capability in [
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::CellOperate,
        Capability::OracleRun,
        Capability::ArtifactRead,
    ] {
        store.grant("alpha", "designer", capability).unwrap();
    }
    store
        .create_draft("alpha", "designer", "archive", &cell("archive"), "create")
        .unwrap();
    let revision = store
        .publish("alpha", "designer", "archive", 1, "publish")
        .unwrap();
    store
        .materialize(
            "alpha",
            "designer",
            "archive-instance",
            &revision.digest,
            "materialize",
        )
        .unwrap();
    store
        .create_experiment(
            "alpha",
            "designer",
            "archive-experiment",
            "archive-instance",
            "experiment",
        )
        .unwrap();
    store
        .start_session(
            "alpha",
            "designer",
            "archive-experiment",
            "archive-session",
            "session",
        )
        .unwrap();
    for index in 1..=125 {
        let id = format!("observation-{index}");
        let operation = store
            .create_operation(
                "alpha",
                "designer",
                "archive-instance",
                "archive-experiment",
                "archive-session",
                &id,
                OperationKind::ConservationOracle,
                &serde_json::json!({"expected_sat":100,"tolerance_sat":0}),
                &id,
                Capability::OracleRun,
            )
            .unwrap();
        store
            .record_operation_result(
                "alpha",
                &operation.id,
                OperationPhase::Succeeded,
                serde_json::json!({"synthetic_fixture":true,"diagnostic":"x".repeat(8192)}),
            )
            .unwrap();
    }
    store
        .finish_session("alpha", "designer", "archive-session", "finish")
        .unwrap();
    store
        .close_experiment("alpha", "designer", "archive-experiment", "close")
        .unwrap();

    (store, revision)
}

#[test]
fn large_closed_archives_export_offline_and_remain_pageable() {
    let (store, revision) = closed_archive_fixture();
    let reader = ProofstormMcp::new(store.clone(), "alpha", "reader")
        .unwrap()
        .offline();
    let read = reader
        .proofstorm_cell_read(Parameters(
            serde_json::from_value(serde_json::json!({"name":"archive-instance"})).unwrap(),
        ))
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(read["value"], serde_json::json!(revision.cell));
    assert_eq!(read["version"], 1);
    let service = ProofstormMcp::new(store, "alpha", "designer")
        .unwrap()
        .offline();
    assert!(service.tool_names().contains(&"evidence_export".into()));
    assert!(
        service
            .tool_names()
            .contains(&"evidence_section_read".into())
    );
    let ids = (1..=40)
        .map(|index| format!("observation-{index}"))
        .collect::<Vec<_>>();
    let request = EvidenceExportRequest {
        experiment_id: "archive-experiment".into(),
        include_oracle_artifacts: true,
        artifact_operation_ids: ids.clone(),
        include_content: false,
    };
    let manifest = service
        .proofstorm_evidence_export(Parameters(request.clone()))
        .unwrap()
        .0;
    let repeated = service
        .proofstorm_evidence_export(Parameters(request))
        .unwrap()
        .0;
    assert_eq!(manifest, repeated);
    assert_eq!(manifest.journal_count, 125);
    assert_eq!(manifest.artifact_count, 125);
    assert!(manifest.byte_length > 512 * 1024);
    assert!(serialized_size(&manifest).unwrap() < MAX_AGENT_RESPONSE_BYTES);
    let mut after_sequence = 100;
    let mut sequences = Vec::new();
    loop {
        let page = service
            .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
                experiment_id: "archive-experiment".into(),
                include_oracle_artifacts: true,
                artifact_operation_ids: ids.clone(),
                section: EvidenceSection::Journal,
                pointer: String::new(),
                operation_id: None,
                capture_id: None,
                after_sequence,
                limit: 50,
            }))
            .unwrap()
            .0;
        assert_eq!(page.evidence_digest, manifest.digest);
        assert!(read_query::wire_size(&page).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
        sequences.extend(
            page.data
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["sequence"].as_u64().unwrap()),
        );
        let Some(next) = page.next_after_sequence else {
            break;
        };
        assert!(next > after_sequence);
        after_sequence = next;
    }
    assert_eq!(sequences, (101..=125).collect::<Vec<_>>());
    let marker = service
        .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
            experiment_id: "archive-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: ids,
            section: EvidenceSection::Artifact,
            pointer: "/artifact/content/synthetic_fixture".into(),
            operation_id: Some("observation-125".into()),
            capture_id: None,
            after_sequence: 0,
            limit: 50,
        }))
        .unwrap()
        .0;
    assert_eq!(marker.data, true);
    assert_eq!(marker.evidence_digest, manifest.digest);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the evidence fixture proves lifecycle closure, restart independence, and sanitization together"
)]
fn evidence_export_is_deterministic_bounded_and_cluster_independent() {
    let store = seeded_store();
    for capability in [
        Capability::ExperimentCreate,
        Capability::ExperimentRead,
        Capability::ExperimentClose,
        Capability::CellOperate,
        Capability::ExperimentRead,
        Capability::OracleRun,
        Capability::ArtifactRead,
    ] {
        store
            .grant("alpha", "designer", capability)
            .expect("evidence grant");
    }
    store
        .create_draft(
            "alpha",
            "designer",
            "evidence-cell",
            &cell("evidence-cell"),
            "create-evidence-cell",
        )
        .expect("draft");
    let revision = store
        .publish(
            "alpha",
            "designer",
            "evidence-cell",
            1,
            "publish-evidence-cell",
        )
        .expect("revision");
    store
        .materialize(
            "alpha",
            "designer",
            "evidence-instance",
            &revision.digest,
            "materialize-evidence-cell",
        )
        .expect("instance");
    store
        .create_experiment(
            "alpha",
            "designer",
            "evidence-experiment",
            "evidence-instance",
            "create-evidence-experiment",
        )
        .expect("experiment");
    store
        .start_session(
            "alpha",
            "designer",
            "evidence-experiment",
            "evidence-session",
            "acquire-evidence-session",
        )
        .expect("session");
    let operation = store
        .create_operation(
            "alpha",
            "designer",
            "evidence-instance",
            "evidence-experiment",
            "evidence-session",
            "evidence-oracle",
            OperationKind::ConservationOracle,
            &serde_json::json!({"expected_sat": 100, "tolerance_sat": 0}),
            "create-evidence-oracle",
            Capability::OracleRun,
        )
        .expect("operation");
    store
        .record_operation_result(
            "alpha",
            &operation.id,
            OperationPhase::Succeeded,
            serde_json::json!({"expected_sat": 100, "actual_sat": 100, "conserved": true}),
        )
        .expect("artifact");

    let active = ProofstormMcp::new(store.clone(), "alpha", "designer").expect("session");
    let Err(error) = active.proofstorm_evidence_export(Parameters(EvidenceExportRequest {
        experiment_id: "evidence-experiment".into(),
        include_oracle_artifacts: true,
        artifact_operation_ids: vec![],
        include_content: false,
    })) else {
        panic!("active experiment evidence must refuse");
    };
    assert_eq!(
        error.data.expect("structured error")["code"],
        "evidence_experiment_active"
    );

    store
        .finish_session(
            "alpha",
            "designer",
            "evidence-session",
            "release-evidence-session",
        )
        .expect("release");
    store
        .close_experiment(
            "alpha",
            "designer",
            "evidence-experiment",
            "close-evidence-experiment",
        )
        .expect("close");
    let restarted = ProofstormMcp::new(store, "alpha", "designer").expect("restart session");
    let request = EvidenceExportRequest {
        experiment_id: "evidence-experiment".into(),
        include_oracle_artifacts: true,
        artifact_operation_ids: vec![],
        include_content: true,
    };
    let first = restarted
        .proofstorm_evidence_export(Parameters(request.clone()))
        .expect("first export")
        .0;
    let second = restarted
        .proofstorm_evidence_export(Parameters(request))
        .expect("second export")
        .0;
    assert_eq!(first, second);
    assert!(first.content_included);
    let content = serde_json::from_value::<EvidenceBundleContent>(
        first.content.clone().expect("explicit bulk content"),
    )
    .expect("typed evidence content");
    assert_eq!(first.digest, proofstorm_core::digest_json(&content));
    assert_eq!(content.journal.len(), 1);
    assert_eq!(content.artifacts.len(), 1);
    assert_eq!(content.revision.digest, revision.digest);
    assert_eq!(content.instance.lock_digest, content.revision.lock.digest);
    assert!(first.byte_length as usize <= MAX_AGENT_RESPONSE_BYTES);
    let encoded = serde_json::to_string(&first).expect("serialize evidence");
    assert!(!encoded.contains("resource_name"));
    assert!(!encoded.contains("instance_key"));
    assert!(!encoded.contains("kubernetes"));

    let journal = restarted
        .proofstorm_evidence_section_read(Parameters(
            serde_json::from_value(
                serde_json::json!({"run_id":"evidence-experiment","section":"journal","limit":50}),
            )
            .unwrap(),
        ))
        .unwrap()
        .0;
    assert_eq!(journal.data.as_array().unwrap().len(), 1);
    assert!(journal.next_after_sequence.is_none());
    assert!(serialized_size(&journal).unwrap() <= MAX_AGENT_RESPONSE_BYTES);

    let manifest = restarted
        .proofstorm_evidence_export(Parameters(EvidenceExportRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            include_content: false,
        }))
        .expect("compact evidence manifest")
        .0;
    assert_eq!(manifest.digest, first.digest);
    assert_eq!(manifest.byte_length, first.byte_length);
    assert!(!manifest.content_included);
    assert!(manifest.content.is_none());
    assert!(manifest.journal_complete);
    assert!(manifest.artifact_bodies_optional);
    assert!(manifest.guidance.contains("Do not retry"));
    assert!(
        manifest
            .resource_uri
            .starts_with("proofstorm://evidence/evidence-experiment/sha256:")
    );
    let (resource_request, resource_digest) =
        parse_evidence_resource_uri(&manifest.resource_uri).expect("resource URI");
    assert_eq!(resource_digest, manifest.digest);
    assert_eq!(resource_request.experiment_id, "evidence-experiment");
    assert!(resource_request.include_oracle_artifacts);
    assert!(resource_request.artifact_operation_ids.is_empty());
    let resource_bundle = restarted
        .build_evidence_bundle(&resource_request)
        .expect("resource bundle");
    assert_eq!(resource_bundle.digest, resource_digest);
    assert!(serialized_size(&manifest).expect("manifest size") < 1024);

    let revision_section = restarted
        .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            section: EvidenceSection::Revision,
            pointer: "/digest".into(),
            operation_id: None,
            capture_id: None,
            after_sequence: 0,
            limit: 20,
        }))
        .expect("revision section")
        .0;
    assert_eq!(revision_section.evidence_digest, manifest.digest);
    assert_eq!(revision_section.data, revision.digest);
    assert!(serialized_size(&revision_section).expect("section size") < 1024);

    let journal_section = restarted
        .proofstorm_evidence_section_read(Parameters(EvidenceSectionReadRequest {
            experiment_id: "evidence-experiment".into(),
            include_oracle_artifacts: true,
            artifact_operation_ids: vec![],
            section: EvidenceSection::Journal,
            pointer: String::new(),
            operation_id: None,
            capture_id: None,
            after_sequence: 0,
            limit: 1,
        }))
        .expect("journal section")
        .0;
    assert_eq!(journal_section.data.as_array().map(Vec::len), Some(1));
    assert!(journal_section.next_after_sequence.is_none());
}
