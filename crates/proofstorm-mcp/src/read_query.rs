//! Transport error mapping for shared bounded-query primitives.
use crate::{
    CallToolResult, ErrorData, app_error,
    input_error::{self, Issue},
};
use proofstorm_app::query;
pub(super) use query::project;
use serde_json::{Value, json};

pub(super) fn pattern(
    query: &str,
    regex: bool,
    insensitive: bool,
) -> Result<regex::Regex, ErrorData> {
    query::pattern(query, regex, insensitive).map_err(app_error)
}

pub(super) fn validate_pointer(pointer: &str) -> Result<(), ErrorData> {
    validate_pointer_at(pointer, "/pointer")
}

pub(super) fn validate_pointer_at(pointer: &str, path: &str) -> Result<(), ErrorData> {
    use proofstorm_app::query::{self, PointerError};
    query::validate_pointer(pointer, 4096).map_err(|error| {
        input_error::invalid("invalid_json_pointer", match error {
            PointerError::RootOrLength => "Use an RFC 6901 pointer of at most 4096 bytes, such as /artifact/content/stdout",
            PointerError::Escape => "Escape ~ as ~0 and / inside a key as ~1",
        }, &[Issue {
            path: path.into(), code: "invalid_pointer",
            expected: json!({"format":"RFC 6901", "maximum_bytes":4096, "empty_selects_root":true, "escape":{"~":"~0","/":"~1"}}),
            example: json!(""),
        }])
    })
}

/// Call only with an authorized document. Suggestions contain paths, never values.
pub(super) fn missing_pointer(document: &Value, pointer: &str) -> Issue {
    // Catalog reads historically accept unbounded pointers. Do not traverse an
    // unbounded rejected input just to diagnose it; the root is always readable.
    let mut parent_shortened = pointer.len() > 4096;
    let mut parent = if parent_shortened { "" } else { pointer };
    let value = loop {
        parent = parent.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        if let Some(value) = document.pointer(parent) {
            // Bound encoded bytes: control characters can expand sixfold in JSON.
            if json!(parent).to_string().len() <= 512 {
                break value;
            }
            parent_shortened = true;
        }
    };
    let mut pointers = Vec::new();
    let mut omitted = parent_shortened;
    let mut add = |key: &str| {
        if pointers.len() == 16 || parent.len() + key.len() + 1 > 256 {
            omitted = true;
            return;
        }
        let pointer = format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"));
        if json!(pointer).to_string().len() <= 256 {
            pointers.push(pointer);
        } else {
            omitted = true;
        }
    };
    match value {
        Value::Object(object) => {
            for key in object.keys().take(17) {
                add(key);
            }
        }
        Value::Array(items) => {
            for index in 0..items.len().min(17) {
                add(&index.to_string());
            }
        }
        _ => (),
    }
    Issue {
        path: "/pointer".into(),
        code: "pointer_missing",
        example: json!(pointers.first().map_or(parent, String::as_str)),
        expected: json!({"existing_parent":parent,"available_pointers":pointers,"pointers_omitted":omitted,"parent_shortened":parent_shortened}),
    }
}

pub(super) fn validate_fields(fields: &[String]) -> Result<(), ErrorData> {
    query::validate_fields(fields).map_err(app_error)
}

pub(super) fn wire(value: &impl serde::Serialize) -> Result<CallToolResult, ErrorData> {
    query::wire(value).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

pub(super) fn wire_size(value: &impl serde::Serialize) -> Result<usize, ErrorData> {
    query::wire_size(value).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_paths_remain_readable_and_bounded_after_json_escaping() {
        let escaped_parent = "\0".repeat(700);
        let cases = [
            (
                json!({"body":(0..100).map(|i| (format!("key/{i}~"),json!("sibling-canary"))).collect::<serde_json::Map<_,_>>()}),
                "/body/missing/deeper".into(),
            ),
            (
                json!({"body":vec!["sibling-canary";100]}),
                "/body/100/absent".into(),
            ),
            (json!({"body":null}), "/body/absent".into()),
            (
                json!({"body":{"\0".repeat(200):"sibling-canary"}}),
                "/body/absent".into(),
            ),
            (
                json!({&escaped_parent:{"child":"sibling-canary"}}),
                format!("/{escaped_parent}/absent"),
            ),
            (
                json!({"body":"sibling-canary"}),
                format!("/{}", "x".repeat(100_000)),
            ),
        ];
        for (document, pointer) in cases {
            let issue = missing_pointer(&document, &pointer);
            assert!(document.pointer(issue.example.as_str().unwrap()).is_some());
            assert!(
                document
                    .pointer(issue.expected["existing_parent"].as_str().unwrap())
                    .is_some()
            );
            let pointers = issue.expected["available_pointers"].as_array().unwrap();
            assert!(pointers.len() <= 16);
            for pointer in pointers {
                assert!(document.pointer(pointer.as_str().unwrap()).is_some());
                assert!(pointer.to_string().len() <= 256);
            }
            let error = input_error::invalid("test_pointer_missing", "Absent pointer", &[issue]);
            let wire = crate::tool_error::result(error);
            assert!(crate::serialized_size(&wire).unwrap() <= crate::MAX_AGENT_RESPONSE_BYTES);
            let data = &wire.structured_content.as_ref().unwrap()["data"];
            assert!(
                data["issues"].is_array(),
                "bounded errors retain corrections"
            );
            assert!(!data.to_string().contains("sibling-canary"));
        }
        let issue = missing_pointer(
            &json!({escaped_parent:{"child":0}}),
            &format!("/{}//missing", "\0".repeat(700)),
        );
        assert_eq!(issue.expected["parent_shortened"], true);
        assert_eq!(issue.expected["pointers_omitted"], true);
        assert_eq!(issue.expected["existing_parent"], "");
    }

    #[test]
    fn transport_errors_preserve_codes_and_longer_individual_pointer_limits() {
        for (result, code) in [
            (
                pattern(&"x".repeat(4097), false, false).map(|_| ()),
                "search_query_invalid",
            ),
            (
                pattern("[", true, false).map(|_| ()),
                "search_regex_invalid",
            ),
            (validate_fields(&["/bad~".into()]), "search_fields_invalid"),
        ] {
            assert_eq!(result.unwrap_err().data.unwrap()["code"], code);
        }
        let long = format!("/{}", "x".repeat(512));
        assert!(validate_fields(std::slice::from_ref(&long)).is_err());
        assert!(validate_pointer(&long).is_ok());
        assert!(validate_pointer(&format!("/{}", "x".repeat(4095))).is_ok());
        for pointer in ["/bad~".into(), format!("/{}", "x".repeat(4096))] {
            let data = validate_pointer(&pointer).unwrap_err().data.unwrap();
            assert_eq!(data["code"], "invalid_json_pointer");
            assert_eq!(data["issues"][0]["path"], "/pointer");
            assert_eq!(data["issues"][0]["expected"]["maximum_bytes"], 4096);
            assert!(validate_pointer(data["issues"][0]["example"].as_str().unwrap()).is_ok());
        }
        let request =
            serde_json::from_value(serde_json::json!({"name":"cell","query":"[","regex":true}))
                .unwrap();
        let error = crate::activity_search::search(
            &proofstorm_store::Store::memory().unwrap(),
            "workspace",
            "actor",
            &request,
        )
        .unwrap_err();
        assert_eq!(error.data.unwrap()["code"], "activity_search_regex_invalid");
    }
}
