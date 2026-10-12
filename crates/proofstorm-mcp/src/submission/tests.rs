use super::*;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{Value, json};

fn service() -> ProofstormMcp {
    let store = crate::tests::seeded_store();
    proofstorm_app::developer::configure(&store, "alpha", "designer").unwrap();
    ProofstormMcp::new(store, "alpha", "designer").unwrap()
}

fn document() -> Value {
    let mut value: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    value["name"] = json!("inline-cell");
    value.as_object_mut().unwrap().remove("policy");
    value
}

fn inline() -> Value {
    let document = document();
    json!({"name":"inline-cell","request_id":"author","components":document["components"],
        "links":[{"id":"mint-chain","kind":"chain_backend","from":"mint","to":"chain","network":"regtest"}]})
}

fn storage_document(size: Option<&str>) -> Value {
    let config = size.map_or_else(|| json!({}), |size| json!({"storage_size":size}));
    json!({"api_version":"proofstorm/v1alpha1","name":"storage-cell","links":[],
    "components":[
        {"id":"scripts","kind":"workspace","implementation":"workspace",
         "config_version":"workspace/0.1/v1","control":"workspace","config":config},
        {"id":"db","kind":"database","implementation":"postgresql",
         "config_version":"postgresql/17/v1","control":"cell","config":config}
    ]})
}

