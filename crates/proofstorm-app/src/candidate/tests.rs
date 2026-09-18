use super::*;
use proofstorm_core::{CandidateDiagnostics, CandidateLog, ComponentKind};
use proofstorm_view::{CatalogListRequest, DirectoryQuery};
use serde_json::json;

fn store() -> Store {
    let store = Store::memory().unwrap();
    store
        .put_workspace(&proofstorm_store::Workspace {
            id: "workspace".into(),
            name: "Workspace".into(),
        })
        .unwrap();
    store.put_principal("agent").unwrap();
    store.put_principal("reader").unwrap();
    for cap in [
        Capability::CandidateBuild,
        Capability::CandidateRead,
        Capability::CatalogRead,
    ] {
        store.grant("workspace", "agent", cap).unwrap();
    }
    store
        .grant("workspace", "reader", Capability::CatalogRead)
        .unwrap();
    store
}
fn request(implementation: &str, id: &str) -> CandidateBuildRequest {
    serde_json::from_value(json!({"implementation":implementation,"candidate_id":id,"source":{"type":"commit","sha":"a".repeat(40)},"request_id":id})).unwrap()
}
fn succeed(store: &Store, mut candidate: CandidateBuild) -> CandidateBuild {
    candidate.phase = CandidateBuildPhase::Succeeded;
    candidate.completed_at_unix = Some(candidate.accepted_at_unix + 1);
    candidate.image = Some(format!(
        "proofstorm-registry.localhost:5000/candidates/{}@sha256:{}",
        candidate.implementation,
        "b".repeat(64)
    ));
    store
        .update_candidate_build("workspace", &candidate)
        .unwrap()
}

#[tokio::test]
async fn candidate_source_forms_preserve_admission_and_legacy_null() {
    let sha = "a".repeat(40);
    for (index, fields) in [
        json!({"source":{"type":"pull_request","url":"https://github.com/cashubtc/cdk/pull/123"}}),
        json!({"source":{"type":"commit","sha":sha}}),
        json!({"source":{"type":"commit","url":format!("https://github.com/cashubtc/cdk/commit/{sha}")}}),
        json!({"source":{"type":"commit","sha":sha,"url":null}}),
        json!({"source":{"type":"tag","tag":"v0.18.0"}}),
        json!({"pull_request_url":"https://github.com/cashubtc/cdk/pull/123"}),
        json!({"source":null,"pull_request_url":"https://github.com/cashubtc/cdk/pull/123"}),
    ].into_iter().enumerate() {
        let store = store();
        let mut value = json!({"candidate_id":format!("source-{index}"),"implementation":"cdk","request_id":format!("source-{index}")});
        value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        let request = serde_json::from_value(value).unwrap();
        let candidate = admit_with_resolver(&store, "workspace", "agent", &request, |_, repository| async move {
            Ok((format!("https://github.com/{repository}.git"), "a".repeat(40)))
        }).await.unwrap();
        assert_eq!(candidate.phase, CandidateBuildPhase::Pending);
        assert!(candidate.provenance.is_some());
    }
}

#[tokio::test]
async fn candidate_source_validation_rejects_invalid_variants_before_resolution() {
    for source in [
        json!({"type":"pull_request"}),
        json!({"type":"tag"}),
        json!({"type":"tag","tag":"v1","sha":"a".repeat(40)}),
        json!({"type":"unknown","url":"https://github.com/cashubtc/cdk/pull/123"}),
    ] {
        let value = json!({"candidate_id":"invalid","implementation":"cdk","request_id":"invalid","source":source});
        assert!(serde_json::from_value::<CandidateBuildRequest>(value).is_err());
    }
    for source in [
        json!({"type":"commit"}),
        json!({"type":"commit","sha":"a".repeat(40),"url":format!("https://github.com/cashubtc/cdk/commit/{}", "a".repeat(40))}),
        json!({"type":"commit","sha":"abc123"}),
        json!({"type":"pull_request","url":"https://github.com/other/repo/pull/123"}),
        json!({"type":"tag","tag":"bad tag"}),
    ] {
        let store = store();
        let request = serde_json::from_value(json!({"candidate_id":"invalid","implementation":"cdk","request_id":"invalid","source":source})).unwrap();
        let result = admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
            panic!("invalid source must fail before resolution")
        })
        .await;
        assert!(result.is_err());
        assert!(
            store
                .candidate_builds("workspace", "agent")
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn candidate_source_failure_is_recorded_and_never_resolved_again_on_replay() {
    let store = store();
    let mut request = request("cdk", "missing-tag");
    request.source = Some(CandidateInput::Tag {
        tag: "missing".into(),
    });
    let failed = admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
        Err(Error::problem(
            "candidate_source_resolution_failed",
            "GitHub returned 404",
        ))
    })
    .await
    .unwrap();
    assert_eq!(failed.phase, CandidateBuildPhase::Failed);
    assert!(failed.commit_sha.is_none() && failed.repository.is_none() && failed.image.is_none());
    assert_eq!(failed.error_message.as_deref(), Some("GitHub returned 404"));
    assert!(failed.completed_at_unix.is_some());
    for key in ["missing-tag", "same-missing-tag"] {
        request.idempotency_key = key.into();
        let replay = admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
            panic!("a recorded source failure must not re-resolve")
        })
        .await
        .unwrap();
        assert_eq!(replay, failed);
    }
    assert!(
        store
            .effective_catalog("workspace", "agent")
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.source.is_none())
    );
}

