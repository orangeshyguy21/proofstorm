use super::*;
use crate::{OperationReadRequest, Parameters, ProofstormMcp};
use proofstorm_core::{Capability, CellSpec};
use serde_json::json;
use std::collections::BTreeSet;

fn fixture() -> (Store, ProofstormMcp, String) {
    let store = Store::memory().unwrap();
    for actor in ["alice", "bob"] {
        proofstorm_app::developer::configure(&store, "test", actor).unwrap();
    }
    let instance = cell(&store, "payments");
    let service = ProofstormMcp::new(store.clone(), "test", "alice")
        .unwrap()
        .offline();
    (store, service, instance)
}

fn cell(store: &Store, name: &str) -> String {
    let spec: CellSpec = serde_json::from_value(json!({
        "api_version":"proofstorm/v1alpha1", "name":name, "links":[],
        "components":[{"id":"chain","kind":"bitcoin","implementation":"bitcoin-core",
            "version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}]
    }))
    .unwrap();
    let handle = store
        .reserve_cell("test", "alice", name, &digest_json(&spec))
        .unwrap();
    store
        .create_draft("test", "alice", name, &spec, &format!("draft-{name}"))
        .unwrap();
    let revision = store
        .publish("test", "alice", name, 1, &format!("publish-{name}"))
        .unwrap();
    store
        .materialize(
            "test",
            "alice",
            &handle.instance_id,
            &revision.digest,
            &format!("up-{name}"),
        )
        .unwrap();
    handle.instance_id
}

fn record(
    store: &Store,
    instance: &str,
    actor: &str,
    id: &str,
    phase: OperationPhase,
    output: Value,
) -> CellOperation {
    let op = store
        .create_operation(
            "test",
            actor,
            instance,
            "",
            "",
            id,
            OperationKind::ComponentExecLive,
            &json!({"component":actor, "argv":["cashu", "balance"], "output":{"mode":"public"}}),
            id,
            Capability::ComponentExecLive,
        )
        .unwrap();
    if phase == OperationPhase::Pending {
        return op;
    }
    if phase == OperationPhase::Running {
        return store.update_operation_phase("test", id, phase).unwrap();
    }
    store
        .record_operation_result("test", id, phase, output)
        .unwrap()
}

fn query(value: Value) -> ActivitySearchRequest {
    serde_json::from_value(value).unwrap()
}

fn visible(result: CallToolResult) -> Value {
    let wire = serde_json::to_value(&result).unwrap();
    assert!(serialized_size(&result).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
    let text: Value = serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result.structured_content.unwrap());
    text
}

fn search(service: &ProofstormMcp, request: ActivitySearchRequest) -> Value {
    visible(
        service
            .proofstorm_activity_search(Parameters(request))
            .unwrap(),
    )
}

fn read(service: &ProofstormMcp, request: Value) -> Result<CallToolResult, ErrorData> {
    service.proofstorm_operation_read(Parameters(
        serde_json::from_value::<OperationReadRequest>(request).unwrap(),
    ))
}

#[test]
fn pending_artifact_reads_offer_a_wait_without_polling_or_bypassing_digests() {
    let (store, service, instance) = fixture();
    for phase in [OperationPhase::Pending, OperationPhase::Running] {
        let id = format!("pending-output-{phase:?}").to_ascii_lowercase();
        let operation = record(&store, &instance, "alice", &id, phase, Value::Null);
        let digest = crate::compact_operation_wait(operation.clone(), false).operation_digest;
        for pointer in ["/artifact", "/artifact/content", "/artifact/content/stdout"] {
            let error = read(
                &service,
                json!({"operation_id":id,"pointer":pointer,
                "expected_digest":digest}),
            )
            .unwrap_err();
            let wire = visible(crate::tool_error::result(error));
            let data = &wire["data"];
            assert_eq!(data["code"], "operation_read_output_pending");
            assert_eq!(data["recorded_phase"], json!(phase));
            assert_eq!(data["operation_digest"], digest);
            assert_eq!(data["next_tool"], "operation_wait");
            let wait: crate::OperationWaitRequest =
                serde_json::from_value(data["next_arguments"].clone()).unwrap();
            crate::validate_operation_wait_request(&wait).unwrap();
            assert_eq!(wait.operation_ids, [id.clone()]);
        }
        assert_eq!(store.operation("test", "alice", &id).unwrap(), operation);
        assert_eq!(
            visible(read(&service, json!({"operation_id":id,"pointer":"/phase"})).unwrap())["value"],
            json!(phase)
        );
        let stale = read(
            &service,
            json!({"operation_id":id,
            "pointer":"/artifact/content/stdout","expected_digest":"stale"}),
        )
        .unwrap_err();
        assert_eq!(stale.data.unwrap()["code"], "operation_read_changed");

        let completed = store
            .record_operation_result(
                "test",
                &id,
                OperationPhase::Succeeded,
                json!({"stdout":"ready","exit_code":0,"output_mode":"public"}),
            )
            .unwrap();
        let old = read(
            &service,
            json!({"operation_id":id,
            "pointer":"/artifact/content/stdout","expected_digest":digest}),
        )
        .unwrap_err();
        assert_eq!(old.data.unwrap()["code"], "operation_read_changed");
        let recovered = visible(
            read(
                &service,
                json!({"operation_id":id,
            "pointer":"/artifact/content/stdout","expected_digest":crate::compact_operation_wait(completed, false).operation_digest}),
            )
            .unwrap(),
        );
        assert_eq!(recovered["value"], "ready");
    }
}

#[test]
fn pending_output_guidance_preserves_missing_paths_private_data_and_authority() {
    let (store, service, instance) = fixture();
    record(
        &store,
        &instance,
        "alice",
        "active-output",
        OperationPhase::Running,
        Value::Null,
    );
    for pointer in ["/missing", "/request/typo", "/artifactish", "/artifac~1t"] {
        let error = read(
            &service,
            json!({"operation_id":"active-output","pointer":pointer}),
        )
        .unwrap_err();
        assert_eq!(
            error.data.unwrap()["code"],
            "operation_read_pointer_missing"
        );
    }
    let invalid = read(
        &service,
        json!({"operation_id":"active-output","pointer":"/artifact/~2"}),
    )
    .unwrap_err();
    assert_eq!(invalid.data.unwrap()["code"], "invalid_json_pointer");
    for phase in [
        OperationPhase::Succeeded,
        OperationPhase::Failed,
        OperationPhase::Cancelled,
    ] {
        let id = format!("terminal-empty-output-{phase:?}").to_ascii_lowercase();
        record(&store, &instance, "alice", &id, phase, Value::Null);
        let error = read(
            &service,
            json!({"operation_id":id,"pointer":"/artifact/content/stdout"}),
        )
        .unwrap_err();
        assert_eq!(
            error.data.unwrap()["code"],
            "operation_read_pointer_missing"
        );
    }
    record(
        &store,
        &instance,
        "alice",
        "private-output",
        OperationPhase::Succeeded,
        json!({"output_mode":"private","stdout":null}),
    );
    assert!(
        visible(
            read(
                &service,
                json!({"operation_id":"private-output","pointer":"/artifact/content/stdout"})
            )
            .unwrap()
        )["value"]
            .is_null()
    );
    let absent = read(
        &service,
        json!({"operation_id":"private-output","pointer":"/artifact/content/stderr"}),
    )
    .unwrap_err();
    assert_eq!(
        absent.data.unwrap()["code"],
        "operation_read_pointer_missing"
    );
    proofstorm_app::developer::configure(&store, "foreign", "alice").unwrap();
    let foreign = ProofstormMcp::new(store.clone(), "foreign", "alice")
        .unwrap()
        .offline();
    store
        .revoke("test", "alice", Capability::ArtifactRead)
        .unwrap();
    for reader in [&service, &foreign] {
        let denied = read(
            reader,
            json!({"operation_id":"active-output","pointer":"/artifact/content/stdout"}),
        )
        .unwrap_err();
        let wire = visible(crate::tool_error::result(denied)).to_string();
        assert!(!wire.contains("operation_read_output_pending"));
        assert!(!wire.contains("recorded_phase") && !wire.contains("next_arguments"));
    }
}

#[test]
fn oversized_wait_batches_offer_smaller_batches_and_a_single_receipt_fallback() {
    let (store, _, instance) = fixture();
    let operations: Vec<_> = (0..40).map(|index| record(&store, &instance, "alice",
        &format!("batch-{index}"), OperationPhase::Succeeded,
        json!({"stdout":"x".repeat(8000),"exit_code":7,"cleanup_verified":true,"output_mode":"public"}))).collect();
    let error =
        crate::compact_operation_wait_many(operations.clone(), vec![], false, 30).unwrap_err();
    let wire = visible(crate::tool_error::result(error));
    let data = &wire["data"];
    assert_eq!(data["code"], "operation_wait_response_too_large");
    assert_eq!(data["operation_count"], 40);
    assert_eq!(data["maximum_response_bytes"], MAX_AGENT_RESPONSE_BYTES);
    assert_eq!(data["next_tool"], "operation_wait");
    let size = usize::try_from(data["suggested_batch_size"].as_u64().unwrap()).unwrap();
    assert!(size > 0 && size < operations.len());
    let mut recovered = Vec::new();
    for chunk in operations.chunks(size) {
        let result = crate::compact_operation_wait_many(chunk.to_vec(), vec![], false, 30).unwrap();
        assert!(result.all_terminal);
        assert!(crate::read_query::wire_size(&result).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
        for operation in result.operations {
            assert_eq!(operation.native_result.as_ref().unwrap()["exit_code"], 7);
            assert_eq!(
                operation.native_result.as_ref().unwrap()["cleanup_verified"],
                true
            );
            recovered.push(operation.operation_id);
        }
    }
    assert_eq!(
        recovered,
        operations
            .iter()
            .map(|op| op.id.clone())
            .collect::<Vec<_>>()
    );
    for count in [1, 2] {
        let errors = (0..count)
            .map(|index| crate::OperationWaitError {
                operation_id: format!("bad-{index}"),
                error: json!({"message":"canary".repeat(5000)}),
            })
            .collect();
        let error = crate::compact_operation_wait_many(vec![], errors, false, 30).unwrap_err();
        let wire = visible(crate::tool_error::result(error));
        assert_eq!(wire["data"]["operation_count"], count);
        assert_eq!(wire["data"]["suggested_batch_size"], 1);
        assert_eq!(
            wire["data"]["next_tool"],
            if count == 1 {
                "operation_read"
            } else {
                "operation_wait"
            }
        );
        assert!(!wire.to_string().contains("canary"));
    }
}

#[test]
fn invalid_field_pointer_identifies_the_search_array_item() {
    let mut request = query(json!({"name":"payments", "fields":["/id", "/broken~"]}));
    let error = pattern(&request).unwrap_err();
    let data = error.data.unwrap();
    assert_eq!(data["code"], "invalid_json_pointer");
    assert_eq!(data["issues"][0]["path"], "/fields/1");
    request.fields[1] = data["issues"][0]["example"].as_str().unwrap().into();
    assert!(pattern(&request).is_ok());
}

#[test]
fn input_issues_identify_each_limit_and_pointer_recovery_preserves_authority() {
    let (store, service, instance) = fixture();
    let error = service
        .proofstorm_activity_search(Parameters(query(json!({
            "name":"payments", "query":"x".repeat(4097),
            "fields":vec!["/id";17], "limit":51
        }))))
        .unwrap_err();
    let details = error.data.unwrap();
    assert_eq!(details["code"], "activity_search_limits");
    let issues = details["issues"].as_array().unwrap();
    assert_eq!(
        issues
            .iter()
            .map(|issue| issue["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["/query", "/fields", "/limit"]
    );
    assert_eq!(issues[2]["expected"], json!({"minimum":1,"maximum":50}));

    let op = record(
        &store,
        &instance,
        "alice",
        "pointer-feedback",
        OperationPhase::Succeeded,
        json!({"a/b~c":{"leaf":null},"stdout":"public-canary","private_output":{"stdout":{"retained_bytes":479}}}),
    );
    let error = read(&service, json!({"operation_id":op.id,"limit":4001})).unwrap_err();
    assert_eq!(
        error.data.unwrap()["issues"][0]["expected"],
        json!({"minimum":1,"maximum":4000})
    );
    let error = read(
        &service,
        json!({"operation_id":op.id,"pointer":"/artifact/content/missing/child"}),
    )
    .unwrap_err();
    let details = error.data.unwrap();
    let issue = &details["issues"][0];
    assert_eq!(issue["expected"]["existing_parent"], "/artifact/content");
    assert!(
        issue["expected"]["available_pointers"]
            .as_array()
            .unwrap()
            .contains(&json!("/artifact/content/a~1b~0c"))
    );
    assert!(!details.to_string().contains("public-canary"));
    let recovered = visible(read(&service, json!({"operation_id":op.id,"pointer":issue["example"],"expected_digest":details["operation_digest"]})).unwrap());
    assert_eq!(recovered["value"], json!({"leaf":null}));
    let nested = read(
        &service,
        json!({"operation_id":op.id,"pointer":"/artifact/content/a~1b~0c/absent"}),
    )
    .unwrap_err()
    .data
    .unwrap();
    assert_eq!(
        nested["issues"][0]["example"],
        "/artifact/content/a~1b~0c/leaf"
    );
    // A missing path must not disclose even field names after permission revocation.
    store
        .revoke("test", "alice", Capability::ArtifactRead)
        .unwrap();
    let denied = read(&service, json!({"operation_id":op.id,"pointer":"/missing"})).unwrap_err();
    assert_ne!(
        denied.data.as_ref().unwrap()["code"],
        "operation_read_pointer_missing"
    );
    assert!(!json!(denied).to_string().contains("available_pointers"));
}

#[test]
fn output_recovery_reads_work_without_revealing_private_streams() {
    let (store, service, instance) = fixture();
    let pending = record(
        &store,
        &instance,
        "alice",
        "pending-wait",
        OperationPhase::Pending,
        Value::Null,
    );
    let batch = crate::compact_operation_wait_many(vec![pending], vec![], true, 2).unwrap();
    assert!(!batch.all_terminal);
    assert!(batch.timed_out && batch.operations[0].timed_out);
    assert_eq!(batch.requested_timeout_seconds, 2);
    assert_eq!(batch.effective_timeout_seconds, 2);
    assert_eq!(batch.next_tool.as_deref(), Some("operation_wait"));
    for (id, phase, content) in [
        (
            "private-timeout",
            OperationPhase::Succeeded,
            json!({"exit_code":null,"timed_out":true,"output_mode":"private","stdout":"","stderr":"","private_output":{"stdout":{"bytes_observed":479,"retained_bytes":479,"sha256":"private-canary"}}}),
        ),
        (
            "public-cancel",
            OperationPhase::Cancelled,
            json!({"exit_code":null,"cancelled":true,"output_mode":"public","stdout":"invoice already created","stderr":"","private_output":{"stdout":{"retained_bytes":23}}}),
        ),
        (
            "projection",
            OperationPhase::Succeeded,
            json!({"exit_code":0,"output_mode":"json_fields","stdout":"","selected_output":{"status":"PAID"}}),
        ),
    ] {
        let operation = record(&store, &instance, "alice", id, phase, content);
        let result = crate::compact_operation_wait(operation, false);
        assert!(result.terminal);
        assert!(!result.timed_out); // Wait timeout is distinct from the native command timeout.
        let output = &result.native_result.as_ref().unwrap()["output"];
        assert_eq!(output["private_streams_readable"], false);
        assert!(!output.to_string().contains("private-canary"));
        let reads = output["reads"].as_array().unwrap();
        if id == "private-timeout" {
            assert!(reads.is_empty());
            assert_eq!(output["streams"]["stdout"]["retained_bytes"], 479);
            assert_eq!(result.native_result.as_ref().unwrap()["timed_out"], true);
        } else {
            assert_eq!(reads.len(), 1);
            let recovered = visible(read(&service, reads[0]["arguments"].clone()).unwrap());
            assert_eq!(recovered["operation_digest"], result.operation_digest);
            assert!(!recovered["value"].is_null());
        }
    }
}

#[tokio::test]
async fn an_unresponsive_backend_returns_wait_limits_and_retry_tools() {
    let (store, _, instance) = fixture();
    record(
        &store,
        &instance,
        "alice",
        "waiting",
        OperationPhase::Pending,
        Value::Null,
    );
    let client = kube::Client::new(
        tower::service_fn(|_: http::Request<kube::client::Body>| {
            std::future::pending::<Result<http::Response<kube::client::Body>, std::io::Error>>()
        }),
        "system",
    );
    let service = ProofstormMcp::new(store, "test", "alice")
        .unwrap()
        .with_kubernetes(client, "system");
    let (operation, cell) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            service.proofstorm_operation_wait(Parameters(crate::OperationWaitRequest {
                operation_ids: vec!["waiting".into()],
                timeout_seconds: 1
            })),
            service.proofstorm_cell_wait(Parameters(crate::CellWaitRequest {
                instance_id: "payments".into(),
                expected_instance_key: None,
                expected_generation: None,
                target_phase: crate::InstancePhase::Ready,
                timeout_seconds: 1
            }))
        )
    })
    .await
    .expect("server waits must bound an unresponsive backend");
    for (error, tool) in [
        (
            operation.map(|result| result.0).unwrap_err(),
            "operation_wait",
        ),
        (cell.map(|result| result.0).unwrap_err(), "cell_wait"),
    ] {
        let data = error.data.unwrap();
        assert_eq!(data["code"], format!("{tool}_deadline_exceeded"));
        assert_eq!(data["requested_timeout_seconds"], 1);
        assert_eq!(data["effective_timeout_seconds"], 1);
        assert_eq!(data["next_tool"], tool);
    }
}

#[test]
fn finds_other_actors_with_filters_paths_and_selected_receipt_fields_without_mutations() {
    let (store, service, instance) = fixture();
    record(
        &store,
        &instance,
        "alice",
        "alice-result",
        OperationPhase::Failed,
        json!({"stdout":"database locked"}),
    );
    let bob = record(
        &store,
        &instance,
        "bob",
        "bob-result",
        OperationPhase::Failed,
        json!({"stdout":"database busy", "exit_code":1, "cleanup_verified":true}),
    );
    record(
        &store,
        &instance,
        "bob",
        "bob-success",
        OperationPhase::Succeeded,
        json!({"stdout":"database busy"}),
    );
    let before = store.observation_token("test", "alice").unwrap();
    let request = query(
        json!({"name":"payments", "query":"DATABASE (busy|locked)", "regex":true,
        "case_insensitive":true, "component":"bob", "phase":"failed", "kind":"component_exec_live",
        "principal_id":"bob", "run_id":bob.experiment_id, "session_id":bob.session_id,
        "accepted_after_unix":bob.accepted_at_unix, "accepted_before_unix":bob.accepted_at_unix+1,
        "fields":["/artifact/content/exit_code", "/missing"]}),
    );
    let found = search(&service, request.clone());
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let hit = &found["items"][0];
    assert_eq!(hit["operation_id"], "bob-result");
    assert_eq!(hit["matches"][0]["pointer"], "/artifact/content/stdout");
    assert_eq!(hit["fields"][0]["value"], 1);
    assert_eq!(hit["fields"][1]["exists"], false);
    let receipt = visible(
        read(
            &service,
            json!({"operation_id":"bob-result",
        "pointer":hit["matches"][0]["pointer"], "expected_digest":hit["operation_digest"]}),
        )
        .unwrap(),
    );
    assert_eq!(receipt["value"], "database busy");
    let mut excluded = request;
    excluded.accepted_after_unix = None;
    excluded.accepted_before_unix = Some(bob.accepted_at_unix);
    assert!(
        search(&service, excluded)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(before, store.observation_token("test", "alice").unwrap());
}

#[test]
fn sparse_search_advances_after_an_empty_bounded_scan() {
    let (store, service, instance) = fixture();
    for n in 0..215 {
        record(
            &store,
            &instance,
            "alice",
            &format!("record-{n:03}"),
            OperationPhase::Succeeded,
            json!({"stdout":if n == 0 {"needle"} else {"ordinary output"}}),
        );
    }
    let mut request = query(json!({"name":"payments", "query":"needle"}));
    let first = search(&service, request.clone());
    assert_eq!(first["scanned_count"], SCAN_LIMIT);
    assert!(first["items"].as_array().unwrap().is_empty());
    request.cursor = Some(first["next_cursor"].as_str().unwrap().into());
    let second = search(&service, request);
    assert_eq!(second["items"][0]["operation_id"], "record-000");
    assert!(second["next_cursor"].is_null());
}

#[test]
fn response_budget_pages_matches_losslessly_and_exposes_omitted_large_fields() {
    let (store, service, instance) = fixture();
    let mut expected = BTreeSet::new();
    for n in 0..40 {
        let id = format!("result-{n:03}");
        record(
            &store,
            &instance,
            "bob",
            &id,
            OperationPhase::Failed,
            json!({"stdout":format!("database {}", "\"\\\n界".repeat(700)), "exit_code":1}),
        );
        expected.insert(id);
    }
    let mut request = query(json!({"name":"payments", "query":"database", "limit":50,
        "fields":["/artifact/content/stdout", "/artifact/content/exit_code"]}));
    let mut seen = BTreeSet::new();
    loop {
        let page = search(&service, request.clone());
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty() && items.len() < 40);
        for item in items {
            assert_eq!(item["fields"][0]["value_omitted"], true);
            assert_eq!(item["fields"][1]["value"], 1);
            assert!(
                seen.insert(item["operation_id"].as_str().unwrap().to_owned()),
                "duplicate match"
            );
        }
        let Some(cursor) = page["next_cursor"].as_str() else {
            break;
        };
        request.cursor = Some(cursor.into());
    }
    assert_eq!(seen, expected);
}

#[test]
fn cursors_reject_changed_filters_and_receipts_but_ignore_other_cells() {
    let (store, service, instance) = fixture();
    record(
        &store,
        &instance,
        "alice",
        "first",
        OperationPhase::Pending,
        Value::Null,
    );
    record(
        &store,
        &instance,
        "bob",
        "second",
        OperationPhase::Succeeded,
        json!({"stdout":"done"}),
    );
    let mut request = query(json!({"name":"payments", "limit":1}));
    let first = search(&service, request.clone());
    request.cursor = Some(first["next_cursor"].as_str().unwrap().into());
    let other = cell(&store, "other");
    record(
        &store,
        &other,
        "alice",
        "unrelated",
        OperationPhase::Succeeded,
        json!({"stdout":"unrelated"}),
    );
    assert_eq!(
        search(&service, request.clone())["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut changed = request.clone();
    changed.query = "different".into();
    assert_eq!(
        service
            .proofstorm_activity_search(Parameters(changed))
            .unwrap_err()
            .data
            .unwrap()["code"],
        "activity_search_cursor_invalid"
    );
    store
        .update_operation_phase("test", "first", OperationPhase::Running)
        .unwrap();
    assert_eq!(
        service
            .proofstorm_activity_search(Parameters(request.clone()))
            .unwrap_err()
            .data
            .unwrap()["code"],
        "activity_search_cursor_invalid"
    );
    request.cursor = None;
    request.cursor = Some(
        search(&service, request.clone())["next_cursor"]
            .as_str()
            .unwrap()
            .into(),
    );
    store
        .record_operation_result(
            "test",
            "first",
            OperationPhase::Failed,
            json!({"stdout":"database"}),
        )
        .unwrap();
    assert!(
        service
            .proofstorm_activity_search(Parameters(request))
            .is_err()
    );
}

#[test]
fn reads_unicode_text_arrays_and_escaped_json_paths_with_digest_checks() {
    let (store, service, instance) = fixture();
    let text = "界🙂\"\\\n".repeat(1500);
    record(
        &store,
        &instance,
        "alice",
        "long-output",
        OperationPhase::Succeeded,
        json!({"stdout":text, "a/b~c":[1,2,3], "null":null}),
    );
    let mut offset = 0;
    let mut collected = String::new();
    let mut digest = None;
    loop {
        let chunk = visible(read(&service, json!({"operation_id":"long-output",
            "pointer":"/artifact/content/stdout", "offset":offset, "limit":4000, "expected_digest":digest})).unwrap());
        assert_eq!(chunk["unit"], "characters");
        assert_eq!(chunk["total_length"], text.chars().count());
        collected.push_str(chunk["value"].as_str().unwrap());
        digest = Some(chunk["operation_digest"].as_str().unwrap().to_owned());
        let Some(next) = chunk["next_offset"].as_u64() else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(collected, text);
    let array = visible(read(&service, json!({"operation_id":"long-output", "pointer":"/artifact/content/a~1b~0c", "offset":1, "limit":1})).unwrap());
    assert_eq!(array["value"], json!([2]));
    assert_eq!(array["next_offset"], 2);
    assert!(
        visible(
            read(
                &service,
                json!({"operation_id":"long-output", "pointer":"/artifact/content/null"})
            )
            .unwrap()
        )["value"]
            .is_null()
    );
    for (extra, code) in [
        (json!({"expected_digest":"stale"}), "operation_read_changed"),
        (
            json!({"pointer":"/missing"}),
            "operation_read_pointer_missing",
        ),
        (json!({"pointer":"/~2"}), "invalid_json_pointer"),
        (
            json!({"pointer":"/artifact/content/stdout", "offset":999_999}),
            "operation_read_offset",
        ),
        (
            json!({"pointer":"/artifact/content"}),
            "operation_read_value_too_large",
        ),
    ] {
        let mut request = json!({"operation_id":"long-output"});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            read(&service, request).unwrap_err().data.unwrap()["code"],
            code
        );
    }
}

#[test]
fn cached_reads_preserve_unknown_and_private_output_and_recheck_authority() {
    let (store, service, instance) = fixture();
    record(
        &store,
        &instance,
        "alice",
        "pending",
        OperationPhase::Pending,
        Value::Null,
    );
    record(
        &store,
        &instance,
        "alice",
        "private",
        OperationPhase::Succeeded,
        json!({"exit_code":0, "stdout":null, "stderr":null, "output_mode":"private"}),
    );
    let pending = visible(read(&service, json!({"operation_id":"pending"})).unwrap());
    assert_eq!(pending["value"]["phase"], "pending");
    assert!(pending["value"]["artifact"].is_null());
    let private = search(
        &service,
        query(json!({"name":"payments", "query":"unrecorded-secret"})),
    );
    assert!(private["items"].as_array().unwrap().is_empty());
    assert!(service.tool_names().contains(&"activity_search".into()));
    assert!(service.tool_names().contains(&"operation_read".into()));
    proofstorm_app::developer::configure(&store, "foreign", "alice").unwrap();
    let foreign = ProofstormMcp::new(store.clone(), "foreign", "alice")
        .unwrap()
        .offline();
    assert!(read(&foreign, json!({"operation_id":"pending"})).is_err());
    assert!(
        foreign
            .proofstorm_activity_search(Parameters(query(json!({"name":"payments"}))))
            .is_err()
    );
    store
        .revoke("test", "alice", Capability::ArtifactRead)
        .unwrap();
    assert!(read(&service, json!({"operation_id":"pending"})).is_err());
    assert!(
        service
            .proofstorm_activity_search(Parameters(query(json!({"name":"payments"}))))
            .is_err()
    );
    let restricted = ProofstormMcp::new(store, "test", "alice").unwrap();
    assert!(!restricted.tool_names().contains(&"activity_search".into()));
    assert!(!restricted.tool_names().contains(&"operation_read".into()));
}

#[test]
fn malformed_searches_fail_with_recoverable_errors() {
    let (_, service, _) = fixture();
    for extra in [
        json!({"query":"[", "regex":true}),
        json!({"limit":0}),
        json!({"fields":["/bad~escape"]}),
        json!({"accepted_after_unix":2,"accepted_before_unix":1}),
    ] {
        let mut request = json!({"name":"payments"});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            service
                .proofstorm_activity_search(Parameters(query(request)))
                .is_err()
        );
    }
}

#[test]
fn native_exit_filter_does_not_confuse_execution_phase_with_command_success() {
    let (store, service, instance) = fixture();
    for (id, output) in [
        ("failed-command", json!({"exit_code":1})),
        ("successful-command", json!({"exit_code":0})),
        ("unknown-exit", json!({"cleanup_verified":true})),
    ] {
        record(
            &store,
            &instance,
            "bob",
            id,
            OperationPhase::Succeeded,
            output,
        );
    }
    let found = search(
        &service,
        query(json!({"name":"payments", "phase":"succeeded", "native_exit_code":1})),
    );
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["operation_id"], "failed-command");
}
