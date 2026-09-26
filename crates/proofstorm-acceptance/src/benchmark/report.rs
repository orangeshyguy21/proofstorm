//! Deterministic structured claims and a separate presentation requirement.
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Claims {
    success: bool,
    minted_sat: u64,
    paid_sat: u64,
    remaining_sat: u64,
    cleanup: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    pub format_valid: bool,
    pub claims: Option<Claims>,
}

fn claims(text: &str) -> Option<Claims> {
    let text = text.trim();
    (text.starts_with('{') && text.ends_with('}'))
        .then(|| serde_json::from_str(text).ok())
        .flatten()
}

impl Report {
    pub fn parse(text: &str) -> Self {
        if let Some(claims) = claims(text) {
            return Self {
                format_valid: true,
                claims: Some(claims),
            };
        }
        // Accept one complete trailing object for claim validation only. Never
        // choose the best of several objects, ignore trailing text, or repair JSON.
        let claims = text.find('{').and_then(|offset| {
            (!text[..offset].contains('}'))
                .then(|| claims(&text[offset..]))
                .flatten()
        });
        Self {
            format_valid: false,
            claims,
        }
    }

    pub fn consistent(
        &self,
        task: &super::task::Task,
        checkpoint_valid: bool,
        remaining: Option<u64>,
    ) -> bool {
        checkpoint_valid
            && self.claims.as_ref().is_some_and(|c| {
                matches_schema(&serde_json::json!(c), &task.report_schema)
                    && Some(c.remaining_sat) == remaining
            })
    }
}

// The task's small report schema supports exact properties, scalar types,
// constants and integer bounds. Fail closed rather than accepting unknown types.
fn matches_schema(claims: &serde_json::Value, schema: &serde_json::Value) -> bool {
    let Some(properties) = schema["properties"].as_object() else {
        return false;
    };
    let Some(required) = schema["required"].as_array() else {
        return false;
    };
    schema["type"] == "object"
        && schema["additionalProperties"] == false
        && claims
            .as_object()
            .is_some_and(|values| values.keys().all(|key| properties.contains_key(key)))
        && required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| claims.get(key).is_some()))
        && properties.iter().all(|(key, rule)| {
            let Some(value) = claims.get(key) else {
                return false;
            };
            let valid_type = match rule["type"].as_str() {
                Some("boolean") => value.is_boolean(),
                Some("integer") => value.as_u64().is_some(),
                _ => false,
            };
            valid_type
                && rule.get("const").is_none_or(|expected| expected == value)
                && rule.get("minimum").is_none_or(|min| {
                    min.as_u64()
                        .zip(value.as_u64())
                        .is_some_and(|(min, value)| value >= min)
                })
                && rule.get("maximum").is_none_or(|max| {
                    max.as_u64()
                        .zip(value.as_u64())
                        .is_some_and(|(max, value)| value <= max)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    const GOOD: &str =
        r#"{"success":true,"minted_sat":1000,"paid_sat":100,"remaining_sat":899,"cleanup":true}"#;

    #[test]
    fn prose_loses_format_credit_without_erasing_valid_claims() {
        let strict = Report::parse(GOOD);
        assert!(
            strict.format_valid && strict.consistent(super::super::task::o1(), true, Some(899))
        );
        let prose = Report::parse(&format!("Payment completed.\n\n{GOOD}"));
        assert!(!prose.format_valid);
        assert!(prose.consistent(super::super::task::o1(), true, Some(899)));
        assert!(!prose.consistent(super::super::task::o1(), true, Some(900)));
        assert!(!prose.consistent(super::super::task::o1(), false, Some(899)));
    }

    #[test]
    fn ambiguous_missing_and_false_claims_fail_closed() {
        for text in [
            format!("{GOOD}\n{GOOD}"),
            format!("{GOOD}\nDone"),
            format!("```json\n{GOOD}\n```"),
            GOOD.replacen("\"success\":true", "\"success\":false,\"success\":true", 1),
            GOOD.replace("\"success\":true", "\"success\":false"),
            GOOD.replace("\"paid_sat\":100", "\"paid_sat\":101"),
            "{}".into(),
            "success".into(),
        ] {
            assert!(
                !Report::parse(&text).consistent(super::super::task::o1(), true, Some(899)),
                "{text}"
            );
        }
    }
}