#[tokio::test]
async fn candidate_concurrent_admission_freezes_the_first_resolved_head() {
    let store = store();
    let mut request = request("cdk", "moving-pr");
    request.source = Some(CandidateInput::PullRequest {
        url: "https://github.com/cashubtc/cdk/pull/123".into(),
    });
    let barrier = tokio::sync::Barrier::new(2);
    let (left, right) = tokio::join!(
        admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
            barrier.wait().await;
            Ok((
                "https://github.com/contributor/cdk.git".into(),
                "a".repeat(40),
            ))
        }),
        admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
            barrier.wait().await;
            Ok((
                "https://github.com/contributor/cdk.git".into(),
                "b".repeat(40),
            ))
        })
    );
    assert_eq!(left.unwrap(), right.unwrap());
    assert_eq!(
        store.candidate_builds("workspace", "agent").unwrap().len(),
        1
    );
}

#[tokio::test]
async fn candidate_legacy_replay_keeps_exact_record_without_fabricating_provenance() {
    let store = store();
    let mut legacy: CandidateBuild = serde_json::from_value(json!({
        "api_version":proofstorm_core::CANDIDATE_BUILD_API_VERSION,"id":"legacy","workspace_id":"workspace","principal_id":"agent",
        "implementation":"cdk","base_version":"0.18.0","pull_request_url":"https://github.com/cashubtc/cdk/pull/123/",
        "resource_name":"candidate-legacy","request_digest":"legacy-fingerprint","phase":"pending","accepted_at_unix":1,
        "repository":"https://github.com/cashubtc/cdk.git","commit_sha":"a".repeat(40),"version":"candidate-pr123-aaaaaaaa"
    })).unwrap();
    store
        .create_candidate_build("workspace", "agent", &legacy, "old-key")
        .unwrap();
    legacy = succeed(&store, legacy);
    let before = serde_json::to_string(&legacy).unwrap();
    for key in ["old-key", "new-key"] {
        let request = serde_json::from_value(json!({"implementation":"cdk","candidate_id":"legacy","pull_request_url":"https://github.com/cashubtc/cdk/pull/123","request_id":key})).unwrap();
        let replay = admit_with_resolver(&store, "workspace", "agent", &request, |_, _| async {
            panic!("legacy replay must not resolve the current PR")
        })
        .await
        .unwrap();
        assert_eq!(serde_json::to_string(&replay).unwrap(), before);
        assert!(replay.provenance.is_none());
    }
    let images = crate::catalog::read(
        &store,
        "workspace",
        "agent",
        &serde_json::from_value(json!({"origins":["candidate"]})).unwrap(),
        32 * 1024,
    )
    .unwrap();
    assert!(images.items[0]["platform"].is_null());
}

