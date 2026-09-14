//! Common query validation and projection for bounded agent reads.
use crate::{ErrorData, coded_invalid_request};
use serde_json::Value;

pub(super) fn pattern(
    query: &str,
    regex: bool,
    insensitive: bool,
) -> Result<regex::Regex, ErrorData> {
    if query.len() > 4096 {
        return Err(coded_invalid_request(
            "search_query_invalid",
            "query must be at most 4096 bytes",
        ));
    }
    regex::RegexBuilder::new(&if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    })
    .case_insensitive(insensitive)
    .size_limit(1 << 20)
    .build()
    .map_err(|error| coded_invalid_request("search_regex_invalid", error.to_string()))
}

pub(super) fn validate_fields(fields: &[String]) -> Result<(), ErrorData> {
    if fields.len() > 32
        || fields.iter().any(|field| {
            field.len() > 512 || (!field.is_empty() && !field.starts_with('/')) || {
                let mut chars = field.chars();
                let mut invalid = false;
                while let Some(ch) = chars.next() {
                    if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
                        invalid = true;
                        break;
                    }
                }
                invalid
            }
        })
    {
        return Err(coded_invalid_request(
            "search_fields_invalid",
            "select at most 32 RFC 6901 JSON pointers, each at most 512 bytes",
        ));
    }
    Ok(())
}

pub(super) fn project(entry: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return entry.clone();
    }
    Value::Object(
        fields
            .iter()
            .map(|field| {
                (
                    field.clone(),
                    entry.pointer(field).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}

pub(super) fn wire_size(value: &impl serde::Serialize) -> Result<usize, ErrorData> {
    crate::serialized_size(&crate::activity_search::wire(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_or_excessive_queries_fail_and_literal_search_escapes_metacharacters() {
        assert!(pattern("[", true, false).is_err());
        assert!(pattern(&"x".repeat(4097), false, false).is_err());
        let literal = pattern("[A]", false, true).unwrap();
        assert!(literal.is_match("value [a]"));
        assert!(!literal.is_match("value A"));
        assert!(validate_fields(&["/a~1b/~0".into(), String::new()]).is_ok());
        for invalid in [
            vec!["/trailing~".into()],
            vec!["not-a-pointer".into()],
            vec!["/id".into(); 33],
        ] {
            assert!(validate_fields(&invalid).is_err());
        }
        let selected = project(&serde_json::json!({"a/b":{"~":42}}), &["/a~1b/~0".into()]);
        assert_eq!(selected["/a~1b/~0"], 42);
    }
}
