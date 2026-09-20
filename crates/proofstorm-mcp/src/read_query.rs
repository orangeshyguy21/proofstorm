//! Transport error mapping for shared bounded-query primitives.
use crate::{CallToolResult, ErrorData, app_error, coded_invalid_request};
use proofstorm_app::query;
pub(super) use query::project;

pub(super) fn pattern(
    query: &str,
    regex: bool,
    insensitive: bool,
) -> Result<regex::Regex, ErrorData> {
    query::pattern(query, regex, insensitive).map_err(app_error)
}

pub(super) fn validate_pointer(pointer: &str) -> Result<(), ErrorData> {
    use proofstorm_app::query::{self, PointerError};
    query::validate_pointer(pointer, 4096).map_err(|error| {
        coded_invalid_request("invalid_json_pointer", match error {
            PointerError::RootOrLength => "Use an RFC 6901 pointer of at most 4096 bytes, such as /artifact/content/stdout",
            PointerError::Escape => "Escape ~ as ~0 and / inside a key as ~1",
        })
    })
}

pub(super) fn validate_fields(fields: &[String]) -> Result<(), ErrorData> {
    query::validate_fields(fields).map_err(app_error)
}

pub(super) fn wire(value: &impl serde::Serialize) -> Result<CallToolResult, ErrorData> {
    query::wire(value).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

pub(super) fn wire_size(value: &impl serde::Serialize) -> Result<usize, ErrorData> {
    query::wire_size(value).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_errors_preserve_codes_and_longer_individual_pointer_limits() {
        for (result, code) in [
            (
                pattern(&"x".repeat(4097), false, false).map(|_| ()),
                "search_query_invalid",
            ),
            (
                pattern("[", true, false).map(|_| ()),
                "search_regex_invalid",
            ),
            (validate_fields(&["/bad~".into()]), "search_fields_invalid"),
        ] {
            assert_eq!(result.unwrap_err().data.unwrap()["code"], code);
        }
        let long = format!("/{}", "x".repeat(512));
        assert!(validate_fields(std::slice::from_ref(&long)).is_err());
        assert!(validate_pointer(&long).is_ok());
        assert!(validate_pointer(&format!("/{}", "x".repeat(4095))).is_ok());
        for pointer in ["/bad~".into(), format!("/{}", "x".repeat(4096))] {
            assert_eq!(
                validate_pointer(&pointer).unwrap_err().data.unwrap()["code"],
                "invalid_json_pointer"
            );
        }
        let request =
            serde_json::from_value(serde_json::json!({"name":"cell","query":"[","regex":true}))
                .unwrap();
        let error = crate::activity_search::search(
            &proofstorm_store::Store::memory().unwrap(),
            "workspace",
            "actor",
            &request,
        )
        .unwrap_err();
        assert_eq!(error.data.unwrap()["code"], "activity_search_regex_invalid");
    }
}
