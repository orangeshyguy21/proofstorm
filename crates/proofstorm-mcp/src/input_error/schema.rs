//! Conservative schema diagnostics after serde rejects a request, never an admission gate.
use serde_json::{Value, json};

const MAX_ISSUES: usize = 16;
const MAX_STEPS: usize = 2048;
const MAX_BYTES: usize = 6 * 1024;

#[derive(Default)]
pub(super) struct Diagnostics {
    pub issues: Vec<Value>,
    pub incomplete: bool,
    steps: usize,
    bytes: usize,
    cell_inputs: bool,
}

pub(super) fn describe(value: &Value, schema: &Value, cell_inputs: bool) -> Diagnostics {
    let mut result = Diagnostics {
        cell_inputs,
        ..Diagnostics::default()
    };
    result.walk(value, schema, schema, "", 0);
    result
}

impl Diagnostics {
    fn issue(
        &mut self,
        path: &str,
        code: &str,
        expected: Value,
        example: Option<Value>,
        suggestion: Option<String>,
    ) {
        if self.issues.len() == MAX_ISSUES || path.len() > 512 {
            self.incomplete = true;
            return;
        }
        let mut issue = json!({"path":path,"code":code});
        issue["expected"] = expected;
        let example = if matches!(code, "invalid_value" | "invalid_variant") {
            suggestion.as_ref().map(|value| json!(value)).or(example)
        } else {
            example
        };
        if let Some(example) = example {
            issue["example"] = example;
        }
        if let Some(suggestion) = suggestion {
            issue["did_you_mean"] = json!(suggestion);
        }
        let size = issue.to_string().len();
        if self.bytes + size > MAX_BYTES {
            self.incomplete = true;
            return;
        }
        self.bytes += size;
        self.issues.push(issue);
    }

    fn walk(&mut self, value: &Value, schema: &Value, root: &Value, path: &str, depth: usize) {
        if self.steps == MAX_STEPS || depth > 32 || self.issues.len() == MAX_ISSUES {
            self.incomplete = true;
            return;
        }
        self.steps += 1;
        // CellInput deliberately accepts canonical links and JSON-encoded cells in
        // addition to its authored schema. Use the same selection rule as parsing.
        if self.cell_inputs && schema["$ref"] == "#/$defs/CellInput" {
            match crate::cell_input::diagnostics::input(value) {
                Some((value, schema)) => self.walk(&value, schema, schema, path, depth + 1),
                None => self.issue(
                    path,
                    "invalid_json",
                    json!({"type":"cell JSON object or JSON-encoded object"}),
                    None,
                    None,
                ),
            }
            return;
        }
        if let Some(reference) = schema["$ref"].as_str() {
            if let Some(target) = reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
            {
                self.walk(value, target, root, path, depth + 1);
            } else {
                self.incomplete = true;
            }
            return;
        }
        for keyword in ["oneOf", "anyOf"] {
            if let Some(branches) = schema[keyword].as_array() {
                self.union(value, branches, root, path, depth);
                return;
            }
        }
        if !matches_type(value, schema) {
            self.issue(path, "invalid_type", summary(schema), example(schema), None);
            return;
        }
        if value.is_number()
            && (schema
                .get("minimum")
                .is_some_and(|bound| compare(value, bound) == Some(std::cmp::Ordering::Less))
                || schema.get("maximum").is_some_and(|bound| {
                    compare(value, bound) == Some(std::cmp::Ordering::Greater)
                }))
        {
            self.issue(path, "out_of_range", summary(schema), example(schema), None);
        }
        if ["allOf", "not", "if", "patternProperties"]
            .iter()
            .any(|key| schema.get(key).is_some())
        {
            self.incomplete = true;
            return;
        }
        if let Some(allowed) = choices(schema) {
            if !allowed.contains(value) {
                self.issue(
                    path,
                    "invalid_value",
                    json!({"allowed_values":allowed}),
                    allowed.first().cloned(),
                    value
                        .as_str()
                        .and_then(|actual| suggestion(actual, &allowed)),
                );
            }
            return;
        }
        if let Some(object) = value.as_object() {
            self.object(object, schema, root, path, depth);
        } else if let Some(items) = value.as_array() {
            for (index, item) in items.iter().enumerate() {
                if self.steps == MAX_STEPS || self.issues.len() == MAX_ISSUES {
                    self.incomplete = true;
                    break;
                }
                self.walk(
                    item,
                    &schema["items"],
                    root,
                    &format!("{path}/{index}"),
                    depth + 1,
                );
            }
        }
    }