#[tokio::test]
async fn candidate_profiles_admit_every_catalog_mint_and_wallet_and_preserve_replays() {
    let store = store();
    let cashu = default_catalog()
        .entries
        .iter()
        .filter(|e| matches!(e.kind, ComponentKind::Mint | ComponentKind::Wallet))
        .collect::<Vec<_>>();
    assert_eq!(cashu.len(), 7);
    for entry in cashu {
        let request = request(&entry.id, &format!("{}-source", entry.id));
        let candidate = admit(&store, "workspace", "agent", &request).await.unwrap();
        let provenance = candidate.provenance.as_ref().unwrap();
        assert_eq!(provenance.baseline_digest, digest_json(entry));
        assert_eq!(provenance.profile_digest, provenance.profile.digest());
        let succeeded = succeed(&store, candidate);
        assert_eq!(
            admit(&store, "workspace", "agent", &request).await.unwrap(),
            succeeded
        );
        let mut duplicate = request.clone();
        duplicate.idempotency_key.push_str("-replay");
        assert_eq!(
            admit(&store, "workspace", "agent", &duplicate)
                .await
                .unwrap(),
            succeeded
        );
        duplicate.source = Some(CandidateInput::Commit {
            sha: Some("c".repeat(40)),
            url: None,
        });
        assert!(
            admit(&store, "workspace", "agent", &duplicate)
                .await
                .is_err()
        );
    }
    let catalog = store.effective_catalog("workspace", "agent").unwrap();
    assert_eq!(
        catalog
            .entries
            .iter()
            .filter(|e| e.source.is_some())
            .count(),
        13
    );
    let page = crate::catalog::list(
        &catalog,
        &serde_json::from_value::<CatalogListRequest>(
            json!({"query":"cdk","kinds":["mint"],"origins":["candidate"]}),
        )
        .unwrap(),
        "linux/arm64",
        32 * 1024,
    )
    .unwrap();
    assert_eq!(page.matched_count, 9);
    assert!(
        store
            .effective_catalog("workspace", "reader")
            .unwrap()
            .entries
            .iter()
            .all(|e| e.source.is_none())
    );
    assert!(store.candidate_builds("workspace", "reader").is_err());
}

#[tokio::test]
async fn candidate_request_collision_rolls_back_and_terminal_provenance_is_immutable() {
    let store = store();
    let request = request("cocod-wallet", "coco-first");
    let candidate = admit(&store, "workspace", "agent", &request).await.unwrap();
    let mut conflict = request.clone();
    conflict.candidate_id = "coco-second".into();
    assert!(
        admit(&store, "workspace", "agent", &conflict)
            .await
            .is_err()
    );
    assert_eq!(
        store.candidate_builds("workspace", "agent").unwrap().len(),
        1
    );
    let mut changed = candidate.clone();
    changed.build_features.clear();
    assert!(store.update_candidate_build("workspace", &changed).is_err());
    let succeeded = succeed(&store, candidate);
    let mut changed = succeeded.clone();
    changed.phase = CandidateBuildPhase::Failed;
    assert!(store.update_candidate_build("workspace", &changed).is_err());
    let mut changed = succeeded;
    changed
        .provenance
        .as_mut()
        .unwrap()
        .profile
        .notes
        .push("changed".into());
    assert!(store.update_candidate_build("workspace", &changed).is_err());
}

#[tokio::test]
async fn candidate_directory_and_diagnostics_are_passive_bounded_and_snapshot_bound() {
    let store = store();
    for id in ["first", "second", "third"] {
        let mut candidate = admit(&store, "workspace", "agent", &request("cdk-cli-wallet", id))
            .await
            .unwrap();
        candidate.phase = CandidateBuildPhase::Failed;
        candidate.diagnostics = Some(CandidateDiagnostics {
            captured_at_unix: 1,
            logs: std::collections::BTreeMap::from([(
                "source".into(),
                CandidateLog {
                    text: "\"🦀\n".repeat(4000),
                    truncated: true,
                    unavailable: None,
                },
            )]),
        });
        candidate.error_code = Some("build_failed".into());
        store
            .update_candidate_build("workspace", &candidate)
            .unwrap();
    }
    let mut query = DirectoryQuery {
        limit: 1,
        phase: Some("failed".into()),
        ..Default::default()
    };
    let first = directory(&store, "workspace", "agent", &query, 32 * 1024).unwrap();
    assert_eq!(first["items"][0]["id"], "first");
    query.cursor = first["next_cursor"].as_str().map(str::to_owned);
    let second = directory(&store, "workspace", "agent", &query, 32 * 1024).unwrap();
    assert_eq!(second["items"][0]["id"], "second");
    admit(&store, "workspace", "agent", &request("cdk", "fourth"))
        .await
        .unwrap();
    assert!(directory(&store, "workspace", "agent", &query, 32 * 1024).is_err());
    let mut query = CandidateReadQuery {
        id: "first".into(),
        path: "/diagnostics/logs/source/text".into(),
        ..Default::default()
    };
    let mut text = String::new();
    loop {
        let page = read(&store, "workspace", "agent", &query).unwrap();
        assert!(
            serde_json::to_vec(&rmcp::model::CallToolResult::structured(page.clone()))
                .unwrap()
                .len()
                <= 32 * 1024
        );
        text.push_str(page["text"].as_str().unwrap());
        query.expected_digest = page["digest"].as_str().map(str::to_owned);
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        query.offset = usize::try_from(next).unwrap();
    }
    assert_eq!(text, "\"🦀\n".repeat(4000));
}

