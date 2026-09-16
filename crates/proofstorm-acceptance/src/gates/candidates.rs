//! Public managed-MCP source builds. Every successful image comes from a real Job.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{GateContext, McpClient, cell, gate::CONTROL_NAMESPACE, json as expect};

const CDK: &str = "d3dec24c784e8fec1fd65f853241c7a2261c7abd";
pub(super) const COCO: &str = "44e5101cbea370132af6e68f88e01b47e39431c4";
const NUTSHELL: &str = "18539020b4fa473ad8ad440e210720d2aaf8401a";
const COCO_PR: &str = "https://github.com/cashubtc/coco/pull/460";

pub(super) fn pod_image(context: &GateContext, namespace: &str, image: &str) -> Result<()> {
    let pods = context
        .kubectl
        .get_json(&["get", "pods", "-n", namespace])?;
    ensure!(
        expect::array(&pods, "/items")?
            .iter()
            .any(|pod| pod["spec"]["containers"]
                .as_array()
                .is_some_and(|containers| containers
                    .iter()
                    .any(|container| container["image"] == image))),
        "ready cell did not deploy the exact selected image {image}"
    );
    context.record(&format!("{namespace}-selected-pods.json"), &pods)
}

fn startup_diagnostics(
    context: &GateContext,
    client: &mut McpClient,
    name: &str,
    prefix: &str,
) -> Result<()> {
    let status = cell::status(client, name)?;
    context.record(&format!("{prefix}-failed-status.json"), &status)?;
    let namespace = expect::string(&status, "/instance_namespace")?;
    let pods = context
        .kubectl
        .get_json(&["get", "pods", "-n", namespace])?;
    context.record(&format!("{prefix}-failed-pods.json"), &pods)?;
    // Readiness command failures live in events, not the daemon's stdout.
    let (ok, stdout, stderr) = context.kubectl.try_run(&[
        "get",
        "events",
        "-n",
        namespace,
        "--field-selector=type=Warning",
        "-o",
        "json",
    ])?;
    context.record(
        &format!("{prefix}-startup-warnings.json"),
        &json!({"available":ok,"stdout":stdout,"stderr":stderr}),
    )?;
    for pod in expect::array(&pods, "/items")?.iter().take(16) {
        let pod_name = expect::string(pod, "/metadata/name")?;
        let (ok, stdout, stderr) = context.kubectl.try_run(&[
            "logs",
            pod_name,
            "-n",
            namespace,
            "--all-containers=true",
            "--tail=100",
            "--prefix=true",
        ])?;
        context.record(
            &format!("{prefix}-{pod_name}-startup.json"),
            &json!({"available":ok,"stdout":stdout,"stderr":stderr}),
        )?;
    }
    Ok(())
}

/// Reassemble the public, digest-bound resource pages, including archived logs.
fn resource(client: &mut McpClient, initial: &str) -> Result<Value> {
    let mut uri = initial.to_owned();
    let mut text = String::new();
    let mut digest = None;
    for _ in 0..256 {
        let response = client.request("resources/read", json!({"uri":uri}))?;
        let page: Value = serde_json::from_str(expect::string(&response, "/contents/0/text")?)?;
        let current = expect::string(&page, "/digest")?.to_owned();
        ensure!(
            digest.as_ref().is_none_or(|value| value == &current),
            "resource changed between pages"
        );
        digest = Some(current);
        ensure!(
            page["offset"].as_u64() == Some(text.chars().count() as u64),
            "resource page skipped content"
        );
        text.push_str(expect::string(&page, "/text")?);
        if let Some(next) = page["next_uri"].as_str() {
            next.clone_into(&mut uri);
        } else {
            return Ok(serde_json::from_str(&text)?);
        }
    }
    anyhow::bail!("resource exceeded the bounded acceptance read");
}

