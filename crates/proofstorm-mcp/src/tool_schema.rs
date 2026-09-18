//! Provider-portable input schemas without changing request validation.
use std::{collections::BTreeSet, sync::Arc};

use schemars::{Schema, transform::transform_subschemas};
use serde_json::{Map, Value};

pub(crate) fn portable_input(input: &Map<String, Value>) -> Arc<Map<String, Value>> {
    let mut schema = Schema::from(input.clone());
    tagged_unions(&mut schema);
    let Value::Object(object) = schema.to_value() else {
        unreachable!("an object schema stays an object")
    };
    Arc::new(object)
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
