use super::*;

fn diagnose<T: JsonSchema>(value: &Value, cell_inputs: bool) -> schema::Diagnostics {
    schema::describe(value, &schemars::schema_for!(T).to_value(), cell_inputs)
}

fn issue(result: &schema::Diagnostics, path: &str) -> Value {
    result
        .issues
        .iter()
        .find(|issue| issue["path"] == path)
        .unwrap_or_else(|| panic!("missing {path}: {:?}", result.issues))
        .clone()
}

#[test]
fn inline_component_errors_keep_precise_paths_and_strict_types() {
    let value = json!({"name":"cell","request_id":"author","components":[{
        "id":"node","kind":"bitcoin","implementation":"bitcoin-core","control":"targte","config":{}}
    ],"links":[]});
    assert!(serde_json::from_value::<crate::SubmissionRequest>(value.clone()).is_err());
    let result = diagnose::<crate::SubmissionRequest>(&value, true);
    assert_eq!(
        issue(&result, "/components/0/config_version")["code"],
        "missing_field"
    );
    assert_eq!(
        issue(&result, "/components/0/control")["did_you_mean"],
        "target"
    );
    let mut null = value;
    null["components"] = Value::Null;
    let result = diagnose::<crate::SubmissionRequest>(&null, true);
    assert_eq!(issue(&result, "/components")["code"], "invalid_type");
}

#[test]
fn fields_types_enums_and_tags_use_the_actual_request_schemas() {
    let result = diagnose::<crate::CellExecRequest>(
        &json!({
            "name":"cell", "componnet":"node", "request_id":"call", "argv":"secret-canary",
            "timeout_seconds":-1, "output":{"mode":"publci"}
        }),
        false,
    );
    assert_eq!(issue(&result, "/component")["code"], "missing_field");
    assert_eq!(issue(&result, "/componnet")["did_you_mean"], "component");
    assert_eq!(issue(&result, "/argv")["code"], "invalid_type");
    assert_eq!(issue(&result, "/timeout_seconds")["code"], "out_of_range");
    assert_eq!(issue(&result, "/output/mode")["did_you_mean"], "public");
    assert!(!json!(result.issues).to_string().contains("secret-canary"));

    for (action, code, hint) in [
        (json!("strat"), "invalid_variant", Some("start")),
        (Value::Null, "invalid_variant", None),
    ] {
        let result = diagnose::<crate::WorkspaceTaskRequest>(
            &json!({"name":"cell","component":"workspace","request_id":"call","task":{"action":action}}),
            false,
        );
        let issue = issue(&result, "/task/action");
        assert_eq!(issue["code"], code);
        assert_eq!(issue["did_you_mean"].as_str(), hint);
        assert!(
            !result
                .issues
                .iter()
                .any(|issue| issue["path"] == "/task/task_id")
        );
    }
    let result = diagnose::<crate::SubmissionRequest>(
        &json!({"name":"cell","request_id":"call","patch":[{"op":"remove_link"}]}),
        true,
    );
    assert_eq!(issue(&result, "/patch/0/id")["code"], "missing_field");
    assert!(
        !result
            .issues
            .iter()
            .any(|issue| issue["path"] == "/patch/0/link")
    );
}

