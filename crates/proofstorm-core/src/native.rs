//! Native execution contracts. Raw output is private unless explicitly requested.
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Private,
    Public,
    JsonFields,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeOutput {
    #[serde(default)]
    pub mode: OutputMode,
    /// Top-level, allowlisted receipt fields from the last JSON document.
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommand {
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub argv: Vec<String>,
    pub timeout_seconds: u32,
    #[serde(default)]
    pub output: NativeOutput,
}

const STATUS_FIELDS: &[&str] = &["status", "state", "failure_reason"];
const BOOLEAN_FIELDS: &[&str] = &["settled", "synced_to_chain"];
const NUMBER_FIELDS: &[&str] = &[
    "amount",
    "amount_sat",
    "fee_paid",
    "fee_paid_sat",
    "value_sat",
    "total_fees",
    "total_fees_msat",
    "num_active_channels",
    "balance",
    "confirmed_balance",
    "unconfirmed_balance",
];

impl NativeCommand {
    /// Validate before creating an action or starting a process.
    ///
    /// # Errors
    /// Returns a static diagnostic for an invalid execution contract.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.script.is_empty() == self.argv.is_empty() {
            return Err("provide exactly one of script or argv");
        }
        if self.script.len() + self.argv.iter().map(String::len).sum::<usize>() > 16 * 1024
            || self.argv.len() > 64
            || self.argv.iter().any(|arg| arg.contains('\0'))
            || self.script.contains('\0')
        {
            return Err("native command exceeds argument limits or contains NUL");
        }
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err("timeout_seconds must be in 1..=300");
        }
        if self.output.mode == OutputMode::JsonFields {
            if self.output.fields.is_empty() || self.output.fields.len() > 16 {
                return Err("json_fields requires 1..=16 receipt fields");
            }
            if self.output.fields.iter().any(|field| {
                !STATUS_FIELDS.contains(&field.as_str())
                    && !BOOLEAN_FIELDS.contains(&field.as_str())
                    && !NUMBER_FIELDS.contains(&field.as_str())
            }) {
                return Err("output field is not an allowlisted receipt field");
            }
        } else if !self.output.fields.is_empty() {
            return Err("fields requires json_fields output mode");
        }
        Ok(())
    }
}

/// Parse a complete JSON stream and project only typed receipt values.
///
/// # Errors
/// Never includes the raw input or parser error in the diagnostic.
pub fn project_receipt(bytes: &[u8], fields: &[String]) -> Result<Value, &'static str> {
    let mut last = None;
    for item in serde_json::Deserializer::from_slice(bytes).into_iter::<Value>() {
        last = Some(item.map_err(|_| "output_format_invalid")?);
    }
    let value = last.ok_or("output_format_invalid")?;
    let mut selected = BTreeMap::new();
    for field in fields {
        let item = value.get(field).ok_or("output_field_missing")?;
        let safe = if STATUS_FIELDS.contains(&field.as_str()) {
            item.as_str().is_some_and(|state| {
                matches!(
                    state,
                    "PAID"
                        | "UNPAID"
                        | "PENDING"
                        | "ISSUED"
                        | "SETTLED"
                        | "SUCCEEDED"
                        | "FAILED"
                        | "IN_FLIGHT"
                        | "OPEN"
                        | "ACCEPTED"
                        | "CANCELED"
                        | "FAILURE_REASON_NONE"
                        | "FAILURE_REASON_TIMEOUT"
                        | "FAILURE_REASON_NO_ROUTE"
                        | "FAILURE_REASON_ERROR"
                        | "FAILURE_REASON_INCORRECT_PAYMENT_DETAILS"
                        | "FAILURE_REASON_INSUFFICIENT_BALANCE"
                        | "FAILURE_REASON_CANCELED"
                )
            })
        } else if BOOLEAN_FIELDS.contains(&field.as_str()) {
            item.is_boolean()
        } else if NUMBER_FIELDS.contains(&field.as_str()) {
            item.as_u64().is_some()
                || item
                    .as_str()
                    .is_some_and(|text| text.parse::<u64>().is_ok())
        } else {
            false
        };
        if !safe {
            return Err("output_field_type_invalid");
        }
        selected.insert(field.clone(), item.clone());
    }
    Ok(serde_json::json!(selected))
}

/// Bound encoded public streams before persisting controller status or journal
/// artifacts. Private retention and process/cleanup evidence remain unchanged.
#[must_use]
pub fn cap_public_streams(mut receipt: serde_json::Value) -> serde_json::Value {
    for field in ["stdout", "stderr"] {
        let Some(text) = receipt.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let mut encoded = 2;
        let mut retained = 0;
        for character in text.chars() {
            let cost = match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
                '\u{0000}'..='\u{001f}' => 6,
                _ => character.len_utf8(),
            };
            if encoded + cost > 12 * 1024 {
                break;
            }
            encoded += cost;
            retained += character.len_utf8();
        }
        if retained < text.len() {
            let bounded = text[..retained].to_owned();
            receipt[field] = serde_json::Value::String(bounded);
            receipt["output_truncated"] = serde_json::json!(true);
        }
    }
    receipt
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projections_fail_closed_and_never_return_preimages() {
        let fields = vec!["status".into(), "value_sat".into()];
        let input = br#"{"status":"IN_FLIGHT"} {"status":"SUCCEEDED","value_sat":"700","payment_preimage":"private-canary"}"#;
        assert_eq!(
            project_receipt(input, &fields).unwrap(),
            serde_json::json!({"status":"SUCCEEDED","value_sat":"700"})
        );
        for input in [
            b"PREIMAGE private-canary".as_slice(),
            br#"{"status":"private-canary","value_sat":"700"}"#,
            br#"{"status":{"secret":"private-canary"},"value_sat":700}"#,
        ] {
            assert!(project_receipt(input, &fields).is_err());
        }
        assert!(project_receipt(input, &["payment_preimage".into()]).is_err());
    }
}
