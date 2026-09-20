use super::*;
use serde_json::json;

#[test]
fn patterns_bound_input_and_compilation_without_interpreting_literal_metacharacters() {
    let literal = pattern("[É]", false, true).unwrap();
    assert!(literal.is_match("value [é]"));
    assert!(!literal.is_match("value É"));
    assert!(pattern("", false, false).unwrap().is_match("anything"));
    assert!(pattern(&"é".repeat(2048), false, false).is_ok());
    assert_eq!(
        pattern(&"é".repeat(2049), false, false)
            .unwrap_err()
            .details
            .unwrap()["code"],
        "search_query_invalid"
    );
    for expression in ["[", "(?:a{1000}){1000}"] {
        assert_eq!(
            pattern(expression, true, false)
                .unwrap_err()
                .details
                .unwrap()["code"],
            "search_regex_invalid"
        );
    }
}

#[test]
fn pointers_validate_byte_limits_and_escapes_and_preserve_projection_semantics() {
    for pointer in ["", "/", "/a~1b/~0", "/~01", "/array/0", "/雪"] {
        assert_eq!(validate_pointer(pointer, 512), Ok(()));
    }
    for pointer in ["/bad~", "/bad~2", "/bad~~0", "/~é"] {
        assert_eq!(validate_pointer(pointer, 512), Err(PointerError::Escape));
    }
    for pointer in ["no-root".into(), format!("/{}", "é".repeat(256))] {
        assert_eq!(
            validate_pointer(&pointer, 512),
            Err(PointerError::RootOrLength)
        );
    }
    assert!(validate_fields(&[format!("/{}", "a".repeat(511))]).is_ok());
    assert!(validate_fields(&vec!["/id".into(); 32]).is_ok());
    assert!(validate_fields(&vec!["/id".into(); 33]).is_err());
    let document = json!({"a/b":{"~":42},"array":["first"],"~1":"escaped once","":null});
    assert_eq!(project(&document, &[]), document);
    let fields = ["", "/", "/a~1b/~0", "/array/0", "/~01", "/absent"].map(str::to_owned);
    assert_eq!(
        project(&document, &fields),
        json!({
            "":document,"/":null,"/a~1b/~0":42,"/array/0":"first","/~01":"escaped once","/absent":null
        })
    );
}

#[test]
fn response_measurement_counts_text_escaping_and_the_actual_continuation() {
    let page = json!({"items":[{"text":"雪\"\\\n".repeat(100)}],"next_cursor":"bound:0123456789","scanned_count":100});
    let expected = json!({
        "resultType":"complete",
        "content":[{"type":"text","text":page.to_string()}],
        "structuredContent":page,
        "isError":false
    });
    assert_eq!(
        serde_json::to_value(wire(&page).unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        wire_size(&page).unwrap(),
        serde_json::to_vec(&expected).unwrap().len()
    );
    assert!(wire_size(&page).unwrap() > 2 * serde_json::to_vec(&page).unwrap().len());
    let mut terminal = page;
    terminal["next_cursor"] = Value::Null;
    assert!(wire_size(&terminal).unwrap() < wire_size(&expected["structuredContent"]).unwrap());
}

#[test]
fn directories_retain_their_existing_validation_codes() {
    let store = proofstorm_store::Store::memory().unwrap();
    for invalid in [
        json!({"fields":["/broken~escape"]}),
        json!({"query":"[","regex":true}),
        json!({"query":"x".repeat(4097)}),
        json!({"fields":[format!("/{}", "x".repeat(512))]}),
    ] {
        let catalog = crate::catalog::list(
            proofstorm_core::default_catalog(),
            &serde_json::from_value(invalid.clone()).unwrap(),
            "linux/arm64",
            32 * 1024,
        )
        .unwrap_err();
        assert_eq!(catalog.details.unwrap()["code"], "catalog_query_invalid");
        let directory = crate::candidate::directory(
            &store,
            "workspace",
            "actor",
            &serde_json::from_value(invalid).unwrap(),
            32 * 1024,
        )
        .unwrap_err();
        assert_eq!(
            directory.details.unwrap()["code"],
            "directory_query_invalid"
        );
    }
}
