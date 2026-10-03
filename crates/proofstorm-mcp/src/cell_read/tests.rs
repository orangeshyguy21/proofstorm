use super::*;
use rmcp::handler::server::wrapper::Parameters;

fn service() -> ProofstormMcp {
    ProofstormMcp::new(crate::tests::seeded_store(), "alpha", "designer").unwrap()
}

fn error_data(error: ErrorData) -> Value {
    let result = crate::tool_error::result(error);
    let wire = json!(result);
    assert_eq!(
        serde_json::from_str::<Value>(wire["content"][0]["text"].as_str().unwrap()).unwrap(),
        wire["structuredContent"]
    );
    assert!(crate::serialized_size(&result).unwrap() <= crate::MAX_AGENT_RESPONSE_BYTES);
    wire["structuredContent"]["data"].clone()
}

#[test]
fn missing_cell_paths_recover_with_the_selected_document_digest_and_recheck_access() {
    let mcp = service();
    let cell: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    let preview = mcp
        .prepare_submission(
            serde_json::from_value(json!({"name":"paths", "request_id":"preview", "cell":cell}))
                .unwrap(),
            false,
        )
        .unwrap();
    for document in ["configuration", "plan", "lock"] {
        let mut request: CellReadRequest = serde_json::from_value(json!({
            "plan_id":preview.id, "document":document, "pointer":"/absent/deeper"
        }))
        .unwrap();
        let data = error_data(mcp.read_cell_document(&request).unwrap_err());
        assert_eq!(data["code"], "cell_read_pointer_missing");
        assert_eq!(data["next_tool"], "cell_read");
        assert_eq!(data["document"], document);
        let issue = &data["issues"][0];
        assert_eq!(issue["path"], "/pointer");
        let root = mcp
            .read_cell_document(&CellReadRequest {
                pointer: String::new(),
                ..request.clone()
            })
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(data["document_digest"], root["document_digest"]);
        if document != "configuration" {
            assert_ne!(data["document_digest"], root["cell_digest"]);
        }
        request.pointer = issue["example"].as_str().unwrap().into();
        request.expected_digest = Some(data["document_digest"].as_str().unwrap().into());
        let recovered = mcp
            .read_cell_document(&request)
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(recovered["document_digest"], data["document_digest"]);
        assert_eq!(
            recovered["value"],
            *root["value"].pointer(&request.pointer).unwrap()
        );
        request.expected_digest = Some("stale-digest".into());
        assert_eq!(
            mcp.read_cell_document(&request).unwrap_err().data.unwrap()["code"],
            "cell_read_changed"
        );
    }
    let request: CellReadRequest =
        serde_json::from_value(json!({"plan_id":preview.id,"pointer":"/absent"})).unwrap();
    let other = ProofstormMcp::new(mcp.store.clone(), "alpha", "reader").unwrap();
    let absent = error_data(other.read_cell_document(&request).unwrap_err());
    assert_eq!(absent["code"], "cell_plan_missing");
    assert!(absent.get("issues").is_none());
    mcp.store.replace_grants("alpha", "designer", []).unwrap();
    let denied = error_data(mcp.read_cell_document(&request).unwrap_err());
    assert_eq!(denied["code"], "access_denied");
    assert!(denied.get("issues").is_none());
    assert!(denied.get("document_digest").is_none());
}

