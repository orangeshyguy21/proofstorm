//! Deterministic structured claims and a separate presentation requirement.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub(super) struct Report {
    pub format_valid: bool,
    pub claims: Option<serde_json::Value>,
}

// Value normally accepts duplicate keys. Reject them before schema validation,
// preserving the same unambiguous-report contract for every task.
struct UniqueObject(serde_json::Value);
impl<'de> Deserialize<'de> for UniqueObject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueObject;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("one JSON object without duplicate keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate report key"));
                    }
                    values.insert(key, map.next_value()?);
                }
                Ok(UniqueObject(serde_json::Value::Object(values)))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}
fn claims(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<UniqueObject>(text.trim())
        .ok()
        .map(|object| object.0)
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

    pub fn consistent(&self, task: &super::task::Task, observed: &serde_json::Value) -> bool {
        self.claims.as_ref().is_some_and(|c| {
            let claims = serde_json::json!(c);
            matches_schema(&claims, &task.report_schema) && claims == *observed
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
                Some("string") => value.is_string(),
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
        let observed = serde_json::from_str(GOOD).unwrap();
        let strict = Report::parse(GOOD);
        assert!(strict.format_valid && strict.consistent(super::super::task::o1(), &observed));
        let prose = Report::parse(&format!("Payment completed.\n\n{GOOD}"));
        assert!(!prose.format_valid);
        assert!(prose.consistent(super::super::task::o1(), &observed));
        let mut different = observed.clone();
        different["remaining_sat"] = serde_json::json!(900);
        assert!(!prose.consistent(super::super::task::o1(), &different));
        assert!(!prose.consistent(super::super::task::o1(), &serde_json::Value::Null));
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
                !Report::parse(&text).consistent(
                    super::super::task::o1(),
                    &serde_json::from_str(GOOD).unwrap()
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn schema_allows_honest_failure_but_truth_requires_observations() {
        let task = super::super::task::o1();
        let failed = serde_json::json!({"success":false,"minted_sat":1000,"paid_sat":0,"remaining_sat":1000,"cleanup":true});
        assert!(matches_schema(&failed, &task.report_schema));
        let report = Report::parse(&failed.to_string());
        assert!(report.format_valid && report.consistent(task, &failed));
        for key in [
            "success",
            "minted_sat",
            "paid_sat",
            "remaining_sat",
            "cleanup",
        ] {
            let mut unknown = failed.clone();
            unknown[key] = serde_json::Value::Null;
            assert!(!report.consistent(task, &unknown), "{key}");
            let mut false_claim = failed.clone();
            false_claim[key] = if failed[key].is_boolean() {
                serde_json::json!(!failed[key].as_bool().unwrap())
            } else {
                serde_json::json!(failed[key].as_u64().unwrap() + 1)
            };
            assert!(
                !Report::parse(&false_claim.to_string()).consistent(task, &failed),
                "{key}"
            );
        }
    }
}
