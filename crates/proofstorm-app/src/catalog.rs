//! Passive catalog discovery, shared by MCP and HTTP. Search never scans configuration or logs.
use crate::{Error, query};
use proofstorm_core::{CatalogEntry, CatalogResponse, ControlClass, digest_json};
use proofstorm_view::{CatalogListRequest, CatalogPage};
use serde_json::{Value, json};

/// Read only the catalog versions visible to this actor. This never contacts the runtime.
pub fn read(
    store: &proofstorm_store::Store,
    workspace: &str,
    principal: &str,
    query: &CatalogListRequest,
    byte_limit: usize,
) -> Result<CatalogPage, Error> {
    let catalog = store
        .effective_catalog(workspace, principal)
        .map_err(Error::from)?;
    let platform = crate::platform::container_platform()
        .map_err(|e| Error::problem("platform_unsupported", e.to_string()))?;
    list_scoped(
        &catalog,
        query,
        &platform,
        byte_limit,
        &(workspace, principal),
    )
}

/// HTTP accepts the exact shared selectors as a URL-encoded JSON `selectors` value.
pub fn http_read(cells: &crate::cell::Cells, query: &str) -> Result<CatalogPage, Error> {
    let selectors = http_selectors(query)?;
    let catalog = cells
        .store
        .effective_catalog(&cells.workspace, &cells.principal)
        .map_err(Error::from)?;
    let platform = crate::platform::container_platform()
        .map_err(|e| Error::problem("platform_unsupported", e.to_string()))?;
    // Browser tables need full pages, including the details expanded in each row.
    // Keep the smaller structured-response budget for agent tools.
    list_scoped_with_limits(
        &catalog,
        &selectors,
        &platform,
        256 * 1024,
        128 * 1024,
        &(&cells.workspace, &cells.principal),
    )
}

pub(crate) fn http_selectors<T: serde::de::DeserializeOwned>(query: &str) -> Result<T, Error> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Query {
        selectors: String,
    }
    if query.len() > 16384 {
        return Err(Error::problem(
            "catalog_query_invalid",
            "Query is too large",
        ));
    }
    let query: Query = serde_urlencoded::from_str(query)
        .map_err(|e| Error::problem("catalog_query_invalid", e.to_string()))?;
    serde_json::from_str(&query.selectors)
        .map_err(|e| Error::problem("catalog_query_invalid", e.to_string()))
}

pub fn list(
    catalog: &CatalogResponse,
    query: &CatalogListRequest,
    platform: &str,
    byte_limit: usize,
) -> Result<CatalogPage, Error> {
    list_scoped(catalog, query, platform, byte_limit, &())
}

fn list_scoped(
    catalog: &CatalogResponse,
    query: &CatalogListRequest,
    platform: &str,
    byte_limit: usize,
    scope: &impl serde::Serialize,
) -> Result<CatalogPage, Error> {
    list_scoped_with_limits(catalog, query, platform, byte_limit, 8 * 1024, scope)
}

fn list_scoped_with_limits(
    catalog: &CatalogResponse,
    query: &CatalogListRequest,
    platform: &str,
    byte_limit: usize,
    structured_byte_limit: usize,
    scope: &impl serde::Serialize,
) -> Result<CatalogPage, Error> {
    validate(query)?;
    let pattern = query::pattern(&query.query, query.regex, query.case_insensitive)
        .map_err(|error| Error::problem("catalog_query_invalid", error.message))?;
    let catalog_digest = digest_json(catalog);
    let mut selectors = query.clone();
    selectors.cursor = None;
    let identity = digest_json(&(&catalog_digest, &selectors, platform, scope));
    let mut entries = catalog
        .entries
        .iter()
        .filter(|entry| {
            matches(entry, query)
                && searchable(entry)
                    .iter()
                    .any(|field| pattern.is_match(field))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.version.cmp(&b.version)));
    let offset = if let Some(cursor) = &query.cursor {
        let (bound, offset) = cursor.rsplit_once(':').ok_or_else(cursor_error)?;
        if bound != identity {
            return Err(cursor_error());
        }
        let offset = offset.parse::<usize>().map_err(|_| cursor_error())?;
        if offset >= entries.len() {
            return Err(cursor_error());
        }
        offset
    } else {
        0
    };
    let mut result = CatalogPage {
        api_version: catalog.api_version.clone(),
        catalog_digest,
        matched_count: entries.len(),
        items: vec![],
        next_cursor: None,
    };
    for (index, entry) in entries
        .iter()
        .enumerate()
        .skip(offset)
        .take(query.limit.clamp(1, 50) as usize)
    {
        let preferred = catalog.implementations.iter().any(|support| {
            support.implementation == entry.id
                && support.preferred_version.as_deref() == Some(&entry.version)
        });
        let value = summary(entry, preferred, platform);
        let selected = if query.scan {
            json!({"id":entry.id,"kind":entry.kind,"version":entry.version,"origin":entry.origin(),"image":entry.image,"platform":value["platform"]})
        } else if query.fields.is_empty() {
            value
        } else {
            query::project(&value, &query.fields)
        };
        if !query::push_bounded(
            &mut result,
            selected,
            (index + 1 < entries.len()).then(|| format!("{identity}:{}", index + 1)),
            |page| (&mut page.items, &mut page.next_cursor),
            |page| {
                let structured_size = serde_json::to_vec(page)?.len();
                query::wire_size(page)
                    .map(|size| size <= byte_limit && structured_size <= structured_byte_limit)
            },
        )
        .map_err(|error| Error::failure(error.to_string(), None))?
        {
            if result.items.is_empty() {
                return Err(Error::problem(
                    "catalog_response_too_large",
                    "Select scan=true or smaller fields",
                ));
            }
            break;
        }
    }
    Ok(result)
}

