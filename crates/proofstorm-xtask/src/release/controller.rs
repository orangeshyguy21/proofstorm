//! Verified controller build inputs and immutable publication evidence.
mod registry;
use super::{archive::output_path, build, bundle, sha256, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{ffi::OsString, fs, io::Write, path::Path};

const REPOSITORY: &str = "ghcr.io/orangeshyguy21/proofstorm/proofstormd";

fn architecture(platform: &str) -> Result<&str> {
    match platform {
        "linux/amd64" => Ok("amd64"),
        "linux/arm64" => Ok("arm64"),
        _ => bail!("controller platform must be linux/amd64 or linux/arm64"),
    }
}

fn platform_for_target(target: &str) -> Result<&str> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok("linux/amd64"),
        "aarch64-apple-darwin" => Ok("linux/arm64"),
        _ => bail!("unsupported controller host target"),
    }
}

fn version(source: &Path) -> Result<String> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(source.join("Cargo.toml"))?)?;
    let version = manifest
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("version"))
        .and_then(toml::Value::as_str)
        .context("missing workspace version")?;
    ensure!(
        super::alpha_version(version),
        "controller CI currently supports alpha versions only"
    );
    Ok(version.into())
}

fn save(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn prepare(source: &Path, work: &Path, platform: &str) -> Result<Vec<String>> {
    architecture(platform)?;
    let source = source.canonicalize()?;
    let work = output_path(work)?;
    ensure!(
        !work.starts_with(&source) && !work.exists(),
        "controller work must be new and outside the checkout"
    );
    let staging = tempfile::Builder::new()
        .prefix("proofstorm-controller-")
        .tempdir_in(work.parent().context("missing work parent")?)?;
    let provenance = build::snapshot(&source, &staging.path().join("source"), false, None)?;
    let version = version(&staging.path().join("source"))?;
    let suffix = staging
        .path()
        .file_name()
        .context("missing staging name")?
        .to_str()
        .context("non-UTF-8 staging name")?
        .to_ascii_lowercase();
    let tag = format!(
        "{REPOSITORY}:ci-{}-{suffix}",
        text(&provenance, "revision")?
    );
    let receipt = json!({"format_version":1,"release_ready":false,"platform":platform,"source":provenance,"version":version,"tag":tag});
    save(&staging.path().join("build.json"), &receipt)?;
    fs::create_dir(&work)?;
    for name in ["source", "build.json"] {
        fs::rename(staging.path().join(name), work.join(name))?;
    }
    Ok(vec![
        work.to_str().context("non-UTF-8 work")?.into(),
        tag,
        text(&provenance, "sha256")?.into(),
    ])
}

fn local(work: &Path) -> Result<()> {
    let mut receipt = bundle::read_json(&work.join("build.json"))?;
    build::verify_snapshot(&work.join("source"), &receipt["source"])?;
    let inspect = bundle::read_json(&work.join("inspect.json"))?;
    let images = inspect.as_array().context("invalid image inspection")?;
    ensure!(images.len() == 1, "expected one controller image");
    let image = &images[0];
    let arch = architecture(text(&receipt, "platform")?)?;
    let id = text(image, "Id")?;
    ensure!(
        id.strip_prefix("sha256:").is_some_and(sha256),
        "invalid local image id"
    );
    ensure!(
        image["Os"] == "linux"
            && image["Architecture"] == arch
            && image["Config"]["User"] == "65532:65532",
        "controller must match the recorded Linux platform and run as non-root"
    );
    ensure!(
        image["Config"]["Labels"]["dev.proofstorm.source-sha256"] == receipt["source"]["sha256"],
        "image/source label mismatch"
    );
    let metadata = bundle::read_json(&work.join("metadata.json"))?;
    ensure!(
        metadata["format_version"] == 1
            && metadata["version"] == receipt["version"]
            && metadata["source_sha256"] == receipt["source"]["sha256"]
            && sha256(text(&metadata, "runtime_contract_sha256")?),
        "controller version/source metadata mismatch"
    );
    ensure!(
        fs::read(work.join("helper.stdout"))?.is_empty()
            && fs::read_to_string(work.join("helper.stderr"))?.trim()
                == "{\"runner_error\":\"native_runner_failed\"}"
            && fs::read_to_string(work.join("helper.status"))?.trim() == "1",
        "controller execution helper failed its startup probe"
    );
    if let Some(previous) = receipt.get("local_image_id") {
        ensure!(
            *previous == image["Id"] && receipt["metadata"] == metadata,
            "controller changed after verification"
        );
    }
    receipt["local_image_id"] = json!(id);
    receipt["metadata"] = metadata;
    receipt["verification"] = json!({"offline_metadata":true,"non_root":true,"helper_startup":true,"cluster_reconciliation":false});
    save(&work.join("build.json"), &receipt)
}

fn helper_probe(work: &Path, id: &str) -> Result<()> {
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    ensure!(
        id.strip_prefix("sha256:").is_some_and(sha256),
        "invalid probe image identity"
    );
    let receipt = bundle::read_json(&work.join("build.json"))?;
    let platform = text(&receipt, "platform")?;
    architecture(platform)?;
    let mut child = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--platform",
            platform,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--memory",
            "128m",
            "--cpus",
            "1",
            "--pids-limit",
            "128",
            "--entrypoint",
            "/usr/local/lib/proofstorm-exec",
            id,
        ])
        .stdout(Stdio::from(fs::File::create(work.join("helper.stdout"))?))
        .stderr(Stdio::from(fs::File::create(work.join("helper.stderr"))?))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            fs::write(
                work.join("helper.status"),
                status.code().unwrap_or(-1).to_string(),
            )?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("controller helper probe timed out");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn validate(
    receipt: &Value,
    provenance: &Value,
    expected_version: &str,
    platform: &str,
) -> Result<()> {
    architecture(platform)?;
    ensure!(
        receipt["format_version"] == 1
            && receipt["platform"] == platform
            && receipt["release_ready"] == false
            && receipt["source"] == *provenance
            && provenance["dirty"] == false,
        "controller must come from the same clean source snapshot as the bundle"
    );
    ensure!(
        receipt["metadata"]["version"] == expected_version
            && receipt["metadata"]["source_sha256"] == provenance["sha256"]
            && sha256(text(&receipt["metadata"], "runtime_contract_sha256")?),
        "controller version/source mismatch"
    );
    let image = text(receipt, "image")?;
    ensure!(
        image
            .strip_prefix(&format!("{REPOSITORY}@sha256:"))
            .is_some_and(sha256),
        "controller is not pinned to the expected GHCR repository"
    );
    ensure!(
        receipt["anonymous_verified"] == true
            && receipt["verification"]["registry_identity"] == true
            && receipt["verification"]["offline_metadata"] == true
            && receipt["verification"]["non_root"] == true
            && receipt["verification"]["helper_startup"] == true,
        "controller publication/startup evidence is incomplete"
    );
    Ok(())
}