#[test]
fn cell_diagnostics_match_inline_encoded_file_and_canonical_forms() {
    let canonical: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json")).unwrap();
    for cell in [
        canonical.clone(),
        json!(canonical.to_string()),
        json!({"file":"/file-must-not-be-opened.json"}),
        Value::Null,
    ] {
        let request =
            json!({"name":"cell","request_id":"call","cell":cell,"delete_data":"wrong-type"});
        assert!(serde_json::from_value::<crate::SubmissionRequest>(request.clone()).is_err());
        let result = diagnose::<crate::SubmissionRequest>(&request, true);
        assert_eq!(result.issues.len(), 1, "{:?}", result.issues);
        assert_eq!(issue(&result, "/delete_data")["code"], "invalid_type");
    }
    let malformed = json!({"components":[{"id":"chain","role":"bitcoin","implementation":"bitcoin-core"}],"links":[{"kind":"chain_backend"}]});
    for cell in [malformed.clone(), json!(malformed.to_string())] {
        let result = diagnose::<crate::SubmissionRequest>(
            &json!({"name":"cell","request_id":"call","cell":cell}),
            true,
        );
        assert_eq!(
            issue(&result, "/cell/api_version")["example"],
            proofstorm_core::API_VERSION
        );
        assert_eq!(
            issue(&result, "/cell/components/0/role")["code"],
            "unknown_field"
        );
        assert_eq!(
            issue(&result, "/cell/links/0/network")["code"],
            "missing_field"
        );
        assert!(
            !result
                .issues
                .iter()
                .any(|issue| issue["path"] == "/cell/links/0/method")
        );
    }
    let encoded_file =
        json!({"name":"cell","request_id":"call","cell":json!({"file":"/not-opened"}).to_string()});
    let result = diagnose::<crate::SubmissionRequest>(&encoded_file, true);
    assert_eq!(issue(&result, "/cell/api_version")["code"], "missing_field");
}

#[test]
fn ambiguous_unions_cycles_and_large_inputs_do_not_invent_or_overflow_diagnostics() {
    let schema = json!({"type":"object","properties":{"choice":{"anyOf":[
        {"type":"object","properties":{"a":{"type":"string"}},"required":["a"]},
        {"type":"object","properties":{"b":{"type":"string"}},"required":["b"]}
    ]}}});
    let result = schema::describe(&json!({"choice":{}}), &schema, false);
    assert!(result.incomplete && result.issues.is_empty());
    let cycle = json!({"$ref":"#/$defs/Loop","$defs":{"Loop":{"$ref":"#/$defs/Loop"}}});
    assert!(schema::describe(&json!({}), &cycle, false).incomplete);
    let bad: serde_json::Map<_, _> = (0..1000)
        .map(|i| (format!("bad-{i}"), json!("secret-canary")))
        .collect();
    let result = diagnose::<crate::CellExecRequest>(&Value::Object(bad), false);
    assert!(result.incomplete && result.issues.len() <= 16);
    assert!(json!(result.issues).to_string().len() <= 6 * 1024);
    for value in [
        json!({"argv":"secret-canary".repeat(20_000)}),
        json!({"argv":vec![0;5000]}),
    ] {
        assert!(snapshot(value.as_object().unwrap()).is_none());
    }
    let error = explain(
        ErrorData::invalid_params("failed to deserialize parameters: secret-canary", None),
        None,
        &schema,
        false,
    );
    let wire = crate::tool_error::result(error);
    assert_eq!(
        wire.structured_content.as_ref().unwrap()["data"]["details_may_be_omitted"],
        true
    );
    assert!(!json!(wire).to_string().contains("secret-canary"));
    assert!(crate::serialized_size(&wire).unwrap() <= crate::MAX_AGENT_RESPONSE_BYTES);
}

#[test]
fn domain_errors_are_preserved_and_spelling_ties_are_not_guessed() {
    let error = ErrorData::invalid_params(
        "field is not allowed",
        Some(json!({"code":"native_output_invalid"})),
    );
    assert_eq!(
        explain(error.clone(), Some(&json!({})), &json!({}), false),
        error
    );
    let schema = json!({"type":"object","additionalProperties":false,"properties":{"cat":{"type":"string"},"bat":{"type":"string"}}});
    let result = schema::describe(&json!({"hat":1}), &schema, false);
    assert!(issue(&result, "/hat").get("did_you_mean").is_none());
    let result = schema::describe(&json!({"a/b~c":"secret-canary"}), &schema, false);
    assert_eq!(result.issues[0]["path"], "/a~1b~0c");
}
