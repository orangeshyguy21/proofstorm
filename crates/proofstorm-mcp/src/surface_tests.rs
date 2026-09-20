//! The single public workflow, including guarantees previously split across profiles.
use super::*;
use proofstorm_core::{BitcoinNetwork, DependencyBinding};
use serde_json::{Value, json};

fn service() -> ProofstormMcp {
    let store = tests::seeded_store();
    proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
    ProofstormMcp::new(store, "alpha", "designer")
        .unwrap()
        .with_kubernetes(lifecycle_tests::cluster_client(), "system")
}
fn spec() -> Value {
    serde_json::from_str(include_str!("../../../examples/developer-cell.json")).unwrap()
}
fn request(value: Value) -> SubmissionRequest {
    serde_json::from_value(value).unwrap()
}
fn value(result: CallToolResult) -> Value {
    assert!(serialized_size(&result).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
    let wire = serde_json::to_value(&result).unwrap();
    let structured = result.structured_content.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(wire["content"][0]["text"].as_str().unwrap()).unwrap(),
        structured
    );
    structured
}

#[test]
fn offline_preview_preparation_resumes_existing_publication_receipts() {
    let store = tests::seeded_store();
    proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
    let mcp = ProofstormMcp::new(store.clone(), "alpha", "designer").unwrap();
    let input = request(json!({"name":"offline-preview", "request_id":"prepare", "cell":spec()}));
    let id = format!(
        "preview-{}",
        &digest_json(&("alpha", "designer", "prepare"))[7..39]
    );
    let mut desired = CellSpec::try_from(input.cell.clone().unwrap()).unwrap();
    desired.name = "offline-preview".into();
    store
        .create_draft("alpha", "designer", &id, &desired, &format!("{id}:draft"))
        .unwrap();
    let published = store
        .publish("alpha", "designer", &id, 1, &format!("{id}:publish"))
        .unwrap();
    let mut later = desired;
    later.components[0]
        .config
        .insert("txindex".into(), json!(false));
    store
        .edit_draft("alpha", "designer", &id, 1, &later, "later-draft-edit")
        .unwrap();

    let preview = mcp.prepare_submission(input.clone(), false).unwrap();
    assert_eq!(preview.id, id);
    assert_eq!(preview.cell, published.cell);
    assert_eq!(preview.revision_digest, published.digest);
    assert_eq!(preview.lock_digest, published.lock.digest);
    assert!(preview.update.is_none());
    assert_eq!(mcp.prepare_submission(input, false).unwrap(), preview);
    assert_eq!(
        store.cell_preview("alpha", "designer", &id).unwrap(),
        Some(preview)
    );
    assert_eq!(
        store.read_draft("alpha", "designer", &id).unwrap().version,
        2
    );
    assert!(matches!(
        store.resolve_cell("alpha", "designer", "offline-preview"),
        Err(StoreError::NotFound { .. })
    ));
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered lifecycle proves retries against later edits and sealed history"
)]
async fn preview_patch_exact_retry_and_replacement_are_fenced() {
    let mcp = service();
    let create = request(json!({"name":"preview-cell","request_id":"create","cell":spec()}));
    let plan = value(
        mcp.proofstorm_cell_plan(Parameters(create.clone()))
            .unwrap(),
    );
    assert!(
        mcp.store
            .resolve_cell("alpha", "designer", "preview-cell")
            .is_err(),
        "preview has no runtime admission"
    );
    let read = value(
        mcp.proofstorm_cell_read(Parameters(
            serde_json::from_value(json!({"plan_id":plan["plan"]["id"]})).unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(read["value"]["name"], "preview-cell");
    let mut wrong = plan["plan"].clone();
    wrong["digest"] = json!("wrong");
    assert!(
        mcp.proofstorm_cell_up(Parameters(request(
            json!({"name":"preview-cell","request_id":"apply","plan":wrong})
        )))
        .await
        .is_err()
    );
    let created = value(
        mcp.proofstorm_cell_up(Parameters(request(
            json!({"name":"preview-cell","request_id":"apply","plan":plan["plan"]}),
        )))
        .await
        .unwrap(),
    );
    let other_plan = value(
        mcp.proofstorm_cell_plan(Parameters(request(
            json!({"name":"other-cell","request_id":"other-plan","cell":spec()}),
        )))
        .unwrap(),
    );
    let conflict = mcp
        .proofstorm_cell_up(Parameters(request(
            json!({"name":"other-cell","request_id":"apply","plan":other_plan["plan"]}),
        )))
        .await
        .unwrap_err();
    assert_eq!(conflict.data.unwrap()["code"], "idempotency_conflict");
    assert!(
        mcp.store
            .resolve_cell("alpha", "designer", "other-cell")
            .is_err(),
        "conflicting apply ID must be rejected before runtime admission"
    );
    let component = read["value"]["components"][0].clone();
    let id = component["id"].as_str().unwrap().to_owned();
    let mut edited = component.clone();
    edited["config"]["txindex"] = json!(false);
    let patch = request(
        json!({"name":"preview-cell","request_id":"patch","expected_generation":1,"expected_instance_key":created["instance_key"],"patch":[{"op":"update_component","component":edited}]}),
    );
    let edit_plan = value(mcp.proofstorm_cell_plan(Parameters(patch.clone())).unwrap());
    assert_eq!(edit_plan["changes"]["restarted"], 1);
    let updated = value(
        mcp.proofstorm_cell_up(Parameters(patch.clone()))
            .await
            .unwrap(),
    );
    assert_eq!(updated["accepted_generation"], 2);
    let mut next = component;
    next["config"]["txindex"] = json!(true);
    let second = request(
        json!({"name":"preview-cell","request_id":"patch-two","expected_generation":2,"expected_instance_key":created["instance_key"],"patch":[{"op":"update_component","component":next}]}),
    );
    mcp.proofstorm_cell_up(Parameters(second)).await.unwrap();
    let replay = value(
        mcp.proofstorm_cell_up(Parameters(patch.clone()))
            .await
            .unwrap(),
    );
    assert_eq!(replay["accepted_generation"], 2);
    assert_eq!(replay["desired_generation"], 3);
    let original = value(
        mcp.proofstorm_cell_up(Parameters(create.clone()))
            .await
            .unwrap(),
    );
    assert_eq!(
        original["desired_generation"], 3,
        "creation retry cannot restore generation one"
    );
    let found = value(
        mcp.proofstorm_cell_search(Parameters(
            serde_json::from_value(
                json!({"name":"preview-cell","id":id,"fields":["/config/txindex"]}),
            )
            .unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(found["items"][0]["value"]["/config/txindex"], true);
    let stale = value(
        mcp.proofstorm_cell_read(Parameters(
            serde_json::from_value(
                json!({"plan_id":edit_plan["plan"]["id"],"pointer":"/components/0/config/txindex"}),
            )
            .unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(
        stale["value"], false,
        "preview is immutable after later edits"
    );
    let mut changed = patch;
    changed.request_id = "new-stale-request".into();
    assert_eq!(
        mcp.proofstorm_cell_up(Parameters(changed))
            .await
            .unwrap_err()
            .data
            .unwrap()["code"],
        "cell_update_conflict"
    );
    mcp.cells().unwrap().down("preview-cell", 2).await.unwrap();
    assert!(
        mcp.proofstorm_cell_up(Parameters(create)).await.is_err(),
        "a removed creation cannot resurrect its name"
    );
}

#[tokio::test]
async fn ordered_patch_keeps_flat_request_identity_and_immutable_preview() {
    let mcp = service();
    let created = value(
        mcp.proofstorm_cell_up(Parameters(request(json!({
            "name":"patch-cell", "request_id":"create", "cell":spec()
        }))))
        .await
        .unwrap(),
    );
    let mut chain = spec()["components"][0].clone();
    chain["config"]["txindex"] = json!(false);
    let patch = json!([
        {"op":"remove_component", "id":"chain"},
        {"op":"remove_link", "id":"mint-chain"},
        {"op":"add_link", "link":{"id":"mint-chain", "kind":"chain_backend", "from":"mint", "to":"chain", "network":"regtest"}},
        {"op":"add_component", "component":spec()["components"][0]},
        {"op":"update_component", "component":chain},
        {"op":"set_policy", "policy":{"allow":[], "limits":{"max_components":2}}}
    ]);
    let input = request(
        json!({"name":"patch-cell", "request_id":"patch", "patch":patch,
        "expected_generation":1, "expected_instance_key":created["instance_key"]}),
    );
    let preview = mcp.prepare_submission(input.clone(), false).unwrap();
    // Independent fingerprint of the existing flat, ordered wire representation.
    assert_eq!(
        digest_json(&input.patch),
        "sha256:65054f2227357690934e35096bdeae421ffdbacdcefdea92c676a5c9ac56fe39"
    );
    assert_eq!(
        preview.request_digest,
        digest_json(&(
            "patch-cell",
            Option::<CellSpec>::None,
            &input.patch,
            1,
            &created["instance_key"],
            false,
            Vec::<String>::new()
        )),
        "saved identity uses the original flat link input"
    );
    assert_eq!(preview.cell.components[0].config["txindex"], false);
    assert_eq!(preview.cell.policy.limits.max_components, Some(2));
    assert_eq!(
        preview.cell.links[0].binding,
        Some(DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest
        })
    );
    let updated = value(
        mcp.proofstorm_cell_up(Parameters(input.clone()))
            .await
            .unwrap(),
    );
    assert_eq!(updated["accepted_generation"], 2);
    let replay = mcp.prepare_submission(input, false).unwrap();
    assert_eq!(digest_json(&preview), digest_json(&replay));
}

#[tokio::test]
async fn failed_patch_batches_leave_no_draft_or_preview_and_preserve_the_desired_revision() {
    let mcp = service();
    let created = value(
        mcp.proofstorm_cell_up(Parameters(request(json!({
            "name":"patch-cell", "request_id":"create", "cell":spec()
        }))))
        .await
        .unwrap(),
    );
    let instance_id = created["cell"]["instance_id"].as_str().unwrap();
    let before = mcp
        .store
        .instance("alpha", "designer", instance_id)
        .unwrap();
    let mut unknown_implementation = spec()["components"][0].clone();
    unknown_implementation["implementation"] = json!("missing");
    for (index, (patch, code)) in [
        (json!([]), "invalid_cell_input"),
        (json!(vec![json!({"op":"remove_link", "id":"mint-chain"}); 101]), "invalid_cell_input"),
        (json!([{"op":"add_component", "component":spec()["components"][0]}]), "invalid_cell_input"),
        (json!([{"op":"remove_component", "id":"chain"}, {"op":"remove_link", "id":"missing"}]), "invalid_cell_input"),
        (json!([{"op":"remove_component", "id":"chain"}]), "cell_plan_invalid"),
        (json!([{"op":"update_component", "component":unknown_implementation}]), "cell_plan_invalid"),
        (json!([{"op":"set_policy", "policy":{"limits":{"max_components":1}}}]), "cell_plan_invalid"),
        (json!([{"op":"remove_link", "id":"mint-chain"}, {"op":"add_link", "link":{"id":"mint-chain", "kind":"bitcoin_peer", "from":"mint", "to":"chain"}}]), "cell_plan_invalid"),
    ].into_iter().enumerate() {
        let request_id = format!("invalid-{index}");
        let error = mcp.prepare_submission(request(json!({
            "name":"patch-cell", "request_id":request_id, "patch":patch,
            "expected_generation":1, "expected_instance_key":created["instance_key"]
        })), false).unwrap_err();
        assert_eq!(error.data.unwrap()["code"], code);
        let id = format!("preview-{}", &digest_json(&json!(["alpha", "designer", request_id]))[7..39]);
        assert!(mcp.store.cell_preview("alpha", "designer", &id).unwrap().is_none());
        assert!(matches!(mcp.store.read_draft("alpha", "designer", &id), Err(StoreError::NotFound { .. })));
        let current = mcp.store.instance("alpha", "designer", instance_id).unwrap();
        assert_eq!(current.generation, before.generation);
        assert_eq!(current.revision_digest, before.revision_digest);
    }
}

#[test]
fn every_tool_requires_its_entire_registry_grant_set() {
    let store = tests::seeded_store();
    for tool in proofstorm_core::mcp::TOOLS {
        for missing in tool.capabilities {
            proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
            store.revoke("alpha", "designer", *missing).unwrap();
            let service = ProofstormMcp::new(store.clone(), "alpha", "designer").unwrap();
            assert!(
                !service.tool_names().contains(&tool.name.to_owned()),
                "{} is exposed without {missing:?}",
                tool.name
            );
        }
    }
}

#[tokio::test]
async fn canonical_preview_rechecks_permissions_and_rejects_invalid_or_conflicting_input() {
    let mcp = service();
    let input = request(json!({"name":"preview-cell","request_id":"preview","cell":spec()}));
    mcp.proofstorm_cell_plan(Parameters(input.clone())).unwrap();
    let mut changed = input.clone();
    changed.name = "different-cell".into();
    assert_eq!(
        mcp.proofstorm_cell_plan(Parameters(changed))
            .unwrap_err()
            .data
            .unwrap()["code"],
        "idempotency_conflict"
    );
    mcp.store
        .revoke("alpha", "designer", Capability::CellCreate)
        .unwrap();
    assert_eq!(
        mcp.proofstorm_cell_plan(Parameters(input))
            .unwrap_err()
            .data
            .unwrap()["code"],
        "access_denied"
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered lifecycle proves retries against later edits and sealed history"
)]
async fn finished_automatic_runs_roll_forward_and_keep_exact_retries_and_exports() {
    let mcp = service();
    let created = value(
        mcp.proofstorm_cell_up(Parameters(request(
            json!({"name":"run-cell","request_id":"create","cell":spec()}),
        )))
        .await
        .unwrap(),
    );
    let instance = created["cell"]["instance_id"].as_str().unwrap();
    let command = json!({"component":"chain","argv":["true"]});
    let first = mcp
        .store
        .create_operation(
            "alpha",
            "designer",
            instance,
            "",
            "",
            "first",
            OperationKind::ComponentExecLive,
            &command,
            "first",
            Capability::ComponentExecLive,
        )
        .unwrap();
    mcp.store
        .record_operation_result(
            "alpha",
            "first",
            OperationPhase::Succeeded,
            json!({"exit_code":0,"cleanup_verified":true}),
        )
        .unwrap();
    mcp.proofstorm_run_finish(Parameters(RunFinishRequest {
        experiment_id: first.experiment_id.clone(),
        idempotency_key: "finish".into(),
    }))
    .await
    .unwrap();
    let export = EvidenceExportRequest {
        experiment_id: first.experiment_id.clone(),
        include_oracle_artifacts: false,
        artifact_operation_ids: vec![first.id.clone()],
        include_content: false,
    };
    let before = mcp.build_evidence_bundle(&export).unwrap();
    let repeated = mcp
        .store
        .create_operation(
            "alpha",
            "designer",
            instance,
            "",
            "",
            "first",
            OperationKind::ComponentExecLive,
            &command,
            "first",
            Capability::ComponentExecLive,
        )
        .unwrap();
    assert_eq!(repeated.experiment_id, first.experiment_id);
    let second = mcp
        .store
        .create_operation(
            "alpha",
            "designer",
            instance,
            "",
            "",
            "second",
            OperationKind::ComponentExecLive,
            &command,
            "second",
            Capability::ComponentExecLive,
        )
        .unwrap();
    assert_ne!(second.experiment_id, first.experiment_id);
    mcp.store
        .create_experiment("alpha", "designer", "empty-run", instance, "empty-run")
        .unwrap();
    let directory = value(
        mcp.proofstorm_environment_read(Parameters(
            serde_json::from_value(
                json!({"instance_id":instance,"runs":{"id":"empty-run","scan":true}}),
            )
            .unwrap(),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(directory["runs"].as_array().unwrap().len(), 1);
    assert_eq!(directory["runs"][0]["id"], "empty-run");
    mcp.store
        .record_operation_result(
            "alpha",
            "second",
            OperationPhase::Succeeded,
            json!({"exit_code":0,"cleanup_verified":true}),
        )
        .unwrap();
    let mut component = spec()["components"][0].clone();
    component["config"]["txindex"] = json!(false);
    mcp.proofstorm_cell_up(Parameters(request(json!({"name":"run-cell","request_id":"later-edit","expected_generation":1,"expected_instance_key":created["instance_key"],"patch":[{"op":"update_component","component":component}]})))).await.unwrap();
    assert_eq!(
        mcp.build_evidence_bundle(&export).unwrap().digest,
        before.digest,
        "sealed evidence survives later cell configuration changes"
    );
}

#[test]
fn large_configuration_reads_scan_then_slice_exact_unicode_and_bind_digests() {
    let mut cell: CellSpec = serde_json::from_value(spec()).unwrap();
    let text = "λ🦀\n\"".repeat(6000);
    cell.components[0]
        .config
        .insert("diagnostic".into(), json!(text));
    let document = CellReadResponse {
        id: "large".into(),
        workspace_id: "alpha".into(),
        version: 4,
        cell,
    };
    let root: CellReadRequest = serde_json::from_value(json!({"name":"large"})).unwrap();
    let scan = value(cell_read::read(&document, &root).unwrap());
    assert_eq!(scan["scan"], true);
    assert!(
        scan["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "/policy")
    );
    let mut read:CellReadRequest=serde_json::from_value(json!({"name":"large","pointer":"/components/0/config/diagnostic","expected_digest":scan["cell_digest"],"offset":1,"limit":37})).unwrap();
    let slice = value(cell_read::read(&document, &read).unwrap());
    assert_eq!(
        slice["value"],
        text.chars().skip(1).take(37).collect::<String>()
    );
    assert_eq!(slice["next_offset"], 38);
    read.expected_digest = Some("wrong".into());
    assert!(cell_read::read(&document, &read).is_err());
}

#[test]
fn public_catalog_controls_reference_only_real_tools() {
    for entry in &default_catalog().entries {
        let detail = CatalogEntryDetail::from_entry(entry, false);
        for endpoint in detail.runtime_endpoints {
            for control in endpoint.controls {
                assert!(proofstorm_core::mcp::tool(&control).is_some(), "{control}");
            }
            assert!(
                endpoint
                    .limitations
                    .iter()
                    .all(|text| !text.contains("component_exec_live"))
            );
        }
    }
}

#[tokio::test]
async fn exact_configuration_and_lock_reads_need_only_read_authority_and_recheck_revocation() {
    let mcp = service();
    mcp.proofstorm_cell_up(Parameters(request(
        json!({"name":"read-only-cell","request_id":"create","cell":spec()}),
    )))
    .await
    .unwrap();
    mcp.store
        .revoke("alpha", "designer", Capability::CellStatus)
        .unwrap();
    let read = value(
        mcp.proofstorm_cell_read(Parameters(
            serde_json::from_value(json!({"name":"read-only-cell","pointer":"/name"})).unwrap(),
        ))
        .unwrap(),
    );
    assert_eq!(read["value"], "read-only-cell");
    value(
        mcp.proofstorm_cell_read(Parameters(
            serde_json::from_value(json!({"name":"read-only-cell","document":"lock","scan":true}))
                .unwrap(),
        ))
        .unwrap(),
    );
    mcp.store
        .revoke("alpha", "designer", Capability::CellRead)
        .unwrap();
    let refusal = mcp
        .proofstorm_cell_read(Parameters(
            serde_json::from_value(json!({"name":"read-only-cell"})).unwrap(),
        ))
        .unwrap_err();
    assert_eq!(refusal.data.unwrap()["code"], "access_denied");
}