#[test]
fn cell_read_bounds_and_scalar_scan_errors_supply_usable_corrections() {
    let document: CellReadResponse = serde_json::from_value(json!({
        "id":"cell","workspace_id":"alpha","version":1,
        "cell":{"api_version":"proofstorm/v1alpha1","name":"🦀a", "components":[],"links":[]}
    }))
    .unwrap();
    for (input, code, field) in [
        (json!({"limit":0}), "cell_read_limit", "limit"),
        (json!({"limit":4001}), "cell_read_limit", "limit"),
        (
            json!({"pointer":"/name", "offset":3}),
            "cell_read_offset",
            "offset",
        ),
        (
            json!({"pointer":"/components", "offset":1}),
            "cell_read_offset",
            "offset",
        ),
        (
            json!({"pointer":"/policy", "offset":1}),
            "cell_read_offset",
            "offset",
        ),
        (
            json!({"pointer":"/name", "scan":true}),
            "cell_read_scan",
            "scan",
        ),
    ] {
        let request: CellReadRequest = serde_json::from_value(input.clone()).unwrap();
        let data = error_data(read(&document, &request).unwrap_err());
        assert_eq!(data["code"], code);
        let issue = &data["issues"][0];
        assert_eq!(issue["path"], format!("/{field}"));
        if code == "cell_read_limit" {
            assert_eq!(issue["expected"], json!({"minimum":1,"maximum":4000}));
        } else if code == "cell_read_offset" {
            assert_eq!(
                issue["expected"]["maximum"],
                if request.pointer == "/name" { 2 } else { 0 }
            );
        }
        let mut corrected = input;
        corrected[field] = issue["example"].clone();
        assert!(read(&document, &serde_json::from_value(corrected).unwrap()).is_ok());
    }
    let end = read(
        &document,
        &serde_json::from_value(json!({"pointer":"/name", "offset":2})).unwrap(),
    )
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(end["value"], "");
    assert_eq!(end["next_offset"], Value::Null);
}

#[test]
fn catalog_paths_keep_the_resource_error_and_only_reveal_visible_schema_paths() {
    let mcp = service();
    let catalog = proofstorm_core::default_catalog();
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == "bitcoin-core")
        .unwrap();
    let mut request = crate::CatalogConfigSchemaRequest {
        id: entry.id.clone(),
        version: entry.version.clone(),
        pointer: "/properties/missing/deeper".into(),
    };
    let error = mcp
        .proofstorm_catalog_config_schema_read(Parameters(request.clone()))
        .map(|result| result.0)
        .unwrap_err();
    assert_eq!(error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    let data = error_data(error);
    assert_eq!(data["code"], "catalog_schema_pointer_not_found");
    assert_eq!(data["config_schema_digest"], entry.config_schema_digest);
    assert_eq!(data["next_tool"], "catalog_config_schema_read");
    assert_eq!(
        data["issues"][0]["expected"]["existing_parent"],
        "/properties"
    );
    for pointer in data["issues"][0]["expected"]["available_pointers"]
        .as_array()
        .unwrap()
    {
        assert!(
            entry
                .config_schema
                .pointer(pointer.as_str().unwrap())
                .is_some()
        );
    }
    request.pointer = data["issues"][0]["example"].as_str().unwrap().into();
    let recovered = mcp
        .proofstorm_catalog_config_schema_read(Parameters(request.clone()))
        .unwrap()
        .0;
    assert_eq!(
        recovered.schema,
        *entry.config_schema.pointer(&request.pointer).unwrap()
    );
    assert_eq!(recovered.config_schema_digest, entry.config_schema_digest);
    request.pointer = "missing-leading-slash".into();
    let invalid = error_data(
        mcp.proofstorm_catalog_config_schema_read(Parameters(request.clone()))
            .map(|result| result.0)
            .unwrap_err(),
    );
    assert_eq!(invalid["code"], "catalog_schema_pointer_invalid");
    request.pointer = invalid["issues"][0]["example"].as_str().unwrap().into();
    assert!(
        mcp.proofstorm_catalog_config_schema_read(Parameters(request.clone()))
            .is_ok()
    );
    request.id = "unavailable-component".into();
    let absent = error_data(
        mcp.proofstorm_catalog_config_schema_read(Parameters(request.clone()))
            .map(|result| result.0)
            .unwrap_err(),
    );
    assert_eq!(absent["code"], "catalog_entry_not_found");
    assert!(absent.get("issues").is_none());
    request.id = entry.id.clone();
    mcp.store.replace_grants("alpha", "designer", []).unwrap();
    let denied = error_data(
        mcp.proofstorm_catalog_config_schema_read(Parameters(request))
            .map(|result| result.0)
            .unwrap_err(),
    );
    assert_eq!(denied["code"], "access_denied");
    assert!(denied.get("issues").is_none());
    assert!(denied.get("config_schema_digest").is_none());
}
