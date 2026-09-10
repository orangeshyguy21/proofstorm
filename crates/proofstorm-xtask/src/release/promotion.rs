//! Promote trusted CI artifacts without executing or rebuilding their payloads.
use super::{alpha_version, archive, bundle, text};
use crate::development::{inventory, regular};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, ffi::OsString, fs, io::Write, path::Path};

fn positive(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .filter(|v| *v > 0)
        .with_context(|| format!("invalid {key}"))
}

fn version(tag: &str) -> Result<&str> {
    let version = tag
        .strip_prefix('v')
        .context("expected vVERSION alpha tag")?;
    ensure!(
        alpha_version(version),
        "only explicit alpha versions may be promoted"
    );
    Ok(version)
}

fn run_plan(metadata: &Path, repo: &str, id: &str, tag: &str) -> Result<Vec<String>> {
    version(tag)?;
    ensure!(
        repo.split('/').count() == 2
            && repo.split('/').all(|p| !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))),
        "invalid repository"
    );
    let id: u64 = id.parse()?;
    ensure!(id > 0, "invalid run id");
    let run = bundle::read_json(&metadata.join("run.json"))?;
    let workflow = bundle::read_json(&metadata.join("workflow.json"))?;
    ensure!(
        positive(&run, "id")? == id
            && run["repository"]["full_name"] == repo
            && run["head_repository"]["full_name"] == repo,
        "run repository/id mismatch"
    );
    ensure!(
        run["path"] == ".github/workflows/check.yml"
            && workflow["path"] == ".github/workflows/check.yml"
            && positive(&run, "workflow_id")? == positive(&workflow, "id")?,
        "expected the Checks workflow"
    );
    ensure!(
        run["head_branch"] == "main"
            && matches!(run["event"].as_str(), Some("push" | "workflow_dispatch")),
        "only trusted main builds may be promoted"
    );
    ensure!(
        run["status"] == "completed" && run["conclusion"] == "success",
        "source run must have completed successfully"
    );
    let sha = text(&run, "head_sha")?;
    ensure!(
        sha.len() == 40
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid source commit"
    );
    let attempt = positive(&run, "run_attempt")?;
    Ok(vec![
        sha.into(),
        attempt.to_string(),
        format!("proofstorm-linux-amd64-{sha}-{attempt}"),
    ])
}

fn pages(path: &Path, key: &str) -> Result<Vec<Value>> {
    let document = bundle::read_json(path)?;
    let mut entries = Vec::new();
    for page in document
        .as_array()
        .context("expected paginated API response")?
    {
        entries.extend(
            page[key]
                .as_array()
                .context("missing API entries")?
                .iter()
                .cloned(),
        );
    }
    Ok(entries)
}

fn unused(metadata: &Path, tag: &str) -> Result<()> {
    let refs = bundle::read_json(&metadata.join("refs.json"))?;
    let exact = format!("refs/tags/{tag}");
    ensure!(
        !refs
            .as_array()
            .context("invalid tag refs")?
            .iter()
            .any(|r| r["ref"] == exact),
        "tag already exists; choose a new version"
    );
    let releases = bundle::read_json(&metadata.join("releases.json"))?;
    for page in releases.as_array().context("invalid release pages")? {
        ensure!(
            !page
                .as_array()
                .context("invalid release list")?
                .iter()
                .any(|r| r["tag_name"] == tag),
            "release or draft already exists; inspect it manually"
        );
    }
    Ok(())
}

