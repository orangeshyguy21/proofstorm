//! Provider-portable input schemas without changing request validation.
use std::{collections::BTreeSet, sync::Arc};

use schemars::{Schema, transform::transform_subschemas};
use serde_json::{Map, Value};

pub(crate) fn portable_input(input: &Map<String, Value>) -> Arc<Map<String, Value>> {
    let mut schema = Schema::from(input.clone());
    tagged_unions(&mut schema);
    if let Some(expanded) = inline_local_references(&schema) {
        schema = expanded;
    }
    let root = schema.clone().to_value();
    reference_types(&mut schema, &root);
    let Value::Object(object) = schema.to_value() else {
        unreachable!("an object schema stays an object")
    };
    Arc::new(object)
}

const MAX_EXPANSION_BYTES: usize = 256 * 1024;
const MAX_EXPANSION_NODES: usize = 4096;

fn inline_local_references(schema: &Schema) -> Option<Schema> {
    // Schemars generates draft 2020-12 by default. Other dialects can introduce schema
    // locations or reference semantics this visitor does not understand.
    if schema
        .get("$schema")
        .is_some_and(|dialect| dialect != "https://json-schema.org/draft/2020-12/schema")
    {
        return None;
    }
    // Work transactionally: unsupported references or an expansion limit keep
    // the original graph intact, including its definitions. No partial cycles
    // are unrolled on each successive discovery call.
    let original_size = serde_json::to_vec(schema).ok()?.len();
    let mut bytes = MAX_EXPANSION_BYTES.checked_sub(original_size)?;
    let mut nodes = MAX_EXPANSION_NODES;
    let mut expanded = schema.clone();
    expand_schema(
        &mut expanded,
        schema.as_value(),
        &mut BTreeSet::new(),
        &mut bytes,
        &mut nodes,
        0,
    )?;
    // All schema references have been expanded, and resource/anchor scopes
    // were refused, so the root definitions are now unused.
    expanded.as_object_mut()?.remove("$defs");
    // Reused manifest definitions can grow substantially when duplicated.
    // Keep their compact graph rather than spending the discovery budget on
    // repeated schemas. The same size rule applies to every tool and model.
    if serde_json::to_vec(&expanded).ok()?.len() > original_size {
        return None;
    }
    Some(expanded)
}

fn expand_schema(
    schema: &mut Schema,
    root: &Value,
    active: &mut BTreeSet<String>,
    bytes: &mut usize,
    nodes: &mut usize,
    depth: usize,
) -> Option<()> {
    *nodes = nodes.checked_sub(1)?;
    if depth >= 64
        || (depth > 0 && schema.get("$schema").is_some())
        || [
            "$id",
            "$anchor",
            "$dynamicAnchor",
            "$dynamicRef",
            "$recursiveAnchor",
            "$recursiveRef",
            "$vocabulary",
        ]
        .iter()
        .any(|key| schema.get(*key).is_some())
    {
        return None;
    }
    if let Some(reference) = schema.get("$ref") {
        let reference = reference.as_str()?.to_owned();
        let name = reference.strip_prefix("#/$defs/")?;
        if name.contains('/') || active.len() >= 32 || !active.insert(reference.clone()) {
            return None;
        }
        let siblings = schema.as_object()?;
        // Only annotations can override definition annotations. Assertion
        // siblings must remain a conjunction; merging them can drop constraints
        // or change additional/unevaluatedProperties semantics.
        if siblings.keys().any(|key| {
            !matches!(
                key.as_str(),
                "$ref"
                    | "title"
                    | "description"
                    | "default"
                    | "examples"
                    | "deprecated"
                    | "readOnly"
                    | "writeOnly"
                    | "$comment"
            )
        }) {
            return None;
        }
        let target = root.pointer(reference.strip_prefix('#')?)?.as_object()?;
        *bytes = bytes.checked_sub(serde_json::to_vec(target).ok()?.len())?;
        let mut target = Schema::from(target.clone());
        for (key, value) in siblings.iter().filter(|(key, _)| key.as_str() != "$ref") {
            target.insert(key.clone(), value.clone());
        }
        expand_schema(&mut target, root, active, bytes, nodes, depth + 1)?;
        active.remove(&reference);
        *schema = target;
        return Some(());
    }
    let mut result = Some(());
    let mut visit = |child: &mut Schema| {
        if result.is_some() {
            result = expand_schema(child, root, active, bytes, nodes, depth + 1);
        }
    };
    transform_subschemas(&mut visit, schema);
    // Schemars' visitor omits these schema-valued keywords. Visit them as well
    // before removing definitions; never traverse defaults or examples as code.
    if let Some(object) = schema.as_object_mut() {
        for key in ["unevaluatedProperties", "unevaluatedItems", "contentSchema"] {
            if let Some(value) = object.get_mut(key)
                && let Ok(child) = value.try_into()
            {
                visit(child);
            }
        }
        for key in ["dependentSchemas", "dependencies"] {
            if let Some(Value::Object(children)) = object.get_mut(key) {
                for value in children.values_mut() {
                    if let Ok(child) = value.try_into() {
                        visit(child);
                    }
                }
            }
        }
    }
    result
}

