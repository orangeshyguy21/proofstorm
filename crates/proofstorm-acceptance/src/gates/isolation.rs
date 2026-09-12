//! Two ordinarily configured, parent-owned installations; no raw k3d lifecycle.
use crate::{GateContext, process};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, process::Command};

fn request(
    context: &GateContext,
    method: &str,
    url: &str,
    body: &[u8],
    content_type: &str,
) -> Result<BTreeMap<String, String>> {
    let payload = tempfile::NamedTempFile::new_in(context.work())?;
    fs::write(payload.path(), body)?;
    let headers = tempfile::NamedTempFile::new_in(context.work())?;
    let mut command = Command::new("curl");
    command
        .args([
            "-q",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "-X",
            method,
            "-H",
            &format!("Content-Type: {content_type}"),
            "--data-binary",
            &format!("@{}", payload.path().display()),
            "--dump-header",
        ])
        .arg(headers.path())
        .arg(url);
    ensure!(
        process::capture(command, 40)?.status.success(),
        "owned registry fixture request failed"
    );
    Ok(fs::read_to_string(headers.path())?
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
        .collect())
}

fn publish(context: &GateContext) -> Result<String> {
    proofstorm_app::bootstrap::verify_runtime_identity(&context.installation)?;
    let base = format!(
        "http://{}/v2/isolation-probe",
        context.installation.host_registry()
    );
    let config = serde_json::to_vec(
        &json!({"architecture":proofstorm_app::platform::container_arch_for(proofstorm_app::platform::target())?,
        "os":"linux","rootfs":{"type":"layers","diff_ids":[]},"config":{"Labels":{"proofstorm.test.installation":context.installation.id}}}),
    )?;
    let digest = format!("sha256:{:x}", Sha256::digest(&config));
    let headers = request(
        context,
        "POST",
        &format!("{base}/blobs/uploads/"),
        b"",
        "application/octet-stream",
    )?;
    let location = headers
        .get("location")
        .context("registry upload location missing")?;
    let location = if location.starts_with('/') {
        format!("http://{}{location}", context.installation.host_registry())
    } else {
        location.clone()
    };
    ensure!(
        location.starts_with(&format!("{base}/blobs/uploads/")),
        "registry upload redirected outside owned instance"
    );
    let separator = if location.contains('?') { '&' } else { '?' };
    request(
        context,
        "PUT",
        &format!("{location}{separator}digest={digest}"),
        &config,
        "application/octet-stream",
    )?;
    let media = "application/vnd.oci.image.manifest.v1+json";
    let manifest = serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":media,
        "config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":digest,"size":config.len()},"layers":[]}))?;
    let digest = format!("sha256:{:x}", Sha256::digest(&manifest));
    let headers = request(
        context,
        "PUT",
        &format!("{base}/manifests/smoke"),
        &manifest,
        media,
    )?;
    ensure!(
        headers.get("docker-content-digest") == Some(&digest),
        "registry changed fixture digest"
    );
    Ok(format!(
        "proofstorm-registry.localhost:5000/isolation-probe@{digest}"
    ))
}

fn pull(context: &GateContext, image: &str) -> Result<std::process::Output> {
    proofstorm_app::bootstrap::verify_runtime_identity(&context.installation)?;
    let mut command = Command::new("docker");
    crate::client::clear_runtime_environment(&mut command);
    command.args([
        "exec",
        &format!("k3d-{}-server-0", context.installation.cluster_name()),
        "crictl",
        "--timeout=20s",
        "pull",
        image,
    ]);
    process::capture(command, 45)
}

pub fn run(context: &GateContext) -> Result<()> {
    let (installation, _) = crate::runner::read_peer(context.work())?;
    ensure!(
        installation.id != context.installation.id
            && installation.registry_port != context.installation.registry_port,
        "peer is not independent"
    );
    let peer = GateContext::new(&context.root, installation, context.artifacts.clone())?;
    let first = publish(context)?;
    let second = publish(&peer)?;
    ensure!(first != second, "isolation fixtures have the same digest");
    for (owned, image) in [(context, &first), (&peer, &second)] {
        ensure!(
            pull(owned, image)?.status.success(),
            "own-registry image pull failed"
        );
    }
    for (owned, foreign) in [(context, &second), (&peer, &first)] {
        let output = pull(owned, foreign)?;
        let error = String::from_utf8_lossy(&output.stderr).to_lowercase();
        ensure!(
            !output.status.success() && (error.contains("not found") || error.contains("notfound")),
            "foreign registry digest was not specifically rejected as absent"
        );
    }
    context.record("installation-isolation.json", &json!({"passed":true,"own_registry_pulls":true,"foreign_digest_pulls_refused":true,
        "installations":[context.installation.id, peer.installation.id],"external_publication":false}))
}
