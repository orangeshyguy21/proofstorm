//! Provider-portable input schemas without changing request validation.
use std::{collections::BTreeSet, sync::Arc};

use schemars::{Schema, transform::transform_subschemas};
use serde_json::{Map, Value};

pub(crate) fn portable_input(input: &Map<String, Value>) -> Arc<Map<String, Value>> {
    let mut schema = Schema::from(input.clone());
    tagged_unions(&mut schema);
    let root = schema.clone().to_value();
    reference_types(&mut schema, &root);
    let Value::Object(object) = schema.to_value() else {
        unreachable!("an object schema stays an object")
    };
    Arc::new(object)
}

fn reference_types(schema: &mut Schema, root: &Value) {
    if schema.get("$id").is_some() {
        return;
    }
    // Ollama's Qwen XML parser converts arguments using the direct property
    // type, without following $ref. Expose only types already required by the
    // referenced schema; preserve the reference and all validation constraints.
    if schema.get("type").is_none()
        && let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(types) = referred_types(reference, root, &mut BTreeSet::new())
    {
        let value = if types.len() == 1 {
            Value::String(types.into_iter().next().unwrap())
        } else {
            Value::Array(types.into_iter().map(Value::String).collect())
        };
        schema.insert("type".into(), value);
    }
    transform_subschemas(
        &mut |child: &mut Schema| reference_types(child, root),
        schema,
    );
}

fn referred_types(
    reference: &str,
    root: &Value,
    seen: &mut BTreeSet<String>,
) -> Option<BTreeSet<String>> {
    // Schemars uses root-local definitions. Do not fetch external references,
    // infer through cycles, or resolve arbitrary anchors/resource scopes.
    let name = reference.strip_prefix("#/$defs/")?;
    if name.contains('/') || seen.len() >= 32 || !seen.insert(reference.into()) {
        return None;
    }
    let result = root
        .pointer(reference.strip_prefix('#')?)
        .and_then(|node| implied_types(node, root, seen));
    seen.remove(reference);
    result
}

fn implied_types(
    node: &Value,
    root: &Value,
    seen: &mut BTreeSet<String>,
) -> Option<BTreeSet<String>> {
    if node.get("$id").is_some() {
        return None;
    }
    if let Some(value) = node.get("type") {
        let values = match value {
            Value::String(_) => vec![value],
            Value::Array(values) => values.iter().collect(),
            _ => return None,
        };
        let types: Option<BTreeSet<_>> = values
            .into_iter()
            .map(|value| {
                let name = value.as_str()?;
                matches!(
                    name,
                    "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
                )
                .then(|| name.to_owned())
            })
            .collect();
        return types.filter(|types| !types.is_empty());
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        return referred_types(reference, root, seen);
    }
    let branches = node
        .get("anyOf")
        .or_else(|| node.get("oneOf"))?
        .as_array()?;
    if branches.is_empty() {
        return None;
    }
    let mut types = BTreeSet::new();
    for branch in branches {
        types.extend(implied_types(branch, root, seen)?);
    }
    Some(types)
}