fn cursor_error() -> Error {
    Error::problem(
        "catalog_cursor_invalid",
        "Catalog or selectors changed; restart without cursor",
    )
}

fn validate(query: &CatalogListRequest) -> Result<(), Error> {
    if query.query.len() > query::MAX_QUERY_BYTES
        || query::validate_fields(&query.fields).is_err()
        || query.scan && !query.fields.is_empty()
        || query
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > 256)
    {
        return Err(Error::problem(
            "catalog_query_invalid",
            "Bound query/fields; choose scan or fields and use valid JSON pointers",
        ));
    }
    Ok(())
}

fn matches(entry: &CatalogEntry, query: &CatalogListRequest) -> bool {
    (query.implementations.is_empty() || query.implementations.contains(&entry.id))
        && (query.kinds.is_empty() || query.kinds.contains(&entry.kind))
        && (query.origins.is_empty() || query.origins.contains(&entry.origin()))
        && query.features_all.is_subset(&entry.features)
        && (query.release_channels.is_empty()
            || query.release_channels.contains(&entry.release_channel))
        && (if query.support_lifecycles.is_empty() {
            entry.support_lifecycle != proofstorm_core::SupportLifecycle::Deprecated
        } else {
            query.support_lifecycles.contains(&entry.support_lifecycle)
        })
        && query.dependency.as_ref().is_none_or(|filter| {
            entry.compatible_dependencies.iter().any(|dep| {
                dep.link_kind == filter.link_kind
                    && dep.implementation == filter.implementation
                    && filter
                        .version
                        .as_ref()
                        .is_none_or(|v| dep.versions.contains(v))
            })
        })
}

fn searchable(entry: &CatalogEntry) -> Vec<&str> {
    let mut fields = vec![
        entry.id.as_str(),
        &entry.description,
        &entry.version,
        &entry.image,
    ];
    if let Some(source) = &entry.source {
        fields.extend([
            source.candidate_id.as_str(),
            &source.pull_request_url,
            &source.repository,
            &source.commit_sha,
        ]);
        if let Some(provenance) = &source.provenance {
            fields.push(provenance.requested_source.label());
        }
    } else if let Some(provenance) = &entry.build_provenance {
        fields.extend([provenance.repository.as_str(), &provenance.commit_sha]);
    }
    fields
}

