//! Actionable diagnostics. Strict request parsing and validation remain the admission gates.
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
mod schema;
#[cfg(test)]
mod tests;

/// Wrap the existing parser so its structured diagnosis reaches our error formatter
/// before rmcp reduces deserialization failures to text-only tool results.
pub(super) fn wrap(
    route: &mut rmcp::handler::server::router::tool::ToolRoute<crate::ProofstormMcp>,
) {
    let call = route.call.clone();
    let schema = std::sync::Arc::new(Value::Object((*route.attr.input_schema).clone()));
    let cell_inputs = matches!(route.attr.name.as_ref(), "cell_plan" | "cell_up");
    route.call = std::sync::Arc::new(move |context| {
        // Avoid retaining arbitrarily large inputs alongside the parser. Diagnostic
        // traversal is independently bounded and never echoes submitted values.
        let input = context
            .arguments
            .as_ref()
            .map_or_else(|| Some(json!({})), snapshot);
        let schema = schema.clone();
        let future = call(context);
        Box::pin(async move {
            future
                .await
                .map_err(|error| explain(error, input.as_ref(), &schema, cell_inputs))
        })
    });
}

fn snapshot(arguments: &serde_json::Map<String, Value>) -> Option<Value> {
    fn size(value: &Value, nodes: &mut usize, bytes: &mut usize, depth: usize) -> Option<()> {
        if depth > 32 {
            return None;
        }
        *nodes = nodes.checked_sub(1)?;
        match value {
            Value::String(text) => {
                *bytes = bytes.checked_sub(text.len())?;
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    *bytes = bytes.checked_sub(key.len())?;
                    size(value, nodes, bytes, depth + 1)?;
                }
            }
            Value::Array(items) => {
                for value in items {
                    size(value, nodes, bytes, depth + 1)?;
                }
            }
            _ => (),
        }
        Some(())
    }
    let mut nodes = 4096;
    let mut bytes: usize = 128 * 1024;
    for (key, value) in arguments {
        bytes = bytes.checked_sub(key.len())?;
        size(value, &mut nodes, &mut bytes, 0)?;
    }
    Some(Value::Object(arguments.clone()))
}

fn explain(
    error: ErrorData,
    input: Option<&Value>,
    schema: &Value,
    cell_inputs: bool,
) -> ErrorData {
    if error.code != rmcp::model::ErrorCode::INVALID_PARAMS
        || error.data.is_some()
        || !error
            .message
            .starts_with("failed to deserialize parameters:")
    {
        return error;
    }
    let diagnostics = input.map(|value| schema::describe(value, schema, cell_inputs));
    let message = if diagnostics
        .as_ref()
        .is_none_or(|result| result.issues.is_empty())
    {
        "Tool arguments could not be parsed. Diagnostics are incomplete; inspect this tool's input schema and documented input forms. No action was accepted."
    } else {
        "Tool arguments could not be parsed. Correct the reported fields and retry; no action was accepted."
    };
    // The parser error can contain caller secrets (for example an invalid enum
    // value). Report schema facts instead, with an explicit partial-result marker.
    ErrorData::invalid_params(
        message,
        Some(json!({
            "code":"tool_input_invalid", "executed":false,
            "issues":diagnostics.as_ref().map_or(&[][..], |result| result.issues.as_slice()),
            "details_may_be_omitted":diagnostics.as_ref().is_none_or(|result| result.incomplete || result.issues.is_empty()),
        })),
    )
}

#[derive(Serialize)]
pub(super) struct Issue {
    pub path: String,
    pub code: &'static str,
    pub expected: Value,
    pub example: Value,
}

impl Issue {
    pub fn range(path: &str, minimum: usize, maximum: usize, example: usize) -> Self {
        Self {
            path: path.into(),
            code: "out_of_range",
            expected: json!({"minimum":minimum,"maximum":maximum}),
            example: json!(example),
        }
    }

    /// Use the same bounds advertised to clients, rather than another list of limits.
    pub fn schema_range<T: JsonSchema>(field: &str, example: usize) -> Self {
        let schema = schemars::schema_for!(T).to_value();
        let field_schema = &schema["properties"][field];
        Self {
            path: format!("/{field}"),
            code: "out_of_range",
            expected: json!({"minimum":field_schema["minimum"],"maximum":field_schema["maximum"]}),
            example: json!(example),
        }
    }
}

pub(super) fn invalid(code: &str, message: &str, issues: &[Issue]) -> ErrorData {
    ErrorData::invalid_request(
        message.to_owned(),
        Some(json!({"code":code,"issues":issues})),
    )
}