fn tagged_unions(schema: &mut Schema) {
    // Moonshot rejects oneOf, sometimes reporting infinite recursion when it
    // is reached through a nullable $ref. For disjoint tagged object variants,
    // anyOf is equivalent: the required tag can match at most one branch.
    // Never weaken overlapping unions or overwrite an existing anyOf.
    if schema.get("anyOf").is_none()
        && schema
            .get("oneOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| disjoint_tags(branches))
    {
        let branches = schema.remove("oneOf").unwrap();
        schema.insert("anyOf".into(), branches);
    }
    // Visit schemas only; defaults/examples and literal property names can
    // themselves contain "oneOf" without being schema composition keywords.
    transform_subschemas(&mut tagged_unions, schema);
}

fn disjoint_tags(branches: &[Value]) -> bool {
    let Some(properties) = branches.first().and_then(|b| b["properties"].as_object()) else {
        return false;
    };
    properties.keys().any(|tag| {
        let mut values = BTreeSet::new();
        branches.iter().all(|branch| {
            branch["type"] == "object"
                && branch["required"].as_array().is_some_and(|required| {
                    required.iter().any(|field| field.as_str() == Some(tag))
                })
                && branch["properties"][tag]["const"]
                    .as_str()
                    .is_some_and(|value| values.insert(value))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn referenced_types_are_explicit_without_inlining_or_changing_constraints() {
        let input = json!({"type":"object","$defs":{
            "Output":{"type":"object","additionalProperties":false,"properties":{"mode":{"$ref":"#/$defs/Mode"}}},
            "Mode":{"type":"string","enum":["private","public"]},
            "Nullable":{"anyOf":[{"$ref":"#/$defs/Output"},{"type":"null"}]},
            "Alias":{"$ref":"#/$defs/Nullable"}
        },"properties":{
            "output":{"$ref":"#/$defs/Output","description":"Select output","default":{"$ref":"literal","type":"literal"}},
            "nullable":{"$ref":"#/$defs/Alias"}
        }});
        let mut expected = input.clone();
        expected["properties"]["output"]["type"] = json!("object");
        expected["properties"]["nullable"]["type"] = json!(["null", "object"]);
        expected["$defs"]["Output"]["properties"]["mode"]["type"] = json!("string");
        expected["$defs"]["Nullable"]["anyOf"][0]["type"] = json!("object");
        expected["$defs"]["Alias"]["type"] = json!(["null", "object"]);
        let result = portable_input(input.as_object().unwrap());
        assert_eq!(json!(result), expected);
        assert_eq!(portable_input(&result), result);
    }

    #[test]
    fn ambiguous_cyclic_external_or_scoped_references_are_not_guessed() {
        let input = json!({"type":"object","$defs":{
            "A":{"$ref":"#/$defs/B"},"B":{"$ref":"#/$defs/A"},
            "Unknown":{"anyOf":[{"type":"object"},{}]},
            "Object":{"type":"object"},
            "Scoped":{"$id":"https://example.test/other","properties":{"value":{"$ref":"#/$defs/Object"}}}
        },"properties":{
            "cyclic":{"$ref":"#/$defs/A"},"unknown":{"$ref":"#/$defs/Unknown"},
            "missing":{"$ref":"#/$defs/Missing"},"external":{"$ref":"https://example.test/other"},
            "existing":{"$ref":"#/$defs/Object","type":"string"},
            "scoped":{"$ref":"#/$defs/Scoped"}
        }});
        assert_eq!(json!(portable_input(input.as_object().unwrap())), input);
    }

    fn variants() -> Value {
        json!({"oneOf":[
            {"type":"object","required":["kind","url"],"additionalProperties":false,"properties":{"kind":{"const":"pr","type":"string"},"url":{"type":"string"}}},
            {"type":"object","required":["kind","tag"],"additionalProperties":false,"properties":{"kind":{"const":"tag","type":"string"},"tag":{"type":"string"}}}
        ]})
    }

    #[test]
    fn rewrites_referenced_tagged_variants_without_losing_constraints() {
        let mut schema = schemars::json_schema!({
            "type":"object", "$defs":{"Source":variants()},
            "properties":{"source":{"anyOf":[{"$ref":"#/$defs/Source"},{"type":"null"}]}}
        });
        let mut expected = schema.clone().to_value();
        expected["$defs"]["Source"] = json!({"anyOf":variants()["oneOf"]});
        tagged_unions(&mut schema);
        assert_eq!(schema.to_value(), expected);
    }

    #[test]
    fn overlapping_optional_or_non_object_tags_are_not_rewritten() {
        for (pointer, value) in [
            ("/oneOf/1/properties/kind/const", json!("pr")),
            ("/oneOf/1/required", json!(["tag"])),
            ("/oneOf/1/type", json!("string")),
            ("/oneOf/1/properties/kind", json!({"type":"string"})),
        ] {
            let mut original = variants();
            *original.pointer_mut(pointer).unwrap() = value;
            let mut schema = Schema::try_from(original.clone()).unwrap();
            tagged_unions(&mut schema);
            assert_eq!(schema.to_value(), original);
        }
    }

    #[test]
    fn preserves_sibling_unions_and_literal_schema_looking_data() {
        let mut with_sibling = variants();
        with_sibling["anyOf"] = json!([{"required":["url"]}]);
        let original = json!({
            "type":"object", "$defs":{"Both":with_sibling},
            "properties":{"oneOf":{"type":"string"},"payload":{"type":"object","default":variants(),"examples":[variants()]}}
        });
        let mut schema = Schema::try_from(original.clone()).unwrap();
        tagged_unions(&mut schema);
        assert_eq!(schema.to_value(), original);
    }
}