#[must_use]
pub fn summary(entry: &CatalogEntry, preferred: bool, platform: &str) -> Value {
    let platform = entry.source.as_ref().map_or(Some(platform), |s| {
        s.provenance.as_ref().map(|p| p.platform.as_str())
    });
    let candidate_support = if entry.source.is_none() {
        proofstorm_core::candidate_build_profile(&entry.id).map(|p| json!({"profile":p.id,"profile_version":p.version,"catalog_implementations":p.catalog_implementations,"source_types":["pull_request","commit","tag"]}))
    } else {
        None
    };
    let shared_image_implementations = entry.source.as_ref().map_or_else(
        || {
            if entry.image == proofstorm_core::CDK_MINT_IMAGE {
                vec!["cdk", "cdk-bdk", "cdk-ldk"]
            } else {
                Vec::new()
            }
        },
        |source| {
            source.provenance.as_ref().map_or_else(Vec::new, |p| {
                p.profile
                    .catalog_implementations
                    .iter()
                    .map(String::as_str)
                    .collect()
            })
        },
    );
    let control = [
        ControlClass::Target,
        ControlClass::Cell,
        ControlClass::Workspace,
        ControlClass::Oracle,
    ]
    .into_iter()
    .find(|control| entry.allowed_control.contains(control));
    json!({"id":entry.id,"kind":entry.kind,"description":entry.description,"version":entry.version,"preferred":preferred,
        "adapter_version":entry.adapter_version,"protocol_action_adapter_version":entry.protocol_action_adapter_version,
        "config_version":entry.config_version,"config_schema_digest":entry.config_schema_digest,
        "allowed_control":entry.allowed_control,"recommended_control":control,
        "release_channel":entry.release_channel,"support_lifecycle":entry.support_lifecycle,
        "origin":entry.origin(),"platform":platform,"image":entry.image,
        "candidate_support":candidate_support,
        "shared_image_implementations":shared_image_implementations,
        "candidate_id":entry.source.as_ref().map(|s| &s.candidate_id),
        "repository":entry.source.as_ref().map(|s| &s.repository).or_else(||entry.build_provenance.as_ref().map(|p| &p.repository)),
        "commit_sha":entry.source.as_ref().map(|s| &s.commit_sha).or_else(||entry.build_provenance.as_ref().map(|p| &p.commit_sha)),
        "compatibility_basis":if entry.source.is_some() {"inherited_unverified"} else {"built_in_contract"}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{CatalogOrigin, ComponentKind, default_catalog};

    fn query(value: Value) -> CatalogListRequest {
        serde_json::from_value(value).unwrap()
    }
    fn page(value: Value) -> CatalogPage {
        list(default_catalog(), &query(value), "linux/arm64", 24 * 1024).unwrap()
    }

    #[test]
    fn workspace_filter_returns_an_authorable_workspace_contract() {
        let result = page(json!({"kinds":["workspace"]}));
        assert_eq!(result.matched_count, 1);
        let entry = &result.items[0];
        assert_eq!(entry["id"], "workspace");
        assert_eq!(entry["kind"], "workspace");
        assert_eq!(entry["allowed_control"], json!(["workspace"]));
        assert_eq!(entry["recommended_control"], "workspace");
        let component = serde_json::from_value(json!({
            "id":"scripts", "kind":entry["kind"], "implementation":entry["id"],
            "version":entry["version"], "config_version":entry["config_version"],
            "control":entry["recommended_control"], "config":{}
        }))
        .unwrap();
        proofstorm_core::validate_catalog_component(&component, default_catalog()).unwrap();
    }

    #[test]
    fn retired_entries_require_an_explicit_discovery_filter() {
        let mut entry = default_catalog().entries[0].clone();
        entry.support_lifecycle = proofstorm_core::SupportLifecycle::Deprecated;
        assert!(!matches(&entry, &CatalogListRequest::default()));
        assert!(matches(
            &entry,
            &query(json!({"support_lifecycles":["deprecated"]}))
        ));
        assert!(!matches(
            &entry,
            &query(json!({"support_lifecycles":["preferred","supported"]}))
        ));
    }

    #[test]
    fn byte_limited_pages_preserve_each_entry_and_the_rejected_items_continuation() {
        let mut catalog = default_catalog().clone();
        let template = catalog.entries[0].clone();
        catalog.entries = (0..4)
            .map(|index| {
                let mut entry = template.clone();
                entry.version = format!("preview-{index}");
                entry.description = "雪\"\\\n".repeat(200);
                entry
            })
            .collect();
        let mut request = query(json!({"limit":1,"fields":["/version","/description"]}));
        let single = list(&catalog, &request, "linux/arm64", 32 * 1024).unwrap();
        let budget = query::wire_size(&single).unwrap();
        request.limit = 50;
        let mut versions = Vec::new();
        loop {
            let page = list(&catalog, &request, "linux/arm64", budget).unwrap();
            assert_eq!(page.matched_count, 4);
            assert_eq!(page.items.len(), 1);
            assert!(query::wire_size(&page).unwrap() <= budget);
            let version = page.items[0]["/version"].as_str().unwrap().to_owned();
            assert!(!versions.contains(&version), "pagination must advance");
            versions.push(version);
            request.cursor = page.next_cursor;
            if request.cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            versions,
            ["preview-0", "preview-1", "preview-2", "preview-3"]
        );
        assert_eq!(
            list(&catalog, &request, "linux/arm64", 1)
                .unwrap_err()
                .details
                .unwrap()["code"],
            "catalog_response_too_large"
        );
    }

    #[test]
    fn browser_pages_hold_25_full_entries_and_preserve_tool_budget() {
        let mut catalog = default_catalog().clone();
        let template = catalog.entries[0].clone();
        catalog.entries = (0..30)
            .map(|index| {
                let mut entry = template.clone();
                entry.version = format!("preview-{index:02}");
                entry
            })
            .collect();
        let mut request = query(json!({"limit":25}));
        let first = list_scoped_with_limits(
            &catalog,
            &request,
            "linux/arm64",
            256 * 1024,
            128 * 1024,
            &(),
        )
        .unwrap();
        assert_eq!(first.items.len(), 25);
        assert_eq!(first.matched_count, 30);
        assert!(
            first
                .items
                .iter()
                .all(|item| item["image"].is_string() && item["description"].is_string())
        );
        request.cursor = first.next_cursor;
        assert!(request.cursor.is_some());
        let last = list_scoped_with_limits(
            &catalog,
            &request,
            "linux/arm64",
            256 * 1024,
            128 * 1024,
            &(),
        )
        .unwrap();
        assert_eq!(last.items.len(), 5);
        assert!(last.next_cursor.is_none());
        assert!(first.items.iter().all(|item| !last.items.contains(item)));
        request.cursor = None;
        let tool_page = list(&catalog, &request, "linux/arm64", 32 * 1024).unwrap();
        assert!(serde_json::to_vec(&tool_page).unwrap().len() <= 8 * 1024);
        assert!(tool_page.items.len() < 25);
    }

    #[test]
    fn family_kind_origin_and_exact_filters_intersect() {
        let family = page(json!({"query":"CDK"}));
        assert_eq!(family.matched_count, 5);
        assert_eq!(
            page(json!({"query":"cdk","kinds":["mint"]})).matched_count,
            3
        );
        assert_eq!(page(json!({"kinds":["mint"]})).matched_count, 5);
        assert_eq!(page(json!({"implementations":["cdk"]})).matched_count, 1);
        let wallets = page(json!({"kinds":["wallet"],"origins":["built_in"]}));
        assert_eq!(wallets.matched_count, 4);
        assert!(
            wallets
                .items
                .iter()
                .any(|e| e["id"] == "cocod-wallet" && e["support_lifecycle"] == "experimental")
        );
        assert_eq!(
            page(json!({"query":"CDK","case_insensitive":false,"implementations":["nutshell"]}))
                .matched_count,
            0
        );
        assert_eq!(
            page(json!({"query":"^cdk-","regex":true,"kinds":["mint"]})).matched_count,
            2
        );
    }

    #[test]
    fn projections_and_pagination_keep_identity_and_invalidate_changed_queries() {
        let mut request = query(json!({"kinds":["mint"],"limit":1,"fields":["/id","/origin"]}));
        let mut ids = Vec::new();
        loop {
            let result = list(default_catalog(), &request, "linux/arm64", 24 * 1024).unwrap();
            ids.push(result.items[0]["/id"].as_str().unwrap().to_owned());
            request.cursor = result.next_cursor;
            if request.cursor.is_none() {
                break;
            }
            let mut changed = request.clone();
            changed.origins.insert(CatalogOrigin::Candidate);
            assert!(list(default_catalog(), &changed, "linux/arm64", 24 * 1024).is_err());
        }
        assert_eq!(ids.len(), 5);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            4
        );
        assert!(
            list(
                default_catalog(),
                &query(json!({"query":"[","regex":true})),
                "linux/arm64",
                24 * 1024
            )
            .is_err()
        );
        let mut catalog = default_catalog().clone();
        let entry = catalog
            .entries
            .iter_mut()
            .find(|e| e.kind == ComponentKind::Wallet)
            .unwrap();
        entry.config_schema["search_secret"] = json!("configuration-only-needle");
        assert_eq!(
            list(
                &catalog,
                &query(json!({"query":"configuration-only-needle"})),
                "linux/arm64",
                24 * 1024
            )
            .unwrap()
            .matched_count,
            0
        );
    }

    #[test]
    fn catalog_cursors_cannot_cross_actor_workspace_or_platform_scope() {
        let mut request = query(json!({"limit":1}));
        let page = list_scoped(
            default_catalog(),
            &request,
            "linux/arm64",
            32 * 1024,
            &("workspace", "agent"),
        )
        .unwrap();
        request.cursor = page.next_cursor;
        assert!(
            list_scoped(
                default_catalog(),
                &request,
                "linux/arm64",
                32 * 1024,
                &("workspace", "agent")
            )
            .is_ok()
        );
        for scope in [("other", "agent"), ("workspace", "reader")] {
            assert!(
                list_scoped(
                    default_catalog(),
                    &request,
                    "linux/arm64",
                    32 * 1024,
                    &scope
                )
                .is_err()
            );
        }
        assert!(
            list_scoped(
                default_catalog(),
                &request,
                "linux/amd64",
                32 * 1024,
                &("workspace", "agent")
            )
            .is_err()
        );
    }
}
