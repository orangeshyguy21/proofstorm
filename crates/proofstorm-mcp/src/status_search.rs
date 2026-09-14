//! Search and project before paging live status; never page an unfiltered dump.
use crate::{
    CellComponentStatusListRequest, CellComponentStatusListResponse, CellInstanceStatus,
    CellInventoryListRequest, CellInventoryListResponse, ErrorData, MAX_AGENT_RESPONSE_BYTES,
    coded_invalid_request, component_status_identity, digest_json, inventory_key, read_query,
    status_cursor, status_page_start, validate_status_list_limit,
};
use serde_json::{Value, json};

pub(super) fn components(
    mut status: CellInstanceStatus,
    request: &CellComponentStatusListRequest,
) -> Result<CellComponentStatusListResponse, ErrorData> {
    validate_status_list_limit(request.limit)?;
    read_query::validate_fields(&request.fields)?;
    if request.scan && !request.fields.is_empty() {
        return Err(coded_invalid_request(
            "search_fields_invalid",
            "choose scan or fields, not both",
        ));
    }
    let pattern = read_query::pattern(&request.query, request.regex, request.case_insensitive)?;
    let identity = component_status_identity(&status);
    status.components.sort_by(|a, b| a.id.cmp(&b.id));
    let observation_digest = digest_json(&status.components);
    let entries: Vec<_> = status
        .components
        .iter()
        .filter(|component| {
            request
                .component
                .as_ref()
                .is_none_or(|id| *id == component.id)
                && request.ready.is_none_or(|ready| ready == component.ready)
        })
        .map(|component| serde_json::to_value(component).expect("serializable component status"))
        .filter(|entry| pattern.is_match(&entry.to_string()))
        .collect();
    // Bind the matching set as well as the query: a readiness change cannot
    // silently insert/remove an item behind a filtered cursor.
    let fingerprint = digest_json(&(
        &identity,
        &request.component,
        request.ready,
        &request.query,
        request.regex,
        request.case_insensitive,
        request.scan,
        &request.fields,
        entries.iter().map(|entry| &entry["id"]).collect::<Vec<_>>(),
    ));
    let cursor_for = |entry: &Value| {
        status_cursor(
            "component",
            &request.instance_id,
            &fingerprint,
            entry["id"].as_str().unwrap(),
        )
    };
    let start = status_page_start(request.cursor.as_deref(), &entries, cursor_for)?;
    let mut end = (start + request.limit as usize).min(entries.len());
    loop {
        let response = CellComponentStatusListResponse {
            instance_id: request.instance_id.clone(),
            revision_digest: status.instance.revision_digest.clone(),
            observation_digest: observation_digest.clone(),
            matched_count: entries.len(),
            components: entries[start..end]
                .iter()
                .map(|entry| {
                    if request.scan {
                        json!({"id": entry["id"], "kind": entry["kind"], "ready": entry["ready"]})
                    } else {
                        read_query::project(entry, &request.fields)
                    }
                })
                .collect(),
            next_cursor: (end < entries.len() && end > start)
                .then(|| cursor_for(&entries[end - 1])),
        };
        if read_query::wire_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
            return Ok(response);
        }
        if end <= start + 1 {
            return Err(oversized("component"));
        }
        end -= 1;
    }
}

pub(super) fn inventory(
    mut status: CellInstanceStatus,
    request: &CellInventoryListRequest,
) -> Result<CellInventoryListResponse, ErrorData> {
    validate_status_list_limit(request.limit)?;
    read_query::validate_fields(&request.fields)?;
    let pattern = read_query::pattern(&request.query, request.regex, request.case_insensitive)?;
    status.inventory.sort_by_key(inventory_key);
    let inventory_digest = digest_json(&status.inventory);
    let fingerprint = digest_json(&(
        &status.instance.instance_key,
        &inventory_digest,
        &request.kind,
        &request.namespace,
        &request.query,
        request.regex,
        request.case_insensitive,
        &request.fields,
    ));
    let entries: Vec<_> = status
        .inventory
        .iter()
        .filter(|entry| {
            request.kind.as_ref().is_none_or(|kind| *kind == entry.kind)
                && request
                    .namespace
                    .as_ref()
                    .is_none_or(|namespace| *namespace == entry.namespace)
        })
        .map(|entry| {
            (
                inventory_key(entry),
                serde_json::to_value(entry).expect("serializable inventory"),
            )
        })
        .filter(|(_, entry)| pattern.is_match(&entry.to_string()))
        .collect();
    let cursor_for = |entry: &(String, Value)| {
        status_cursor("inventory", &request.instance_id, &fingerprint, &entry.0)
    };
    let start = status_page_start(request.cursor.as_deref(), &entries, cursor_for)?;
    let mut end = (start + request.limit as usize).min(entries.len());
    loop {
        let response = CellInventoryListResponse {
            instance_id: request.instance_id.clone(),
            inventory_digest: inventory_digest.clone(),
            matched_count: entries.len(),
            inventory: entries[start..end]
                .iter()
                .map(|(_, entry)| read_query::project(entry, &request.fields))
                .collect(),
            next_cursor: (end < entries.len() && end > start)
                .then(|| cursor_for(&entries[end - 1])),
        };
        if read_query::wire_size(&response)? <= MAX_AGENT_RESPONSE_BYTES {
            return Ok(response);
        }
        if end <= start + 1 {
            return Err(oversized("inventory"));
        }
        end -= 1;
    }
}