fn reference_types(schema: &mut Schema, root: &Value) {
    if schema.get("$id").is_some() {
        return;
    }
    // Ollama's Qwen XML parser converts arguments using the direct property
    // type, without following $ref. Expose only types already required by the
    // referenced schema; preserve the reference and all validation constraints.
    // Do not hoist a union's types: Moonshot expands refs and rejects a type
    // beside anyOf. Its branches already carry their individual types.
    if schema.get("type").is_none()
        && schema.get("anyOf").is_none()
        && schema.get("oneOf").is_none()
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
    if node.get("$id").is_some() || node.get("anyOf").is_some() || node.get("oneOf").is_some() {
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
    None
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

    fn with_reference_types(input: &Value) -> Value {
        let mut schema = Schema::try_from(input.clone()).unwrap();
        tagged_unions(&mut schema);
        let root = schema.clone().to_value();
        reference_types(&mut schema, &root);
        schema.to_value()
    }

    #[test]
    fn inlines_nested_objects_arrays_and_enums_without_changing_constraints() {
        let input = json!({"type":"object","$defs":{
            "Mode":{"type":"string","enum":["private","public"]},
            "Output":{"type":"object","description":"Definition", "additionalProperties":false,
                "required":["mode"],"properties":{
                    "mode":{"$ref":"#/$defs/Mode"},
                    "fields":{"type":"array","items":{"type":"string"},"maxItems":16}}}
        },"required":["output"],"properties":{
            "output":{"$ref":"#/$defs/Output","description":"Use-site", "default":{"$ref":"literal"}},
            "$ref":{"type":"string","examples":[{"$ref":"literal"}]}
        }});
        let expected = json!({"type":"object","required":["output"],"properties":{
            "output":{"type":"object","description":"Use-site","additionalProperties":false,
                "required":["mode"],"default":{"$ref":"literal"},"properties":{
                    "mode":{"type":"string","enum":["private","public"]},
                    "fields":{"type":"array","items":{"type":"string"},"maxItems":16}}},
            "$ref":{"type":"string","examples":[{"$ref":"literal"}]}
        }});
        let result = portable_input(input.as_object().unwrap());
        assert_eq!(json!(result), expected);
        assert_eq!(portable_input(&result), result);
    }

    #[test]
    fn inlines_nullable_tagged_unions_and_aliases_without_hoisting_types() {
        let input = json!({"type":"object","$defs":{
            "Payload":variants(),"Alias":{"$ref":"#/$defs/Payload"}
        },"properties":{"payload":{"anyOf":[{"$ref":"#/$defs/Alias"},{"type":"null"}],"default":null}}});
        let expected = json!({"type":"object","properties":{"payload":{
            "anyOf":[{"anyOf":variants()["oneOf"]},{"type":"null"}],"default":null
        }}});
        let result = portable_input(input.as_object().unwrap());
        assert_eq!(json!(result), expected);
        assert_eq!(portable_input(&result), result);
    }

    #[test]
    fn unsupported_expansions_keep_the_entire_reference_graph() {
        for bad in [
            json!({"$ref":"#/$defs/Missing"}),
            json!({"$ref":"https://example.test/schema"}),
            json!({"$ref":"#anchor"}),
            json!({"$ref":"#/$defs/Object","required":["extra"]}),
            json!({"$ref":"#/$defs/Object","type":"string"}),
            json!({"$ref":"#/$defs/Object","anyOf":[{"required":["a"]}]}),
            json!({"$ref":"#/$defs/A"}),
            json!({"$id":"https://example.test/scoped"}),
            json!({"$anchor":"anchor"}),
            json!({"$dynamicRef":"#anchor"}),
        ] {
            let input = json!({"type":"object","$defs":{
                "Object":{"type":"object","additionalProperties":false},
                "A":{"$ref":"#/$defs/B"},"B":{"$ref":"#/$defs/A"}
            },"properties":{"good":{"$ref":"#/$defs/Object"},"bad":bad}});
            // Remove the unused cycle except for the explicit cycle case.
            let mut input = input;
            if input["properties"]["bad"]["$ref"] != "#/$defs/A" {
                input["$defs"].as_object_mut().unwrap().remove("A");
                input["$defs"].as_object_mut().unwrap().remove("B");
            }
            let schema = Schema::try_from(input.clone()).unwrap();
            assert!(inline_local_references(&schema).is_none(), "{input}");
            assert_eq!(schema.to_value(), input);
            let portable = portable_input(input.as_object().unwrap());
            assert!(portable.get("$defs").is_some());
            assert_eq!(portable_input(&portable), portable);
        }
    }

    #[test]
    fn expands_all_schema_locations_without_rewriting_literal_data() {
        let input = json!({"$defs":{"S":{"type":"string"}},
            "dependentSchemas":{"field":{"$ref":"#/$defs/S"}},
            "dependencies":{"old":{"$ref":"#/$defs/S"},"names":["a"]},
            "unevaluatedProperties":{"$ref":"#/$defs/S"},
            "unevaluatedItems":{"$ref":"#/$defs/S"},
            "contentSchema":{"$ref":"#/$defs/S"},
            "default":{"$ref":"literal"},"examples":[{"$id":"literal"}]
        });
        let mut expected = input.clone();
        expected.as_object_mut().unwrap().remove("$defs");
        for pointer in [
            "/dependentSchemas/field",
            "/dependencies/old",
            "/unevaluatedProperties",
            "/unevaluatedItems",
            "/contentSchema",
        ] {
            *expected.pointer_mut(pointer).unwrap() = json!({"type":"string"});
        }
        assert_eq!(json!(portable_input(input.as_object().unwrap())), expected);
    }

    #[test]
    fn expansion_limits_do_not_leave_partial_results() {
        let mut definitions = Map::new();
        definitions.insert("Leaf".into(), json!({"type":"string"}));
        for i in 0..16 {
            let target = if i == 0 {
                "Leaf".into()
            } else {
                format!("N{}", i - 1)
            };
            definitions.insert(
                format!("N{i}"),
                json!({"type":"object","properties":{
                    "left":{"$ref":format!("#/$defs/{target}")},
                    "right":{"$ref":format!("#/$defs/{target}")}
                }}),
            );
        }
        let schema = Schema::try_from(
            json!({"$defs":definitions,"properties":{"tree":{"$ref":"#/$defs/N15"}}}),
        )
        .unwrap();
        assert!(inline_local_references(&schema).is_none());
        let deep = (0..65).fold(json!({"type":"string"}), |s, _| json!({"items":s}));
        assert!(inline_local_references(&Schema::try_from(deep).unwrap()).is_none());
        let huge = json!({"description":"x".repeat(MAX_EXPANSION_BYTES)});
        assert!(inline_local_references(&Schema::try_from(huge).unwrap()).is_none());
    }

    #[test]
    fn reused_definitions_remain_compact_when_expansion_would_grow() {
        let input = json!({"type":"object","$defs":{"Shared":{
            "type":"object","additionalProperties":false,"required":["value"],
            "description":"x".repeat(1024),"properties":{"value":{"type":"string","minLength":1}}
        }},"properties":{"left":{"$ref":"#/$defs/Shared"},"right":{"$ref":"#/$defs/Shared"}}});
        assert!(inline_local_references(&Schema::try_from(input.clone()).unwrap()).is_none());
        let result = portable_input(input.as_object().unwrap());
        assert_eq!(json!(result), with_reference_types(&input));
        assert_eq!(portable_input(&result), result);
    }

    #[test]
    fn other_dialects_and_nested_dialect_declarations_are_not_expanded() {
        let old = Schema::try_from(json!({
            "$schema":"http://json-schema.org/draft-03/schema#",
            "$defs":{"S":{"type":"string"}},"extends":{"$ref":"#/$defs/S"}
        }))
        .unwrap();
        assert!(inline_local_references(&old).is_none());
        let nested = Schema::try_from(json!({"properties":{"value":{
            "$schema":"https://json-schema.org/draft/2020-12/schema","type":"string"
        }}}))
        .unwrap();
        assert!(inline_local_references(&nested).is_none());
        let current = json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
            "$defs":{"S":{"type":"string"}},"properties":{"value":{"$ref":"#/$defs/S"}}});
        let expanded =
            inline_local_references(&Schema::try_from(current.clone()).unwrap()).unwrap();
        assert_eq!(
            expanded.as_value()["properties"]["value"],
            json!({"type":"string"})
        );
        assert_eq!(expanded.as_value()["$schema"], current["$schema"]);
    }

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
        expected["$defs"]["Output"]["properties"]["mode"]["type"] = json!("string");
        expected["$defs"]["Nullable"]["anyOf"][0]["type"] = json!("object");
        assert_eq!(with_reference_types(&input), expected);
    }

    #[test]
    fn union_references_keep_branch_types_without_hoisting_through_aliases() {
        let input = json!({"type":"object","$defs":{
            "Payload":variants(),
            "Alias":{"$ref":"#/$defs/Payload"},
            "Object":{"type":"object","additionalProperties":false}
        },"properties":{
            "private_payload":{"anyOf":[{"$ref":"#/$defs/Alias"},{"type":"null"}],"default":null},
            "sibling_union":{"$ref":"#/$defs/Object","anyOf":[{"required":["a"]},{"required":["b"]}]},
            "output":{"$ref":"#/$defs/Object"}
        }});
        let mut expected = input.clone();
        expected["$defs"]["Payload"] = json!({"anyOf":variants()["oneOf"]});
        expected["properties"]["output"]["type"] = json!("object");
        assert_eq!(with_reference_types(&input), expected);
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