    fn object(
        &mut self,
        object: &serde_json::Map<String, Value>,
        schema: &Value,
        root: &Value,
        path: &str,
        depth: usize,
    ) {
        let properties = schema["properties"].as_object();
        if let Some(required) = schema["required"].as_array() {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    let field = resolve(&schema["properties"][key], root);
                    self.issue(
                        &pointer(path, key),
                        "missing_field",
                        summary(field),
                        example(field),
                        None,
                    );
                }
            }
        }
        let allowed: Vec<_> = properties
            .into_iter()
            .flat_map(|fields| fields.keys())
            .take(32)
            .map(|key| json!(key))
            .collect();
        for (key, child) in object {
            if self.steps == MAX_STEPS || self.issues.len() == MAX_ISSUES {
                self.incomplete = true;
                break;
            }
            let path = pointer(path, key);
            if let Some(field) = properties.and_then(|fields| fields.get(key)) {
                self.walk(child, field, root, &path, depth + 1);
            } else if schema["additionalProperties"] == false {
                let omitted = properties.is_some_and(|fields| fields.len() > 32);
                self.incomplete |= omitted;
                self.issue(&path, "unknown_field", json!({"allowed_fields":allowed,"fields_omitted":omitted,"correction":"remove or rename this field"}), None, suggestion(key, &allowed));
            } else if schema["additionalProperties"].is_object() {
                self.walk(
                    child,
                    &schema["additionalProperties"],
                    root,
                    &path,
                    depth + 1,
                );
            }
        }
    }

    fn union(&mut self, value: &Value, branches: &[Value], root: &Value, path: &str, depth: usize) {
        let matching: Vec<_> = branches
            .iter()
            .filter(|branch| compatible(value, branch, root, 0))
            .collect();
        if matching.len() == 1 {
            self.walk(value, matching[0], root, path, depth + 1);
            return;
        }
        let resolved: Vec<_> = matching
            .iter()
            .map(|branch| resolve(branch, root))
            .collect();
        // A common required discriminator is decisive; never merge requirements
        // from variants that were not selected. Tags may be kind, op, action, etc.
        if let Some(properties) = resolved
            .first()
            .and_then(|schema| schema["properties"].as_object())
        {
            for tag in properties.keys() {
                let choices: Option<Vec<_>> = resolved
                    .iter()
                    .map(|branch| {
                        let required = branch["required"].as_array()?;
                        if !required.contains(&json!(tag)) {
                            return None;
                        }
                        let choices = choices(resolve(&branch["properties"][tag], root))?;
                        (choices.len() == 1).then(|| choices[0].clone())
                    })
                    .collect();
                if let Some(choices) = choices {
                    if choices
                        .iter()
                        .enumerate()
                        .any(|(i, choice)| choices[..i].contains(choice))
                    {
                        continue;
                    }
                    if let Some(index) = value
                        .get(tag)
                        .and_then(|value| choices.iter().position(|choice| choice == value))
                    {
                        self.walk(value, matching[index], root, path, depth + 1);
                    } else {
                        self.issue(
                            &pointer(path, tag),
                            if value.get(tag).is_some() {
                                "invalid_variant"
                            } else {
                                "missing_field"
                            },
                            json!({"allowed_values":choices}),
                            choices.first().cloned(),
                            value[tag]
                                .as_str()
                                .and_then(|actual| suggestion(actual, &choices)),
                        );
                    }
                    return;
                }
            }
        }
        // For untagged object forms, only select by a property that other closed
        // branches cannot accept. An empty or ambiguous object gets no guessed fields.
        if let Some(object) = value.as_object() {
            let selected: Vec<_> = resolved
                .iter()
                .enumerate()
                .filter(|(index, branch)| {
                    object.keys().any(|key| {
                        branch["properties"].get(key).is_some()
                            && resolved.iter().enumerate().all(|(other, schema)| {
                                other == *index
                                    || (schema["additionalProperties"] == false
                                        && schema["properties"].get(key).is_none())
                            })
                    })
                })
                .collect();
            if selected.len() == 1 {
                self.walk(value, matching[selected[0].0], root, path, depth + 1);
                return;
            }
        }
        self.incomplete = true;
        if matching.is_empty() {
            self.issue(path, "invalid_type", json!({"alternatives":branches.iter().take(8).map(|branch| summary(resolve(branch, root))).collect::<Vec<_>>()}), None, None);
        }
    }
}