fn oversized(section: &str) -> ErrorData {
    coded_invalid_request(
        "status_response_too_large",
        format!(
            "one {section} entry exceeds the response budget; select smaller fields using JSON pointers{}",
            if section == "component" {
                " or use scan=true to find component IDs"
            } else {
                ""
            }
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> CellInstanceStatus {
        serde_json::from_value(json!({
            "instance":{"id":"fleet","workspace_id":"alpha","instance_key":"key","resource_name":"fleet","revision_digest":"sha256:revision","lock_digest":"sha256:lock","generation":2},
            "phase":"pending","instance_namespace":"fleet","observed_generation":1,
            "observed_revision_digest":"sha256:old","last_converged_revision":null,"retained_storage":{},
            "components":(0..100).map(|i| json!({"id":format!("node-{i:03}"),"kind":"bitcoin","ready":i%2==0,"conditions":[],"service":"\"\\\n".repeat(100),"ports":{},"observed_revision_digest":"sha256:old","observed_rollout_digest":"sha256:rollout"})).collect::<Vec<_>>(),
            "inventory":(0..100).map(|i| json!({"api_version":"v1","kind":if i%2==0 {"Service"} else {"Pod"},"namespace":"fleet","name":format!("node-{i:03}")})).collect::<Vec<_>>(),
            "teardown_receipt":null,"message":null
        })).unwrap()
    }

    #[test]
    fn component_pages_count_wire_bytes_and_preserve_every_match() {
        let status = status();
        let mut request: CellComponentStatusListRequest =
            serde_json::from_value(json!({"instance_id":"fleet","limit":50})).unwrap();
        let mut ids = Vec::new();
        loop {
            let page = components(status.clone(), &request).unwrap();
            assert!(read_query::wire_size(&page).unwrap() <= MAX_AGENT_RESPONSE_BYTES);
            assert!(!page.components.is_empty());
            assert!(
                page.components.len() < 50,
                "must shrink by bytes, not only count"
            );
            ids.extend(
                page.components
                    .iter()
                    .map(|entry| entry["id"].as_str().unwrap().to_owned()),
            );
            request.cursor = page.next_cursor;
            if request.cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            ids,
            (0..100).map(|i| format!("node-{i:03}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn filtered_scans_project_fields_and_fence_matching_membership() {
        let mut request: CellComponentStatusListRequest = serde_json::from_value(json!({"instance_id":"fleet","ready":false,"query":"NODE-0[0-2]","regex":true,"case_insensitive":true,"scan":true,"limit":2})).unwrap();
        let first = components(status(), &request).unwrap();
        assert_eq!(first.matched_count, 15);
        assert_eq!(
            first.components[0],
            json!({"id":"node-001","kind":"bitcoin","ready":false})
        );
        request.cursor = first.next_cursor;
        let mut changed = status();
        changed.components[3].service = "updated detail".into();
        assert_eq!(
            components(changed.clone(), &request).unwrap().components[0]["id"],
            "node-005"
        );
        changed.components[1].ready = true;
        assert!(components(changed, &request).is_err());
        request.query = "node".into();
        assert!(components(status(), &request).is_err());
        request.cursor = None;
        request.scan = false;
        request.component = Some("node-003".into());
        request.fields = vec!["/id".into(), "/ready".into(), "/missing".into()];
        let selected = components(status(), &request).unwrap();
        assert_eq!(selected.matched_count, 1);
        assert_eq!(
            selected.components[0],
            json!({"/id":"node-003","/ready":false,"/missing":null})
        );
    }

    #[test]
    fn oversized_status_has_a_scan_and_projection_escape() {
        let mut status = status();
        status.components[0].service = "huge".repeat(MAX_AGENT_RESPONSE_BYTES);
        let mut request: CellComponentStatusListRequest =
            serde_json::from_value(json!({"instance_id":"fleet","component":"node-000"})).unwrap();
        assert_eq!(
            components(status.clone(), &request)
                .unwrap_err()
                .data
                .unwrap()["code"],
            "status_response_too_large"
        );
        request.scan = true;
        assert_eq!(
            components(status.clone(), &request)
                .unwrap()
                .components
                .len(),
            1
        );
        request.scan = false;
        request.fields = vec!["/id".into(), "/ready".into()];
        assert_eq!(
            components(status, &request).unwrap().components[0]["/id"],
            "node-000"
        );
    }

    #[test]
    fn inventory_filters_and_projection_are_cursor_bound() {
        let mut request: CellInventoryListRequest = serde_json::from_value(json!({"instance_id":"fleet","kind":"Pod","namespace":"fleet","query":"node-00","fields":["/name"],"limit":2})).unwrap();
        let first = inventory(status(), &request).unwrap();
        assert_eq!(first.matched_count, 5);
        assert_eq!(first.inventory[0], json!({"/name":"node-001"}));
        request.cursor = first.next_cursor;
        assert_eq!(
            inventory(status(), &request).unwrap().inventory[0]["/name"],
            "node-005"
        );
        request.fields = vec!["/kind".into()];
        assert!(inventory(status(), &request).is_err());
    }
}
