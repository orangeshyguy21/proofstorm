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
    // One complete BOLT11 string (for example, cocod receive bolt11).
    Bolt11,
    // One LND addinvoice JSON object; payment_request and hex r_hash must agree.
    LndInvoice,
}

/// Extract one bounded invoice. Raw responses and parser diagnostics stay private.
/// This validates invoice syntax/signature and hash linkage, not settlement,
/// expiry at payment time, routing, or the intended amount/network.
///
/// # Errors
/// Returns only static diagnostics; never echoes native response data.
#[cfg(feature = "invoice-validation")]
pub fn project_invoice(bytes: &[u8], mode: OutputMode) -> Result<Value, &'static str> {
    use lightning_invoice::Bolt11Invoice;

    #[derive(Deserialize)]
    struct LndInvoice {
        payment_request: String,
        r_hash: String,
    }

    // Bound parser work independently of private stream retention. Unknown JSON
    // members remain private; duplicate selected members are rejected by serde.
    if bytes.len() > 64 * 1024 {
        return Err("invoice_response_too_large");
    }
    let (request, hash) = match mode {
        OutputMode::Bolt11 => (
            std::str::from_utf8(bytes)
                .map_err(|_| "invoice_format_invalid")?
                .trim()
                .to_owned(),
            None,
        ),
        OutputMode::LndInvoice => {
            if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
                return Err("invoice_format_invalid");
            }
            let response: LndInvoice =
                serde_json::from_slice(bytes).map_err(|_| "invoice_format_invalid")?;
            (response.payment_request, Some(response.r_hash))
        }
        _ => return Err("invoice_mode_invalid"),
    };
    if request.len() > 4096 {
        return Err("invoice_too_large");
    }
    let invoice: Bolt11Invoice = request.parse().map_err(|_| "invoice_invalid")?;
    invoice.check_signature().map_err(|_| "invoice_invalid")?;
    let payment_hash = invoice.payment_hash().to_string();
    if hash.is_some_and(|hash| hash != payment_hash) {
        return Err("invoice_hash_mismatch");
    }
    Ok(serde_json::json!({
        "payment_request":request,
        "payment_hash":payment_hash,
        "amount_msat":invoice.amount_milli_satoshis(),
        "currency":invoice.currency().to_string(),
        "expires_at_unix":invoice.expires_at().map(|expiry| expiry.as_secs()),
    }))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeOutput {
    #[serde(default)]
    pub mode: OutputMode,
    /// Fixed receipt fields, including allowlisted lifecycle leaf paths; last JSON document.
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_io: Option<crate::private_io::PrivateIo>,
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
const LIFECYCLE_FIELDS: &[&str] = &[
    "seedAccess.state",
    "seedAccess.requiresPassphrase",
    "cocoSession.state",
];
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
        if let Some(binding) = &self.private_io {
            binding.validate(self)?;
        }
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
                    && !LIFECYCLE_FIELDS.contains(&field.as_str())
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
        if LIFECYCLE_FIELDS.contains(&field.as_str()) {
            selected.insert(field.clone(), lifecycle_value(&value, field)?.clone());
            continue;
        }
        let item = value.get(field).ok_or("output_field_missing")?;
        let safe = if STATUS_FIELDS.contains(&field.as_str()) {
            item.as_str().is_some_and(|state| {
                matches!(
                    state,
                    "PAID"
                        | "ok"
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

// These are fixed leaf projections, not arbitrary traversal or object export.
// A null seedAccess parent represents an uninitialized native wallet; a missing
// parent or malformed child is an error and must never manufacture a state.
fn lifecycle_value<'a>(value: &'a Value, field: &str) -> Result<&'a Value, &'static str> {
    let (parent, leaf) = field.split_once('.').ok_or("output_field_missing")?;
    let node = value.get(parent).ok_or("output_field_missing")?;
    if parent == "seedAccess" && node.is_null() {
        return Ok(node);
    }
    let item = node.get(leaf).ok_or("output_field_missing")?;
    let safe = match field {
        "seedAccess.state" => item
            .as_str()
            .is_some_and(|state| matches!(state, "locked" | "available")),
        "seedAccess.requiresPassphrase" => item.is_boolean(),
        "cocoSession.state" => item.as_str().is_some_and(|state| {
            matches!(
                state,
                "stopped" | "starting" | "running" | "stopping" | "failed"
            )
        }),
        _ => false,
    };
    if safe {
        Ok(item)
    } else {
        Err("output_field_type_invalid")
    }
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
    fn lifecycle_projection_preserves_native_states_without_exporting_objects() {
        let fields = LIFECYCLE_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect::<Vec<_>>();
        let command = NativeCommand {
            private_io: None,
            script: String::new(),
            argv: vec!["cocod".into(), "status".into()],
            timeout_seconds: 30,
            output: NativeOutput {
                mode: OutputMode::JsonFields,
                fields: fields.clone(),
            },
        };
        command.validate().unwrap();
        let input = br#"{"seedAccess":{"state":"locked","requiresPassphrase":true,"secret":"canary"},"cocoSession":{"state":"stopped","lastFailure":{"message":"canary"}},"mnemonic":"canary"}"#;
        assert_eq!(
            project_receipt(input, &fields).unwrap(),
            serde_json::json!({
            "seedAccess.state":"locked", "seedAccess.requiresPassphrase":true, "cocoSession.state":"stopped"})
        );
        assert_eq!(
            project_receipt(
                br#"{"seedAccess":null,"cocoSession":{"state":"stopped"}}"#,
                &fields
            )
            .unwrap(),
            serde_json::json!({"seedAccess.state":null,"seedAccess.requiresPassphrase":null,"cocoSession.state":"stopped"})
        );
        for input in [
            br#"{"seedAccess":{"state":"canary","requiresPassphrase":true},"cocoSession":{"state":"stopped"}}"#.as_slice(),
            br#"{"seedAccess":{"state":null,"requiresPassphrase":true},"cocoSession":{"state":"stopped"}}"#,
            br#"{"seedAccess":{"state":"locked","requiresPassphrase":"true"},"cocoSession":{"state":"stopped"}}"#,
            br#"{"seedAccess":null,"cocoSession":{"state":"canary"}}"#,
            br#"{"seedAccess":null,"cocoSession":null}"#,
            br#"{"cocoSession":{"state":"stopped"}}"#,
        ] { assert!(project_receipt(input, &fields).is_err()); }
        for field in [
            "seedAccess",
            "cocoSession",
            "cocoSession.lastFailure",
            "cocoSession.lastFailure.message",
            "mnemonic",
        ] {
            let mut invalid = command.clone();
            invalid.output.fields = vec![field.into()];
            assert!(invalid.validate().is_err());
            assert!(project_receipt(input, &invalid.output.fields).is_err());
        }
        assert_eq!(
            project_receipt(br#"{"status":"ok","secret":"canary"}"#, &["status".into()]).unwrap(),
            serde_json::json!({"status":"ok"})
        );
    }

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