pub(super) fn wait(context: &GateContext, client: &mut McpClient, id: &str) -> Result<Value> {
    for _ in 0..32 {
        let receipt = client.call(
            "candidate_wait",
            json!({"candidate_id":id,"timeout_seconds":120}),
        )?;
        context.record(&format!("{id}-receipt.json"), &receipt)?;
        eprintln!("candidate {id}: {}", receipt["phase"]);
        if matches!(
            receipt["phase"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            let record = resource(client, expect::string(&receipt, "/record_resource_uri")?)?;
            context.record(&format!("{id}-record.json"), &record)?;
            let logs = resource(client, expect::string(&receipt, "/logs_resource_uri")?)?;
            context.record(&format!("{id}-logs.json"), &logs)?;
            return Ok(receipt);
        }
    }
    anyhow::bail!("candidate {id} did not reach a terminal receipt");
}

fn source_cases(context: &GateContext, client: &mut McpClient) -> Result<()> {
    let id = "acceptance-coco-pr";
    let request = json!({"candidate_id":id,"implementation":"cocod-wallet","request_id":id,
        "source":{"type":"pull_request","url":COCO_PR}});
    let admitted = client.call("candidate_build", request.clone())?;
    context.record("candidate-real-pr-admission.json", &admitted)?;
    ensure!(
        admitted["source_resolved"] == true
            && admitted["commit_sha"] == "4936bdcafd88da3d6ad01b513b183833a853dc30",
        "public PR did not resolve to a commit: {admitted}"
    );
    let mut retry = request;
    retry["request_id"] = json!("acceptance-coco-pr-retry");
    let replay = client.call("candidate_build", retry)?;
    ensure!(
        replay["commit_sha"] == admitted["commit_sha"]
            && replay["catalog_entry"] == admitted["catalog_entry"],
        "retry changed the frozen PR identity"
    );
    client.call("candidate_cancel", json!({"candidate_id":id}))?;
    ensure!(
        wait(context, client, id)?["phase"] == "cancelled",
        "cancelled PR build did not terminate"
    );
    let missing = "acceptance-missing-commit";
    client.call(
        "candidate_build",
        json!({"candidate_id":missing,"implementation":"cocod-wallet","request_id":missing,
        "source":{"type":"commit","sha":"0000000000000000000000000000000000000001"}}),
    )?;
    let failed = wait(context, client, missing)?;
    ensure!(
        failed["phase"] == "failed" && failed["image"].is_null(),
        "missing source was not a build failure: {failed}"
    );
    let catalog = client.call(
        "catalog_list",
        json!({"origins":["candidate"],"query":"acceptance-"}),
    )?;
    ensure!(
        expect::array(&catalog, "/items")?.is_empty(),
        "failed/cancelled attempts entered the image catalog"
    );
    Ok(())
}

fn active_build_drain(context: &GateContext, client: &mut McpClient) -> Result<()> {
    let stopped = crate::process::capture(context.command(&["stop", "--timeout", "1"])?, 60)?;
    let result = (|| -> Result<()> {
        ensure!(
            !stopped.status.success(),
            "stop ignored the active candidate build"
        );
        let refusal = client.call_error(
            "candidate_build",
            json!({"candidate_id":"acceptance-during-drain","implementation":"cocod-wallet",
            "request_id":"acceptance-during-drain","source":{"type":"commit","sha":COCO}}),
        )?;
        ensure!(
            refusal["data"]["code"] == "runtime_suspended",
            "build was not refused during drain: {refusal}"
        );
        let listed = client.call("candidate_list", json!({"id":"acceptance-during-drain"}))?;
        ensure!(
            expect::array(&listed, "/items")?.is_empty(),
            "draining installation admitted a build record"
        );
        context.record(
            "candidate-active-drain.json",
            &json!({"stop_incomplete":true,"new_build_refused":true,"refusal":refusal}),
        )
    })();
    let resumed = context.cli(&["start"])?;
    ensure!(
        resumed["ready"] == true,
        "could not resume after incomplete stop"
    );
    result
}

fn suspension(
    context: &GateContext,
    client: &mut McpClient,
    name: &str,
    receipt: &Value,
) -> Result<()> {
    let uri = expect::string(receipt, "/record_resource_uri")?;
    let record = resource(client, uri)?;
    let before = cell::status(client, name)?;
    for cycle in 0..2 {
        eprintln!("candidate installation stop/start cycle {}", cycle + 1);
        ensure!(
            context.cli(&["stop"])?["state"] == "stopped",
            "candidate installation did not stop"
        );
        ensure!(
            resource(client, uri)? == record,
            "stopped recorded build changed"
        );
        let catalog = client.call(
            "catalog_list",
            json!({"origins":["candidate"],"implementations":["cocod-wallet"]}),
        )?;
        ensure!(
            expect::array(&catalog, "/items")?
                .iter()
                .any(|entry| entry["version"] == receipt["catalog_entry"]["version"]),
            "stopped catalog lost the candidate"
        );
        ensure!(
            context.cli(&["start"])?["ready"] == true,
            "candidate installation did not restart"
        );
        let after = cell::wait_ready(client, name)?;
        ensure!(
            before["instance_key"] == after["instance_key"] && resource(client, uri)? == record,
            "restart changed cell/build identity"
        );
        pod_image(
            context,
            expect::string(&after, "/instance_namespace")?,
            expect::string(receipt, "/image")?,
        )?;
        context.record(
            &format!("candidate-suspension-{cycle}.json"),
            &json!({"passed":true,"image":receipt["image"],"instance_key":after["instance_key"]}),
        )?;
    }
    Ok(())
}

pub(super) fn retained_logs(
    context: &GateContext,
    client: &mut McpClient,
    receipt: &Value,
) -> Result<()> {
    let id = expect::string(receipt, "/candidate_id")?;
    let uri = expect::string(receipt, "/logs_resource_uri")?;
    let before = resource(client, uri)?;
    ensure!(
        expect::string(&before, "/logs/buildkit/text")?.contains("exporting manifest sha256:"),
        "retained successful build log omitted terminal image export"
    );
    let builds =
        context
            .kubectl
            .get_json(&["get", "proofstormcandidatebuilds", "-n", CONTROL_NAMESPACE])?;
    let build = expect::array(&builds, "/items")?
        .iter()
        .find(|build| build["spec"]["candidateId"] == id)
        .context("candidate runtime record missing")?;
    let job = expect::string(build, "/status/jobName")?;
    context.kubectl.run(&[
        "delete",
        "job",
        job,
        "-n",
        CONTROL_NAMESPACE,
        "--ignore-not-found",
        "--wait=true",
        "--timeout=60s",
    ])?;
    ensure!(
        resource(client, uri)? == before,
        "archived logs changed after Job cleanup"
    );
    ensure!(
        client.call(
            "candidate_wait",
            json!({"candidate_id":id,"timeout_seconds":1})
        )?["image"]
            == receipt["image"],
        "Job cleanup changed the terminal image"
    );
    context.record(
        &format!("{id}-job-cleanup.json"),
        &json!({"job":job,"archived_logs_retained":true}),
    )
}

fn wallet_cell(context: &GateContext, client: &mut McpClient, receipt: &Value) -> Result<()> {
    let implementation = expect::string(receipt, "/catalog_entry/implementation")?;
    let coco = implementation == "cocod-wallet";
    let (name, run, mut document) = if coco {
        (
            "cocod-wallet-instance",
            "cocod-wallet-experiment",
            super::cocod_wallet::document(),
        )
    } else {
        (
            "cdk-wallet-instance",
            "cdk-wallet-experiment",
            super::cdk_wallet::document(0),
        )
    };
    let component = if matches!(implementation, "cdk" | "nutshell") {
        3
    } else {
        4
    };
    let component_id = if component == 3 { "mint" } else { "wallet-a" };
    document["components"][component]["version"] = receipt["catalog_entry"]["version"].clone();
    if implementation == "nutshell" {
        document["components"][component]["implementation"] = json!(implementation);
        document["components"][component]["config_version"] = json!("nutshell-mint/0.20/v1");
    } else if implementation == "nutshell-wallet" {
        document["components"][component]["implementation"] = json!(implementation);
        document["components"][component]["config_version"] = json!("nutshell-wallet/0.20/v1");
    }
    let prefix = format!("candidate-{implementation}");
    let preview = client.call(
        "cell_plan",
        json!({"name":name,"cell":document,"request_id":"candidate-cell-plan"}),
    )?;
    let reviewed = cell::review(client, &preview)?;
    context.record(&format!("{prefix}-plan.json"), &reviewed)?;
    let lock = expect::array(&reviewed, "/lock/entries")?
        .iter()
        .find(|entry| entry["component_id"] == component_id)
        .context("candidate wallet lock missing")?;
    ensure!(
        lock["image"] == receipt["image"] && lock["source"]["commit_sha"] == receipt["commit_sha"],
        "saved plan lost candidate image/source: {lock}"
    );
    let accepted = cell::apply(client, &preview)?;
    let result = (|| -> Result<()> {
        let ready = cell::wait_ready(client, name)?;
        context.record(&format!("{prefix}-ready.json"), &ready)?;
        let namespace = expect::string(&ready, "/instance_namespace")?;
        let pods = context
            .kubectl
            .get_json(&["get", "pods", "-n", namespace])?;
        ensure!(
            expect::array(&pods, "/items")?
                .iter()
                .any(|pod| pod["spec"]["containers"]
                    .as_array()
                    .is_some_and(|containers| containers
                        .iter()
                        .any(|container| container["image"] == receipt["image"]))),
            "ready cell did not deploy the exact candidate image"
        );
        context.record(&format!("{prefix}-pods.json"), &pods)?;
        client.call(
            "run_start",
            json!({"name":name,"run_id":run,"request_id":"candidate-run-start"}),
        )?;
        let directory = context.work().join(format!("{prefix}-payment"));
        std::fs::create_dir(&directory)?;
        if coco {
            super::cocod_wallet::exercise(context, client, &directory, namespace)?;
            suspension(context, client, name, receipt)?;
        } else if implementation == "nutshell-wallet" {
            super::candidates_nutshell::exercise(context, client)?;
        } else {
            super::cdk_wallet::exercise(context, client, &directory, namespace, 0)?;
        }
        client.call(
            "run_finish",
            json!({"run_id":run,"request_id":"candidate-run-finish"}),
        )?;
        let evidence = cell::evidence(client, json!({"run_id":run}))?;
        context.record(&format!("{prefix}-evidence.json"), &evidence)?;
        let exported = expect::array(&evidence, "/content/revision/lock/entries")?
            .iter()
            .find(|entry| entry["component_id"] == component_id)
            .context("exported candidate lock missing")?;
        ensure!(
            exported["image"] == receipt["image"]
                && exported["source"]["commit_sha"] == receipt["commit_sha"]
                && exported["source"]["provenance"] == lock["source"]["provenance"],
            "exported experiment lost frozen candidate evidence"
        );
        Ok(())
    })();
    if result.is_err() {
        let diagnostics = startup_diagnostics(context, client, name, &prefix);
        if let Err(error) = diagnostics {
            context.record(
                &format!("{prefix}-diagnostics-error.json"),
                &json!({"error":format!("{error:#}")}),
            )?;
        }
    }
    context.record(&format!("{prefix}-cell-outcome.json"), &json!({"passed":result.is_ok(),"error":result.as_ref().err().map(|error|format!("{error:#}"))}))?;
    let removed = client.call(
        "cell_remove",
        json!({"name":name,"expected_instance_key":accepted["instance_key"],"timeout_seconds":120}),
    );
    if removed.is_ok() {
        context.record(
            &format!("{prefix}-closed.json"),
            &cell::wait_closed(client, name)?,
        )?;
    }
    result?;
    removed?;
    Ok(())
}

fn build(context: &GateContext, client: &mut McpClient, implementation: &str) -> Result<Value> {
    if implementation == "cocod-wallet" {
        source_cases(context, client)?;
    }
    let sha = match implementation {
        "cocod-wallet" => COCO,
        "nutshell" | "nutshell-wallet" => NUTSHELL,
        _ => CDK,
    };
    let id = format!("acceptance-{implementation}");
    let source = if implementation == "nutshell-wallet" {
        json!({"type":"tag","tag":"0.20.3"})
    } else {
        json!({"type":"commit","sha":sha})
    };
    let request =
        json!({"candidate_id":id,"implementation":implementation,"request_id":id,"source":source});
    context.record(&format!("{id}-request.json"), &request)?;
    client.call("candidate_build", request)?;
    if implementation == "cocod-wallet" {
        active_build_drain(context, client)?;
    }
    let receipt = wait(context, client, &id)?;
    ensure!(
        receipt["phase"] == "succeeded",
        "candidate build failed: {receipt}"
    );
    ensure!(
        receipt["commit_sha"] == sha && expect::string(&receipt, "/image")?.contains("@sha256:"),
        "build did not retain frozen source and immutable image"
    );
    let catalog = client.call(
        "catalog_list",
        json!({"implementations":[implementation],"origins":["candidate"]}),
    )?;
    ensure!(
        expect::array(&catalog, "/items")?
            .iter()
            .any(
                |entry| entry["version"] == receipt["catalog_entry"]["version"]
                    && entry["origin"] == "candidate"
            ),
        "successful build is absent from the filtered catalog: {catalog}"
    );
    context.record(&format!("{id}-catalog.json"), &catalog)?;
    let entry = client.call(
        "catalog_entry_read",
        json!({"id":implementation,"version":receipt["catalog_entry"]["version"]}),
    )?;
    context.record(&format!("{id}-entry.json"), &entry)?;
    retained_logs(context, client, &receipt)?;
    Ok(receipt)
}

pub fn run(context: &GateContext, implementation: &str) -> Result<()> {
    let mut client = context.managed_session(&format!("candidate-{implementation}"))?;
    let receipt = build(context, &mut client, implementation)?;
    let payment = matches!(
        implementation,
        "cocod-wallet" | "cdk-cli-wallet" | "nutshell-wallet" | "cdk" | "nutshell"
    );
    if payment {
        wallet_cell(context, &mut client, &receipt)?;
    } else if implementation == "cdk-ldk" {
        super::cdk_ldk::run_candidate(context, client, &receipt)?;
    } else if implementation == "cdk-bdk" {
        super::cdk_bdk_stress::run_candidate(context, client, &receipt)?;
    }
    context.record(
        &format!("acceptance-{implementation}-outcome.json"),
        &json!({"build_catalog_retention_passed":true,
        "payment_exercised":payment,"image":receipt["image"],"commit_sha":receipt["commit_sha"]}),
    )
}

/// One real source build, three independently exercised runtime presets.
pub fn run_cdk_modes(context: &GateContext) -> Result<()> {
    let mut client = context.managed_session("candidate-cdk-modes")?;
    let receipt = build(context, &mut client, "cdk")?;
    ensure!(
        expect::array(&receipt, "/catalog_entries")?.len() == 3,
        "shared build did not advertise three presets"
    );
    for implementation in ["cdk", "cdk-bdk", "cdk-ldk"] {
        let entry = client.call(
            "catalog_entry_read",
            json!({"id":implementation,"version":receipt["catalog_entry"]["version"]}),
        )?;
        context.record(
            &format!("candidate-cdk-shared-{implementation}.json"),
            &entry,
        )?;
        // The full entry resource preserves the image and frozen source for each preset.
        let catalog = client.call(
            "catalog_list",
            json!({"implementations":[implementation],"origins":["candidate"]}),
        )?;
        ensure!(
            expect::array(&catalog, "/items")?
                .iter()
                .any(|entry| entry["image"] == receipt["image"]
                    && entry["candidate_id"] == receipt["candidate_id"]),
            "preset does not reuse the shared image"
        );
    }
    let builds = client.call("candidate_list", json!({"id":"acceptance-cdk"}))?;
    ensure!(
        expect::array(&builds, "/items")?.len() == 1,
        "expected one CDK build record"
    );
    context.record("candidate-cdk-shared-build.json", &builds)?;
    wallet_cell(context, &mut client, &receipt)?;
    super::cdk_ldk::run_candidate(context, client, &receipt)?;
    let client = context.managed_session("candidate-cdk-modes")?;
    super::cdk_bdk_stress::run_candidate(context, client, &receipt)?;
    context.record("candidate-cdk-modes-outcome.json", &json!({"passed":true,"build_count":1,"image":receipt["image"],"presets":["cdk","cdk-ldk","cdk-bdk"]}))
}