pub(super) fn stage(
    receipt: &Path,
    source: &Path,
    provenance: &Value,
    destination: &Path,
    platform: &str,
) -> Result<()> {
    build::verify_snapshot(source, provenance)?;
    let receipt = bundle::read_json(receipt)?;
    validate(&receipt, provenance, &version(source)?, platform)?;
    save(destination, &receipt)
}

fn published(work: &Path) -> Result<()> {
    let mut receipt = bundle::read_json(&work.join("build.json"))?;
    let publication = bundle::read_json(&work.join("published.json"))?;
    let digest = text(&publication, "digest")?;
    ensure!(
        digest.strip_prefix("sha256:").is_some_and(sha256),
        "invalid published digest"
    );
    let platform = text(&receipt, "platform")?.to_owned();
    registry::verify(digest, text(&receipt, "local_image_id")?, &platform)?;
    receipt["image"] = json!(format!("{REPOSITORY}@{digest}"));
    receipt["anonymous_verified"] = json!(true);
    receipt["verification"]["registry_identity"] = json!(true);
    validate(
        &receipt,
        &receipt["source"],
        text(&receipt["metadata"], "version")?,
        &platform,
    )?;
    save(&work.join("controller.json"), &receipt)
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args.collect();
    let args: Vec<_> = args
        .iter()
        .map(|s| s.to_str().context("UTF-8 arguments required"))
        .collect::<Result<_>>()?;
    match args.as_slice() {
        ["prepare", source, work, rest @ ..] if rest.len() <= 1 => {
            for field in prepare(
                Path::new(source),
                Path::new(work),
                rest.first().copied().unwrap_or("linux/amd64"),
            )? {
                std::io::stdout().write_all(field.as_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
        }
        ["local", work] => local(Path::new(work))?,
        ["helper", work, id] => helper_probe(Path::new(work), id)?,
        ["published", work] => published(Path::new(work))?,
        ["platform", work] => {
            let receipt = bundle::read_json(&Path::new(work).join("build.json"))?;
            let platform = text(&receipt, "platform")?;
            architecture(platform)?;
            println!("{platform}");
        }
        ["host", info, receipt] => ensure!(
            bundle::read_json(Path::new(info))?["controller"]
                == bundle::read_json(Path::new(receipt))?,
            "host binaries did not embed the verified controller receipt"
        ),
        ["tag", work] => {
            let receipt = bundle::read_json(&Path::new(work).join("build.json"))?;
            let tag = text(&receipt, "tag")?;
            ensure!(
                tag.starts_with(&format!("{REPOSITORY}:ci-"))
                    && tag
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/.:_-".contains(&b)),
                "unsafe controller publication tag"
            );
            println!("{tag}");
        }
        ["stage", receipt, source, provenance, destination, rest @ ..] if rest.len() <= 1 => stage(
            Path::new(receipt),
            Path::new(source),
            &bundle::read_json(Path::new(provenance))?,
            Path::new(destination),
            if let Some(target) = rest.first() {
                platform_for_target(target)?
            } else {
                "linux/amd64"
            },
        )?,
        _ => bail!("invalid controller command"),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
