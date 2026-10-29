use super::*;
use proofstorm_core::CandidateBuildPhase;
use serde_json::json;

fn store() -> Store {
    let store = Store::memory().unwrap();
    store
        .put_workspace(&Workspace {
            id: "workspace".into(),
            name: "workspace".into(),
        })
        .unwrap();
    for principal in ["agent", "reader"] {
        store.put_principal(principal).unwrap();
        store
            .grant("workspace", principal, Capability::CandidateRead)
            .unwrap();
    }
    for capability in [Capability::CandidateBuild, Capability::CandidateCancel] {
        store.grant("workspace", "agent", capability).unwrap();
    }
    store
}

fn candidate() -> CandidateBuild {
    serde_json::from_value(json!({
        "api_version": proofstorm_core::CANDIDATE_BUILD_API_VERSION, "id": "fix",
        "workspace_id": "workspace", "principal_id": "agent", "implementation": "nutshell",
        "base_version": "0.20.3", "pull_request_url": "https://github.com/cashubtc/nutshell/pull/1",
        "resource_name": "candidate-fix", "request_digest": "sha256:request", "phase": "pending",
        "accepted_at_unix": 1, "repository": "https://github.com/cashubtc/nutshell.git",
        "commit_sha": "a".repeat(40), "version": "candidate-fix"
    }))
    .unwrap()
}

/// A cell named `name` whose current revision locks the candidate `fix`.
fn cell_running_fix(store: &Store, name: &str) {
    let revision = json!({"lock": {"entries": [{"source": {"candidate_id": "fix"}}]}});
    let connection = store.lock().unwrap();
    connection
        .execute(
            "INSERT INTO revisions(digest,workspace_id,draft_id,draft_version,revision_json) VALUES ('rev','workspace','draft',1,?1)",
            [revision.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO instances(workspace_id,id,revision_digest,lock_digest,instance_key,resource_name) VALUES ('workspace','cell-1','rev','lock','key','resource')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cell_handles(workspace_id,name,generation,owner,config_digest,phase,instance_id) VALUES ('workspace',?1,1,'agent','config','\"open\"','cell-1')",
            [name],
        )
        .unwrap();
}

#[test]
fn candidate_removal_needs_a_finished_build_that_no_cell_runs() {
    let store = store();
    let created = store
        .create_candidate_build("workspace", "agent", &candidate(), "build-1")
        .unwrap();
    let removal = |principal| store.remove_candidate_build("workspace", principal, "fix");
    assert!(matches!(removal("agent"), Err(StoreError::Validation(_))));

    let mut finished = created;
    finished.phase = CandidateBuildPhase::Failed;
    finished.completed_at_unix = Some(2);
    store
        .update_candidate_build("workspace", &finished)
        .unwrap();
    cell_running_fix(&store, "payments");
    assert_eq!(
        store
            .candidate_build_users("workspace", "reader", "fix")
            .unwrap(),
        ["payments"]
    );
    assert!(matches!(removal("agent"), Err(StoreError::Validation(_))));
    store
        .lock()
        .unwrap()
        .execute("DELETE FROM instances", [])
        .unwrap();
    assert!(
        store
            .candidate_build_users("workspace", "agent", "fix")
            .unwrap()
            .is_empty()
    );

    assert!(matches!(
        removal("reader"),
        Err(StoreError::AccessDenied { .. })
    ));
    let generation = store
        .candidate_directory_generation("workspace", "agent")
        .unwrap();
    assert_eq!(removal("agent").unwrap(), finished);
    assert!(matches!(
        store.candidate_build("workspace", "agent", "fix"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(
        store
            .candidate_directory_generation("workspace", "agent")
            .unwrap()
            > generation
    );
    // The request replay goes too, so the same request_id builds afresh.
    let replays: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM idempotency WHERE operation='candidate.build'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replays, 0);
    assert!(matches!(removal("agent"), Err(StoreError::NotFound { .. })));
}