fn resolve<'a>(mut schema: &'a Value, root: &'a Value) -> &'a Value {
    for _ in 0..32 {
        let Some(target) = schema["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix('#'))
            .and_then(|pointer| root.pointer(pointer))
        else {
            break;
        };
        schema = target;
    }
    schema
}

fn matches_type(value: &Value, schema: &Value) -> bool {
    let matches = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    };
    match &schema["type"] {
        Value::String(kind) => matches(kind),
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).any(matches),
        _ => true,
    }
}

fn compatible(value: &Value, schema: &Value, root: &Value, depth: usize) -> bool {
    if depth > 16 {
        return true;
    }
    let schema = resolve(schema, root);
    for keyword in ["anyOf", "oneOf"] {
        if let Some(branches) = schema[keyword].as_array() {
            return branches
                .iter()
                .any(|branch| compatible(value, branch, root, depth + 1));
        }
    }
    matches_type(value, schema)
}

fn choices(schema: &Value) -> Option<Vec<Value>> {
    if let Some(value) = schema.get("const") {
        return Some(vec![value.clone()]);
    }
    schema["enum"].as_array().cloned()
}

fn compare(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    let integer = |value: &Value| {
        value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
    };
    if let (Some(left), Some(right)) = (integer(left), integer(right)) {
        return Some(left.cmp(&right));
    }
    left.as_f64()?.partial_cmp(&right.as_f64()?)
}

fn summary(schema: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in [
        "type",
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "required",
    ] {
        if let Some(value) = schema.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    if let Some(allowed) = choices(schema) {
        result.insert("allowed_values".into(), json!(allowed));
    }
    Value::Object(result)
}

fn example(schema: &Value) -> Option<Value> {
    choices(schema)
        .and_then(|values| values.into_iter().next())
        .or_else(|| schema.get("default").cloned())
        .or_else(|| match schema["type"].as_str()? {
            "string"
                if ["minLength", "maxLength", "pattern", "format"]
                    .iter()
                    .all(|key| schema.get(key).is_none()) =>
            {
                Some(json!("example"))
            }
            "boolean" => Some(json!(false)),
            "null" => Some(Value::Null),
            "integer" | "number" => Some(schema.get("minimum").cloned().unwrap_or(json!(0))),
            "array" if schema["minItems"].as_u64().unwrap_or(0) == 0 => Some(json!([])),
            _ => None,
        })
}

fn pointer(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn suggestion(actual: &str, allowed: &[Value]) -> Option<String> {
    if actual.len() > 64 || !actual.is_ascii() {
        return None;
    }
    let mut candidates: Vec<_> = allowed
        .iter()
        .filter_map(Value::as_str)
        .filter(|candidate| candidate.len() <= 64 && candidate.is_ascii())
        .map(|candidate| (distance(actual.as_bytes(), candidate.as_bytes()), candidate))
        .filter(|(distance, _)| *distance <= if actual.len() > 5 { 2 } else { 1 })
        .collect();
    candidates.sort_unstable();
    let best = candidates.first()?;
    (candidates.get(1).is_none_or(|next| next.0 != best.0)).then(|| best.1.to_owned())
}

// Adjacent transpositions count as one edit; lengths are bounded before allocation.
fn distance(left: &[u8], right: &[u8]) -> usize {
    let mut costs = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (i, row) in costs.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, value) in costs[0].iter_mut().enumerate() {
        *value = j;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            costs[i][j] = (costs[i - 1][j] + 1).min(costs[i][j - 1] + 1).min(
                costs[i - 1][j - 1] + usize::from(!left[i - 1].eq_ignore_ascii_case(&right[j - 1])),
            );
            if i > 1
                && j > 1
                && left[i - 1].eq_ignore_ascii_case(&right[j - 2])
                && left[i - 2].eq_ignore_ascii_case(&right[j - 1])
            {
                costs[i][j] = costs[i][j].min(costs[i - 2][j - 2] + 1);
            }
        }
    }
    costs[left.len()][right.len()]
}