fn evidence(metadata: &Path, repo: &str, id: &str, tag: &str) -> Result<Vec<String>> {
    let mut plan = run_plan(metadata, repo, id, tag)?;
    let sha = &plan[0];
    let compare = bundle::read_json(&metadata.join("ancestry.json"))?;
    ensure!(
        compare["base_commit"]["sha"] == *sha
            && compare["merge_base_commit"]["sha"] == *sha
            && matches!(compare["status"].as_str(), Some("ahead" | "identical")),
        "source commit is not on main"
    );
    let jobs = pages(&metadata.join("jobs.json"), "jobs")?;
    for name in [
        "Formatting and shell",
        "Rust lints and tests",
        "Linux bundle and installer",
    ] {
        let matches: Vec<_> = jobs.iter().filter(|j| j["name"] == name).collect();
        // The caller fetches the attempt-specific endpoint; GitHub job objects
        // do not guarantee a run_attempt field.
        ensure!(
            matches.len() == 1
                && matches[0]["status"] == "completed"
                && matches[0]["conclusion"] == "success"
                && matches[0]["head_sha"] == *sha
                && matches[0]["run_id"].as_u64() == Some(id.parse()?),
            "required job did not pass in this attempt: {name}"
        );
    }
    let artifacts = pages(&metadata.join("artifacts.json"), "artifacts")?;
    let matches: Vec<_> = artifacts.iter().filter(|a| a["name"] == plan[2]).collect();
    ensure!(
        matches.len() == 1,
        "expected exactly one matching Linux artifact"
    );
    let artifact = matches[0];
    positive(artifact, "id")?;
    ensure!(
        artifact["expired"] == false
            && artifact["workflow_run"]["id"].as_u64() == Some(id.parse()?)
            && artifact["workflow_run"]["head_sha"] == *sha,
        "artifact expired or belongs to another run"
    );
    plan.push(positive(artifact, "id")?.to_string());
    Ok(plan)
}

fn file_digest(path: &Path) -> Result<String> {
    regular(path)?;
    bundle::checksum(path, fs::metadata(path)?.len())
}

fn verify(metadata: &Path, candidate: &Path, repo: &str, id: &str, tag: &str) -> Result<()> {
    let plan = evidence(metadata, repo, id, tag)?;
    unused(metadata, tag)?;
    let version = version(tag)?;
    let archive_name = format!("proofstorm-{version}-x86_64-unknown-linux-gnu.tar.gz");
    let expected: BTreeSet<_> = [
        archive_name.clone(),
        format!("{archive_name}.sha256"),
        "install.sh".into(),
        "build-report.json".into(),
        "smoke-report.json".into(),
        "install-smoke-report.json".into(),
    ]
    .into_iter()
    .collect();
    let files = inventory(candidate)?;
    ensure!(
        files.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "unexpected or missing artifact files"
    );
    let archive_path = candidate.join(&archive_name);
    let scratch = tempfile::tempdir()?;
    let extracted = scratch.path().join("verified");
    archive::extract(&archive_path, &extracted)?;
    let manifest = bundle::read_json(&extracted.join("proofstorm/manifest.json"))?;
    ensure!(
        manifest["version"] == version
            && manifest["target"] == "x86_64-unknown-linux-gnu"
            && manifest["channel"] == "alpha"
            && manifest["build_profile"] == "release"
            && manifest["source"]["dirty"] == false
            && manifest["source"]["revision"] == plan[0],
        "candidate must be an optimized, clean alpha build of the selected commit"
    );
    let build = bundle::read_json(&candidate.join("build-report.json"))?;
    ensure!(
        Path::new(text(&build, "archive")?)
            .file_name()
            .and_then(|s| s.to_str())
            == Some(&archive_name)
            && build["sha256"] == files[&archive_name]
            && build["release_ready"] == manifest["release_ready"]
            && build["release_blockers"] == manifest["release_blockers"],
        "build report does not match bundle"
    );
    let smoke = bundle::read_json(&candidate.join("smoke-report.json"))?;
    ensure!(
        smoke["integrity_verified"] == true
            && smoke["relocated_binaries_verified"] == true
            && smoke["source_read_access_denied"] == false
            && smoke["release_ready"] == manifest["release_ready"],
        "relocation checks are missing or unsuccessful"
    );
    let install = bundle::read_json(&candidate.join("install-smoke-report.json"))?;
    for key in ["local_install", "reinstall", "cli_mcp_metadata_match"] {
        ensure!(install[key] == true, "installer check missing: {key}");
    }
    for key in [
        "source_checkout_present",
        "build_tools_present",
        "network_enabled",
        "runtime_tested",
        "github_download_tested",
        "development_override",
    ] {
        ensure!(
            install[key] == false,
            "unexpected installer evidence: {key}"
        );
    }
    ensure!(
        install["archive_sha256"] == files[&archive_name]
            && install["installer_sha256"] == files["install.sh"],
        "installer report checksum mismatch"
    );
    ensure!(
        file_digest(&metadata.join("source-install.sh"))? == files["install.sh"],
        "installer differs from selected source commit"
    );
    verify_installer_default(&candidate.join("install.sh"), version)?;
    let notes = format!(
        "Linux AMD64 alpha candidate {tag}\n\nPromoted without rebuilding from https://github.com/{repo}/actions/runs/{id} (attempt {}).\nSource commit: {}\n\nIncludes the tested installer, archive, checksum, and test reports. macOS assets are not included. Runtime setup, public image availability, and fresh-VM/public-download acceptance remain separate checks. This is not a stable or release-ready build.\n",
        plan[1], plan[0]
    );
    fs::write(metadata.join("notes.md"), &notes)?;
    fs::write(
        metadata.join("create-release.json"),
        serde_json::to_vec_pretty(
            &json!({"tag_name":tag,"target_commitish":plan[0],"name":format!("Proofstorm {tag} — Linux alpha"),"body":notes,"draft":true,"prerelease":true,"make_latest":"false"}),
        )?,
    )?;
    fs::write(
        metadata.join("promotion.json"),
        serde_json::to_vec_pretty(&json!({"tag":tag,"sha":plan[0],"files":files}))?,
    )?;
    Ok(())
}

