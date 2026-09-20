use super::*;
use crate::query::wire_size;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Page {
    source: String,
    scanned_count: usize,
    items: Vec<Value>,
    next_cursor: Option<String>,
}

fn page() -> Page {
    Page {
        source: "recorded".into(),
        scanned_count: 99,
        items: vec![],
        next_cursor: Some("snapshot:already-scanned".into()),
    }
}

fn append(page: &mut Page, item: Value, cursor: Option<String>, limit: usize) -> bool {
    push_bounded(
        page,
        item,
        cursor,
        |page| (&mut page.items, &mut page.next_cursor),
        |page| wire_size(page).map(|size| size <= limit),
    )
    .unwrap()
}

#[test]
fn exact_budget_includes_escaped_content_and_continuation_and_rolls_back_rejected_items() {
    let mut result = page();
    let item = json!({"id":"first","text":"雪\"\\\n".repeat(30)});
    let cursor = Some("snapshot:first".into());
    let expected = Page {
        items: vec![item.clone()],
        next_cursor: cursor.clone(),
        ..result.clone()
    };
    let limit = wire_size(&expected).unwrap();
    assert!(!append(
        &mut result,
        item.clone(),
        cursor.clone(),
        limit - 1
    ));
    assert_eq!(result, page());
    assert!(append(&mut result, item.clone(), cursor, limit));
    assert_eq!(result, expected);
    // Even a terminal candidate must be rolled back to the prior continuation.
    assert!(!append(&mut result, item.clone(), None, limit));
    assert_eq!(result, expected);
    let mut next = page();
    assert!(append(&mut next, item, None, limit));
    assert!(next.next_cursor.is_none());
}

#[test]
fn admission_measures_page_metadata_and_preserves_scan_progress() {
    let mut result = page();
    result.source = "large metadata".repeat(100);
    let before = result.clone();
    assert!(!append(
        &mut result,
        json!("small"),
        Some("unread".into()),
        256
    ));
    assert_eq!(result, before);
    assert!(append(
        &mut result,
        json!("small"),
        Some("read".into()),
        8192
    ));
    assert_eq!(
        result.scanned_count, 99,
        "the directory advances its own scan"
    );
}

#[test]
fn measurement_errors_restore_the_original_page() {
    let mut result = page();
    result.items.push(json!("accepted"));
    let before = result.clone();
    let error = push_bounded(
        &mut result,
        json!("candidate"),
        None,
        |page| (&mut page.items, &mut page.next_cursor),
        |_| Err::<bool, _>("measurement failed"),
    );
    assert_eq!(error, Err("measurement failed"));
    assert_eq!(result, before);
}
