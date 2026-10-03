use super::*;
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