#[tokio::test]
async fn candidate_one_cdk_build_supplies_all_presets_without_expanding_old_builds() {
    for implementation in ["cdk", "cdk-ldk", "cdk-bdk"] {
        let store = store();
        let pending = admit(
            &store,
            "workspace",
            "agent",
            &request(implementation, "shared-cdk"),
        )
        .await
        .unwrap();
        assert!(
            store
                .effective_catalog("workspace", "agent")
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.source.is_none())
        );
        let candidate = succeed(&store, pending);
        let receipt = receipt(&candidate, false);
        assert_eq!(receipt.catalog_entry.implementation, implementation);
        assert_eq!(receipt.catalog_entries.len(), 3);
        assert_eq!(
            store.candidate_builds("workspace", "agent").unwrap().len(),
            1
        );
        let catalog = store.effective_catalog("workspace", "agent").unwrap();
        for preset in ["cdk", "cdk-bdk", "cdk-ldk"] {
            let page = crate::catalog::list(
                &catalog,
                &serde_json::from_value(
                    json!({"kinds":["mint"],"implementations":[preset],"origins":["candidate"]}),
                )
                .unwrap(),
                "linux/arm64",
                32 * 1024,
            )
            .unwrap();
            assert_eq!(page.matched_count, 1);
            assert_eq!(page.items[0]["candidate_id"], "shared-cdk");
            assert_eq!(page.items[0]["image"], candidate.image.as_deref().unwrap());
            assert_eq!(
                page.items[0]["shared_image_implementations"],
                json!(["cdk", "cdk-bdk", "cdk-ldk"])
            );
            let base = default_catalog()
                .entries
                .iter()
                .find(|entry| entry.id == preset)
                .unwrap();
            let selected = catalog
                .entries
                .iter()
                .find(|entry| entry.id == preset && entry.source.is_some())
                .unwrap();
            assert_eq!(selected.config_version, base.config_version);
            assert_eq!(selected.support_matrix, base.support_matrix);
            assert_eq!(selected.source, candidate.source());
            assert_eq!(selected.image, candidate.image.as_deref().unwrap());
            assert_eq!(
                selected.support_lifecycle,
                proofstorm_core::SupportLifecycle::Experimental
            );
            assert_eq!(base.image, proofstorm_core::CDK_MINT_IMAGE);
            assert_eq!(
                base.build_provenance,
                default_catalog()
                    .entries
                    .iter()
                    .find(|entry| entry.id == "cdk-ldk")
                    .unwrap()
                    .build_provenance
            );
        }
        assert_historical_cdk_scope(&candidate);
        let mut invalid = candidate.provenance.unwrap();
        invalid
            .profile
            .catalog_implementations
            .insert("nutshell".into());
        invalid.profile_digest = invalid.profile.digest();
        assert!(invalid.validate().is_err());
    }
}

fn assert_historical_cdk_scope(candidate: &CandidateBuild) {
    let implementation = candidate.implementation.as_str();
    let mut historical = candidate.clone();
    let saved = historical.provenance.as_mut().unwrap();
    saved.profile.catalog_implementations.clear();
    saved.profile.id = format!("{implementation}-source");
    saved.profile.version = 4;
    saved.profile_digest = saved.profile.digest();
    let encoded = serde_json::to_string(&historical).unwrap();
    assert!(!encoded.contains("catalog_implementations"));
    let decoded: CandidateBuild = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, historical);
    for old in [
        decoded,
        CandidateBuild {
            provenance: None,
            ..historical
        },
    ] {
        let old_catalog =
            proofstorm_core::effective_catalog(default_catalog(), &[old.clone()]).unwrap();
        assert_eq!(
            old_catalog
                .entries
                .iter()
                .filter(|entry| entry.source.is_some())
                .count(),
            1
        );
        let other = default_catalog()
            .entries
            .iter()
            .find(|entry| {
                entry.id
                    == if implementation == "cdk" {
                        "cdk-ldk"
                    } else {
                        "cdk"
                    }
            })
            .unwrap();
        assert!(proofstorm_core::candidate_catalog_entry(other, &old).is_err());
    }
}
