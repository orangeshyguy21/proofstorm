//! An intentionally unavailable catalog image in the gate's private database.
//! This is synthetic test data, not a record of a real source build.
use crate::{GateContext, McpClient};
use anyhow::{Result, ensure};
use proofstorm_core::{CandidateBuild, CandidateBuildPhase, Capability};
use serde_json::json;

pub fn check(
    context: &GateContext,
    client: &mut McpClient,
    directory: &std::path::Path,
) -> Result<()> {
    let workspace = format!("dynamic-{}", context.run_id);
    let store = proofstorm_store::Store::open(context.database())?;
    store.grant(&workspace, "agent", Capability::CandidateBuild)?;
    let fixture = CandidateBuild {
        api_version: proofstorm_core::CANDIDATE_BUILD_API_VERSION.into(),
        id: "missing-image-test-fixture".into(),
        workspace_id: workspace.clone(),
        principal_id: "agent".into(),
        implementation: "nutshell".into(),
        base_version: "0.20.3".into(),
        pull_request_url: "https://github.com/cashubtc/nutshell/pull/1".into(),
        resource_name: "synthetic-missing-image-fixture".into(),
        request_digest: "sha256:test-fixture".into(),
        phase: CandidateBuildPhase::Pending,
        accepted_at_unix: 1,
        started_at_unix: None,
        completed_at_unix: None,
        repository: Some("https://github.com/cashubtc/nutshell.git".into()),
        commit_sha: Some("1".repeat(40)),
        version: Some("candidate-test-missing-image".into()),
        image: None,
        error_code: None,
        error_message: None,
    };
    store.create_candidate_build(&workspace, "agent", &fixture, "missing-image-fixture")?;
    let fixture = CandidateBuild {
        phase: CandidateBuildPhase::Succeeded,
        started_at_unix: Some(1),
        completed_at_unix: Some(2),
        image: Some(format!(
            "proofstorm-registry.localhost:5000/missing-test-fixture@sha256:{}",
            "1".repeat(64)
        )),
        ..fixture
    };
    store.update_candidate_build(&workspace, &fixture)?;
    let (mut components, mut connections) = super::topology();
    components.push(json!({"id":"blocked-mint","implementation":"nutshell","version":"candidate-test-missing-image"}));
    connections.push(json!({"id":"blocked-backend","kind":"payment_backend","mint":"blocked-mint","lightning":"mint-lnd"}));
    let plan = super::plan(
        client,
        "failed-addition",
        &components,
        &connections,
        &super::target(5),
    )?;
    ensure!(
        plan["update"]["changes"]["restarted"] == json!([]),
        "failed addition would restart existing workloads"
    );
    super::apply(client, &plan, "failed-addition-apply")?;
    let status = client.call("lab_wait", json!({"instance_id":super::INSTANCE,"target_phase":"ready","expected_generation":6,"timeout_seconds":120}))?;
    ensure!(
        status["reached"] == false && status["timed_out"] == false,
        "missing image did not end the wait with a blocker: {status}"
    );
    ensure!(
        status["blockers"]
            .as_array()
            .is_some_and(|items| items.iter().any(|b| b["component_id"] == "blocked-mint")),
        "missing image blocker was not attributed: {status}"
    );
    ensure!(
        super::balance(client, "balance-while-blocked")? == 1000,
        "existing wallet unusable during failed addition"
    );
    std::fs::write(
        directory.join("failed-addition.json"),
        serde_json::to_vec_pretty(&status)?,
    )?;
    Ok(())
}