fn draft(metadata: &Value, tag: &str, sha: &str) -> Result<u64> {
    ensure!(
        metadata["draft"] == true
            && metadata["prerelease"] == true
            && metadata["tag_name"] == tag
            && metadata["target_commitish"] == sha,
        "unexpected release response; do not upload/publish"
    );
    positive(metadata, "id")
}

fn verify_installer_default(path: &Path, version: &str) -> Result<()> {
    let installer = fs::read_to_string(path)?;
    let defaults: Vec<_> = installer
        .lines()
        .filter(|l| l.starts_with("install_version="))
        .collect();
    ensure!(
        defaults == [format!("install_version=\"{version}\"")],
        "installer default version must match the release"
    );
    Ok(())
}

fn uploaded(metadata: &Path, downloaded: &Path) -> Result<()> {
    let promotion = bundle::read_json(&metadata.join("promotion.json"))?;
    let original = bundle::read_json(&metadata.join("created.json"))?;
    let remote = bundle::read_json(&metadata.join("uploaded.json"))?;
    let tag = text(&promotion, "tag")?;
    let sha = text(&promotion, "sha")?;
    ensure!(
        draft(&remote, tag, sha)? == draft(&original, tag, sha)?,
        "release identity changed"
    );
    let files = inventory(downloaded)?;
    ensure!(
        serde_json::to_value(&files)? == promotion["files"],
        "uploaded assets differ from the tested candidate"
    );
    let assets = remote["assets"]
        .as_array()
        .context("missing uploaded assets")?;
    ensure!(
        assets.len() == files.len(),
        "unexpected uploaded asset count"
    );
    let mut names = BTreeSet::new();
    for asset in assets {
        let name = text(asset, "name")?;
        ensure!(
            names.insert(name) && files.contains_key(name) && asset["state"] == "uploaded",
            "unexpected/incomplete release asset"
        );
    }
    Ok(())
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args.collect();
    let strings: Vec<_> = args
        .iter()
        .map(|s| s.to_str().context("arguments must be UTF-8"))
        .collect::<Result<_>>()?;
    match strings.as_slice() {
        ["run", metadata, repo, id, tag] => {
            for value in run_plan(Path::new(metadata), repo, id, tag)? {
                std::io::stdout().write_all(value.as_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
        }
        ["evidence", metadata, repo, id, tag] => {
            println!("{}", evidence(Path::new(metadata), repo, id, tag)?[3]);
        }
        ["unused", metadata, tag] => unused(Path::new(metadata), tag)?,
        ["verify", metadata, candidate, repo, id, tag] => {
            verify(Path::new(metadata), Path::new(candidate), repo, id, tag)?;
        }
        ["created", file, tag, sha] => {
            println!("{}", draft(&bundle::read_json(Path::new(file))?, tag, sha)?);
        }
        ["uploaded", metadata, downloaded] => uploaded(Path::new(metadata), Path::new(downloaded))?,
        _ => bail!("invalid release-promotion arguments"),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
