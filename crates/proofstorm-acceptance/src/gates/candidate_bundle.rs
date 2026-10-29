//! Build a private commit from a git bundle, then remove it with its Secret.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{GateContext, gate::CONTROL_NAMESPACE, json as expect};

const ID: &str = "acceptance-bundle";
const REPOSITORY: &str = "https://github.com/cashubtc/cdk.git";
const ABOUT: &str = "A cashu mint written in rust";
const MARKER: &str = "proofstorm-bundle-marker";

/// Git without the operator's configuration: no URL rewrites, signing or hooks.
fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    // A fixed identity and date make the private commit reproducible.
    for role in ["AUTHOR", "COMMITTER"] {
        command
            .env(format!("GIT_{role}_NAME"), "Proofstorm acceptance")
            .env(format!("GIT_{role}_EMAIL"), "acceptance@proofstorm.invalid")
            .env(format!("GIT_{role}_DATE"), "2026-01-01T00:00:00Z");
    }
    let output = crate::process::capture(command, 300)?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// One unpublished commit on the pinned public CDK commit, as a thin bundle.
/// It marks cdk-mintd's help so the image shows which source it came from.
fn private_fix(context: &GateContext) -> Result<(String, String)> {
    let source = context.work().join("bundle-source");
    fs::create_dir(&source)?;
    git(&source, &["init", "--quiet"])?;
    git(
        &source,
        &[
            "fetch",
            "--quiet",
            "--depth=1",
            REPOSITORY,
            super::candidates::CDK,
        ],
    )?;
    git(&source, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
    let cli = source.join("crates/cdk-mintd/src/cli.rs");
    let text = fs::read_to_string(&cli)?;
    let about = format!("\"{ABOUT}\"");
    ensure!(
        text.matches(&about).count() == 1,
        "pinned CDK changed cdk-mintd's about text"
    );
    fs::write(
        &cli,
        text.replace(&about, &format!("\"{ABOUT} ({MARKER})\"")),
    )?;
    git(
        &source,
        &["commit", "--quiet", "--all", "--message", "Private fix"],
    )?;
    let commit = git(&source, &["rev-parse", "HEAD"])?;
    let bundle = context.work().join("fix.bundle");
    let path = bundle.to_str().context("work path is not UTF-8")?;
    let range = format!("{}..HEAD", super::candidates::CDK);
    git(&source, &["bundle", "create", "--quiet", path, &range])?;
    let path = fs::canonicalize(&bundle)?;
    Ok((
        path.to_str().context("work path is not UTF-8")?.to_owned(),
        commit,
    ))
}

fn docker(args: &[&str], seconds: u64) -> Result<Output> {
    let mut command = Command::new("docker");
    crate::client::clear_runtime_environment(&mut command);
    command.args(args);
    crate::process::capture(command, seconds)
}

/// Run the built cdk-mintd offline, pulled through the installation's host registry port.
fn help_text(context: &GateContext, image: &str) -> Result<String> {
    let (_, path) = image
        .split_once('/')
        .context("candidate image has no registry host")?;
    let local = format!("{}/{path}", context.installation.host_registry());
    let output = docker(
        &[
            "run",
            "--rm",
            "--network=none",
            "--entrypoint",
            "cdk-mintd",
            &local,
            "--help",
        ],
        600,
    );
    // Only this copy is removed; the registry keeps the image until teardown.
    let _ = docker(&["image", "rm", &local], 60);
    let output = output?;
    ensure!(
        output.status.success(),
        "candidate image did not run: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// Kubernetes garbage-collects the Job, Pod and bundle Secret after removal.
fn resources_gone(context: &GateContext, resource: &str) -> Result<()> {
    let selector = format!("{}={resource}", proofstorm_kube::CANDIDATE_BUILD_LABEL);
    let secret = proofstorm_kube::candidate_bundle_secret(resource);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let mut left = Vec::new();
        for args in [
            [
                "get",
                "proofstormcandidatebuild",
                resource,
                "--ignore-not-found",
            ],
            ["get", "secret", secret.as_str(), "--ignore-not-found"],
            ["get", "jobs,pods", "-l", selector.as_str()],
        ] {
            let mut args = args.to_vec();
            args.extend(["-n", CONTROL_NAMESPACE, "-o", "name"]);
            let found = context.kubectl.run(&args)?;
            left.extend(found.lines().map(str::to_owned));
        }
        if left.is_empty() {
            return context.record(
                "candidate-bundle-gone.json",
                &json!({"resource":resource,"secret":secret,"remaining":[]}),
            );
        }
        ensure!(
            Instant::now() < deadline,
            "removal left runtime resources: {left:?}"
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub(super) fn run(context: &GateContext) -> Result<()> {
    let mut client = context.managed_session("candidate-bundle")?;
    let (bundle, commit) = private_fix(context)?;
    let bytes = fs::read(&bundle)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let request = json!({"candidate_id":ID,"implementation":"cdk","request_id":ID,
        "source":{"type":"bundle","path":bundle,"commit":commit}});
    context.record(
        "candidate-bundle-request.json",
        &json!({"request":request,"bytes":bytes.len(),"sha256":sha256}),
    )?;
    let admitted = client.call("candidate_build", request)?;
    context.record("candidate-bundle-admission.json", &admitted)?;
    ensure!(
        admitted["source_resolved"] == true && admitted["commit_sha"] == commit.as_str(),
        "bundle admission did not freeze the private commit: {admitted}"
    );

    let (resource, pod) = super::candidate_isolation::build_pod(context, ID)?;
    let secret = proofstorm_kube::candidate_bundle_secret(&resource);
    super::candidate_isolation::check_mounts(context, "candidate-bundle", &pod, Some(&secret))?;
    // Metadata only; the Secret's data is the bundle this gate just wrote.
    let owners = context
        .kubectl
        .get_json(&["get", "secret", &secret, "-n", CONTROL_NAMESPACE])?["metadata"]
        ["ownerReferences"]
        .clone();
    context.record("candidate-bundle-secret-owners.json", &owners)?;
    ensure!(
        owners.as_array().is_some_and(|owners| owners.len() == 1
            && owners[0]["kind"] == "ProofstormCandidateBuild"
            && owners[0]["name"] == resource.as_str()),
        "bundle Secret is not owned by its build: {owners}"
    );
    client.call_refused(
        "candidate_remove",
        json!({"candidate_id":ID}),
        "candidate_active",
    )?;

    let receipt = super::candidates::wait(context, &mut client, ID)?;
    ensure!(
        receipt["phase"] == "succeeded" && receipt["commit_sha"] == commit.as_str(),
        "bundle build failed: {receipt}"
    );
    let image = expect::string(&receipt, "/image")?.to_owned();
    let record = super::candidates::resource(
        &mut client,
        expect::string(&receipt, "/record_resource_uri")?,
    )?;
    ensure!(
        record["repository"] == REPOSITORY
            && record["provenance"]["requested_source"]
                == json!({"type":"bundle","commit":commit,"sha256":sha256,
                    "prerequisites":[super::candidates::CDK]}),
        "record did not identify the bundle by its bytes and public base: {record}"
    );
    ensure!(
        !record.to_string().contains(&bundle),
        "record kept the local bundle path"
    );
    let catalog = client.call(
        "catalog_list",
        json!({"origins":["candidate"],"query":&sha256[..12]}),
    )?;
    context.record("candidate-bundle-catalog.json", &catalog)?;
    ensure!(
        expect::array(&catalog, "/items")?
            .iter()
            .any(|entry| entry["version"] == receipt["catalog_entry"]["version"]),
        "catalog label does not name the bundle digest: {catalog}"
    );
    let help = help_text(context, &image)?;
    ensure!(
        help.contains(&format!("{ABOUT} ({MARKER})")),
        "image was not built from the private commit: {help}"
    );

    let removed = client.call("candidate_remove", json!({"candidate_id":ID}))?;
    context.record("candidate-bundle-removed.json", &removed)?;
    ensure!(
        removed["candidate_id"] == ID && removed["retained_image"] == image.as_str(),
        "removal receipt did not name the retained image: {removed}"
    );
    resources_gone(context, &resource)?;
    let listed = client.call("candidate_list", json!({"id":ID}))?;
    let catalog = client.call(
        "catalog_list",
        json!({"origins":["candidate"],"query":&sha256[..12]}),
    )?;
    ensure!(
        expect::array(&listed, "/items")?.is_empty()
            && expect::array(&catalog, "/items")?.is_empty(),
        "removed build is still listed or in the catalog"
    );
    let missing = client.call_error("candidate_remove", json!({"candidate_id":ID}))?;
    ensure!(
        missing["code"] == -32002,
        "repeat removal was not not-found: {missing}"
    );
    context.record(
        "candidate-bundle-outcome.json",
        &json!({"passed":true,"commit_sha":commit,"bundle_bytes":bytes.len(),"sha256":sha256,
            "retained_image":image,"marker_in_image":true}),
    )
}