#[tokio::test]
async fn storage_resize_is_unsupported_in_preview_and_submission_before_runtime_access() {
    let mcp = service().with_kubernetes(crate::lifecycle_tests::cluster_client(), "system");
    let document = storage_document(Some("2Gi"));
    mcp.proofstorm_cell_up(Parameters(
        serde_json::from_value(json!({
            "name":"storage-cell","request_id":"create","cell":document
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let handle = mcp
        .store
        .resolve_cell("alpha", "designer", "storage-cell")
        .unwrap();
    let original = mcp
        .store
        .instance("alpha", "designer", &handle.instance_id)
        .unwrap();
    let state = json!(
        mcp.store
            .update_state("alpha", "designer", &handle.instance_id)
            .unwrap()
    );
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let client = kube::Client::new(
        tower::service_fn(move |_: http::Request<kube::client::Body>| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Err::<http::Response<kube::client::Body>, _>(
                std::io::Error::other(
                    "storage resize must be rejected before accessing Kubernetes",
                ),
            ))
        }),
        "system",
    );
    let mcp = mcp.with_kubernetes(client, "system");
    for index in 0..2 {
        for size in [Some("1Gi"), Some("5Gi"), None] {
            let mut edited = document.clone();
            edited["components"][index]["config"] =
                size.map_or_else(|| json!({}), |size| json!({"storage_size":size}));
            let key = format!("resize-{index}-{size:?}");
            let input = json!({"name":"storage-cell","request_id":key,"cell":edited,
                "expected_generation":original.generation,"expected_instance_key":original.instance_key});
            let plan = mcp
                .proofstorm_cell_plan(Parameters(serde_json::from_value(input.clone()).unwrap()))
                .unwrap()
                .structured_content
                .unwrap();
            assert_eq!(plan["changes"]["supported"], false);
            let mut direct = input;
            direct["request_id"] = json!(format!("{key}-direct"));
            for submission in [
                direct,
                json!({"name":"storage-cell","request_id":format!("{key}-saved"),"plan":plan["plan"]}),
            ] {
                let error = mcp
                    .proofstorm_cell_up(Parameters(serde_json::from_value(submission).unwrap()))
                    .await
                    .unwrap_err();
                assert_eq!(error.data.unwrap()["code"], "cell_update_unsupported");
                assert!(
                    error
                        .message
                        .contains(document["components"][index]["id"].as_str().unwrap())
                );
                assert!(error.message.contains("storage_size"));
                assert!(
                    error
                        .message
                        .contains("live storage resizing is unsupported")
                );
                assert_eq!(
                    mcp.store
                        .instance("alpha", "designer", &handle.instance_id)
                        .unwrap(),
                    original
                );
                assert_eq!(
                    json!(
                        mcp.store
                            .update_state("alpha", "designer", &handle.instance_id)
                            .unwrap()
                    ),
                    state
                );
                assert!(
                    mcp.store
                        .pending_updates("alpha", "designer")
                        .unwrap()
                        .is_empty()
                );
            }
        }
    }
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn storage_sizes_remain_configurable_for_new_components_and_defaults_are_equivalent() {
    let mcp = service().with_kubernetes(crate::lifecycle_tests::cluster_client(), "system");
    mcp.proofstorm_cell_up(Parameters(
        serde_json::from_value(json!({
            "name":"storage-cell","request_id":"create","cell":storage_document(None)
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let handle = mcp
        .store
        .resolve_cell("alpha", "designer", "storage-cell")
        .unwrap();
    let original = mcp
        .store
        .instance("alpha", "designer", &handle.instance_id)
        .unwrap();
    // Explicitly spelling the default is a no-op, not a resize.
    let mut document = storage_document(Some("1Gi"));
    let input = json!({"name":"storage-cell","request_id":"same-size","cell":document,
        "expected_generation":1,"expected_instance_key":original.instance_key});
    let plan = mcp
        .proofstorm_cell_plan(Parameters(serde_json::from_value(input.clone()).unwrap()))
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(plan["changes"]["supported"], true);
    assert_eq!(plan["changes"]["unchanged"], 2);
    mcp.proofstorm_cell_up(Parameters(serde_json::from_value(input).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        mcp.store
            .instance("alpha", "designer", &handle.instance_id)
            .unwrap(),
        original
    );

    let mut additions = storage_document(Some("10Gi"));
    for component in additions["components"].as_array_mut().unwrap() {
        component["id"] = json!(format!("new-{}", component["id"].as_str().unwrap()));
        document["components"]
            .as_array_mut()
            .unwrap()
            .push(component.clone());
    }
    let input = json!({"name":"storage-cell","request_id":"add-storage","cell":document,
        "expected_generation":1,"expected_instance_key":original.instance_key});
    let plan = mcp
        .proofstorm_cell_plan(Parameters(serde_json::from_value(input.clone()).unwrap()))
        .unwrap()
        .structured_content
        .unwrap();
    assert_eq!(plan["changes"]["supported"], true);
    assert_eq!(plan["changes"]["added"], 2);
    mcp.proofstorm_cell_up(Parameters(serde_json::from_value(input).unwrap()))
        .await
        .unwrap();
    let updated = mcp
        .store
        .instance("alpha", "designer", &handle.instance_id)
        .unwrap();
    assert_eq!(updated.generation, 2);
    assert_eq!(updated.instance_key, original.instance_key);
    let revision = mcp
        .store
        .revision("alpha", "designer", &updated.revision_digest)
        .unwrap();
    for component in &revision.cell.components {
        assert_eq!(
            component.config["storage_size"],
            if component.id.starts_with("new-") {
                "10Gi"
            } else {
                "1Gi"
            }
        );
    }
}

#[test]
fn inline_and_document_forms_share_preview_and_exact_retry_identity() {
    let mcp = service();
    let flat: SubmissionRequest = serde_json::from_value(inline()).unwrap();
    let roundtrip: SubmissionRequest =
        serde_json::from_value(serde_json::to_value(&flat).unwrap()).unwrap();
    let plan = mcp.prepare_submission(roundtrip, false).unwrap();
    let full = json!({"name":"inline-cell","request_id":"author","cell":document()});
    let same = mcp
        .prepare_submission(serde_json::from_value(full).unwrap(), false)
        .unwrap();
    assert_eq!(digest_json(&plan), digest_json(&same));
    let mut different = inline();
    different["components"][0]["config"]["txindex"] = json!(false);
    let error = mcp
        .prepare_submission(serde_json::from_value(different).unwrap(), false)
        .unwrap_err();
    assert_eq!(error.data.unwrap()["code"], "idempotency_conflict");
    assert!(
        mcp.store
            .resolve_cell("alpha", "designer", "inline-cell")
            .is_err(),
        "preview must not create a runtime cell"
    );
}

#[test]
fn inline_requires_both_arrays_and_cannot_mix_other_forms_or_fences() {
    let mcp = service();
    let mut cases = vec![
        json!({"name":"inline-cell","request_id":"author","components":[]}),
        json!({"name":"inline-cell","request_id":"author","links":[]}),
        json!({"name":"inline-cell","request_id":"author","policy":{}}),
    ];
    for (field, value) in [
        ("cell", document()),
        ("patch", json!([])),
        ("plan", json!({"id":"absent","digest":"absent"})),
        ("expected_generation", json!(1)),
        ("expected_instance_key", json!("absent")),
    ] {
        let mut candidate = inline();
        candidate[field] = value;
        cases.push(candidate);
    }
    for value in cases {
        let error = mcp
            .prepare_submission(serde_json::from_value(value).unwrap(), false)
            .unwrap_err();
        assert!(matches!(
            error.data.unwrap()["code"].as_str(),
            Some("invalid_cell_input" | "cell_update_conflict")
        ));
    }
    // Failed submissions must not occupy the request ID or publish a cell.
    assert!(
        mcp.prepare_submission(serde_json::from_value(inline()).unwrap(), false)
            .is_ok()
    );
    assert!(
        mcp.store
            .resolve_cell("alpha", "designer", "inline-cell")
            .is_err()
    );
}

#[test]
fn inline_preserves_strict_components_links_policy_and_null_rejection() {
    for field in ["components", "links", "policy"] {
        let mut value = inline();
        value[field] = Value::Null;
        assert!(serde_json::from_value::<SubmissionRequest>(value).is_err());
    }
    for field in [
        "id",
        "kind",
        "implementation",
        "config_version",
        "control",
        "config",
    ] {
        let mut value = inline();
        value["components"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            serde_json::from_value::<SubmissionRequest>(value).is_err(),
            "missing {field}"
        );
    }
    for (pointer, invalid) in [
        ("/components/0/control", json!("invented")),
        ("/links/0/network", json!("invented")),
        ("/components", json!("[]")),
        ("/links", json!({})),
    ] {
        let mut value = inline();
        *value.pointer_mut(pointer).unwrap() = invalid;
        assert!(serde_json::from_value::<SubmissionRequest>(value).is_err());
    }
    let mut bad_link = inline();
    bad_link["links"][0]["unit"] = json!("sat");
    assert!(serde_json::from_value::<SubmissionRequest>(bad_link).is_err());
    let mut restricted = inline();
    restricted["policy"] =
        json!({"limits":{"max_components":1,"max_links":256,"max_config_bytes":65536}});
    assert!(
        service()
            .prepare_submission(serde_json::from_value(restricted).unwrap(), false)
            .is_err()
    );
}

#[test]
fn component_parameter_is_inline_and_optional_without_nullable_union() {
    let mcp = service();
    for name in ["cell_plan", "cell_up"] {
        let tool = mcp
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap();
        let schema = json!(tool.input_schema);
        let components = &schema["properties"]["components"];
        assert_eq!(components["type"], "array");
        assert!(components.get("anyOf").is_none());
        assert!(!components.to_string().contains("\"$ref\""));
        assert!(!components.to_string().contains("\"$defs\""));
        assert_eq!(components["items"]["additionalProperties"], false);
        for field in ["config_version", "control", "kind"] {
            assert!(
                components["items"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(field))
            );
            assert_eq!(components["items"]["properties"][field]["type"], "string");
        }
        assert_eq!(schema["required"], json!(["name", "request_id"]));
        assert_eq!(schema["properties"]["links"]["type"], "array");
        assert_eq!(schema["properties"]["policy"]["type"], "object");
    }
}
