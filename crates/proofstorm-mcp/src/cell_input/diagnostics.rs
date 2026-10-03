//! Explain independent missing/unknown fields together after strict serde parsing fails.
//! This is diagnostic only: serde and semantic validation remain the admission gates.
use serde_json::{Value, json};
use std::sync::OnceLock;

const LIMIT: usize = 32;

/// Match the custom `CellInput` parser without opening files or changing admission.
pub(crate) fn input(value: &Value) -> Option<(Value, &'static Value)> {
    static AUTHORED: OnceLock<Value> = OnceLock::new();
    static CANONICAL: OnceLock<Value> = OnceLock::new();
    static FILE: OnceLock<Value> = OnceLock::new();
    let encoded = value.is_string();
    let value = if let Some(encoded) = value.as_str() {
        serde_json::from_str(encoded).ok()?
    } else {
        value.clone()
    };
    let schema = if !encoded
        && value
            .as_object()
            .is_some_and(|object| object.contains_key("file"))
    {
        FILE.get_or_init(|| json!(schemars::schema_for!(super::CellFile)))
    } else if value
        .get("links")
        .and_then(Value::as_array)
        .is_some_and(|links| links.iter().any(|link| link.get("binding").is_some()))
    {
        CANONICAL.get_or_init(|| json!(schemars::schema_for!(proofstorm_core::CellSpec)))
    } else {
        AUTHORED.get_or_init(|| json!(schemars::schema_for!(super::AuthoredCellSpec)))
    };
    Some((value, schema))
}

pub(super) fn describe(value: &Value, canonical: bool, original: &serde_json::Error) -> String {
    static AUTHORED: OnceLock<Value> = OnceLock::new();
    static CANONICAL: OnceLock<Value> = OnceLock::new();
    let schema = if canonical {
        CANONICAL.get_or_init(|| json!(schemars::schema_for!(proofstorm_core::CellSpec)))
    } else {
        AUTHORED.get_or_init(|| json!(schemars::schema_for!(super::AuthoredCellSpec)))
    };
    let mut issues = Vec::new();
    fields(value, schema, schema, "", &mut issues, 0);
    if issues.is_empty() {
        return original.to_string();
    }
    format!(
        "invalid cell structure: {}",
        json!({"issues":issues,"limit":LIMIT,"details_may_be_omitted":issues.len()==LIMIT,"parse_error":original.to_string()})
    )
}

fn fields(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    issues: &mut Vec<Value>,
    depth: usize,
) {
    if issues.len() >= LIMIT || depth > 32 {
        return;
    }
    if let Some(reference) = schema["$ref"].as_str().and_then(|s| s.strip_prefix('#')) {
        if let Some(resolved) = root.pointer(reference) {
            fields(value, resolved, root, path, issues, depth + 1);
        }
        return;
    }
    // Only select an unambiguous tagged variant. Never report requirements from
    // alternative branches that the caller did not choose.
    for keyword in ["oneOf", "anyOf"] {
        if let Some(variants) = schema[keyword].as_array() {
            if let Some(selected) = variants.iter().find(|variant| {
                let tag = &variant["properties"]["kind"];
                value["kind"].is_string()
                    && (tag["const"] == value["kind"]
                        || tag["enum"]
                            .as_array()
                            .is_some_and(|values| values.contains(&value["kind"])))
            }) {
                fields(value, selected, root, path, issues, depth + 1);
            }
            return;
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema["required"].as_array() {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) && issues.len() < LIMIT {
                    issues.push(json!({"path":pointer(path,key),"code":"missing_field"}));
                }
            }
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (key, value) in object {
                if let Some(child) = properties.get(key) {
                    fields(value, child, root, &pointer(path, key), issues, depth + 1);
                } else if schema["additionalProperties"] == false && issues.len() < LIMIT {
                    issues.push(json!({"path":pointer(path,key),"code":"unknown_field"}));
                }
            }
        }
    } else if let Some(items) = value.as_array() {
        for (index, value) in items.iter().enumerate() {
            if issues.len() >= LIMIT {
                break;
            }
            fields(
                value,
                &schema["items"],
                root,
                &format!("{path}/{index}"),
                issues,
                depth + 1,
            );
        }
    }
}

fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_missing_fields_and_unknown_role_are_reported_without_admission() {
        let value = json!({"components":[{"id":"chain","role":"bitcoin","implementation":"bitcoin-core"}],"links":[]});
        let error = serde_json::from_value::<super::super::CellInput>(value)
            .unwrap_err()
            .to_string();
        for path in [
            "/api_version",
            "/name",
            "/components/0/kind",
            "/components/0/config_version",
            "/components/0/control",
            "/components/0/config",
            "/components/0/role",
        ] {
            assert!(error.contains(path), "{path}: {error}");
        }
    }

    #[test]
    fn tagged_link_diagnostics_do_not_mix_variants_and_remain_bounded() {
        let value = json!({"api_version":"proofstorm/v1alpha1","name":"example","components":[],"links":[{"kind":"chain_backend"}]});
        let error = serde_json::from_value::<super::super::CellInput>(value)
            .unwrap_err()
            .to_string();
        for field in ["id", "from", "to", "network"] {
            assert!(error.contains(&format!("/links/0/{field}")), "{error}");
        }
        assert!(!error.contains("/method") && !error.contains("/protocol"));
        let value = json!({"components":vec![json!({});1000]});
        let error = serde_json::from_value::<super::super::CellInput>(value)
            .unwrap_err()
            .to_string();
        assert!(error.contains("\"details_may_be_omitted\":true"));
        assert!(error.len() < 4096);
    }

    #[test]
    fn authored_and_canonical_schemas_advertise_the_exact_api_version() {
        for schema in [
            json!(schemars::schema_for!(super::super::AuthoredCellSpec)),
            json!(schemars::schema_for!(proofstorm_core::CellSpec)),
        ] {
            assert_eq!(
                schema["properties"]["api_version"]["const"],
                proofstorm_core::API_VERSION
            );
        }
    }
}
