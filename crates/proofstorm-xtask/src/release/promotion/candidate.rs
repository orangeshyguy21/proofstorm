use super::{PLATFORMS, REPORTS, file_digest, verify_installer_default, verify_manifest};
use crate::{
    development::inventory,
    release::{archive, bundle, text},
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) fn verify(
    metadata: &Path,
    candidate: &Path,
    version: &str,
    revision: &str,
) -> Result<BTreeMap<String, String>> {
    let mut expected = BTreeSet::new();
    for (slug, target) in PLATFORMS {
        let archive = format!("proofstorm-{version}-{target}.tar.gz");
        for name in [
            archive.clone(),
            format!("{archive}.sha256"),
            "install.sh".into(),
            "build-report.json".into(),
            "smoke-report.json".into(),
            "install-smoke-report.json".into(),
        ] {
            expected.insert(format!("{slug}/{name}"));
        }
    }
    let files = inventory(candidate)?;
    ensure!(
        files.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "unexpected or missing platform artifact files"
    );
    let mut assets = BTreeMap::new();
    let mut source = None;
    for (slug, target) in PLATFORMS {
        let directory = candidate.join(slug);
        let manifest = verify_platform(metadata, &directory, version, revision, target)?;
        if let Some(source) = &source {
            ensure!(
                *source == manifest["source"],
                "Linux and Mac source fingerprints differ"
            );
        } else {
            source = Some(manifest["source"].clone());
        }
        let archive = format!("proofstorm-{version}-{target}.tar.gz");
        for name in [
            archive.clone(),
            format!("{archive}.sha256"),
            "install.sh".into(),
        ] {
            assets.insert(name.clone(), files[&format!("{slug}/{name}")].clone());
        }
        for report in REPORTS {
            assets.insert(
                format!("{report}-{slug}.json"),
                files[&format!("{slug}/{report}.json")].clone(),
            );
        }
    }
    Ok(assets)
}

fn verify_platform(
    metadata: &Path,
    directory: &Path,
    version: &str,
    revision: &str,
    target: &str,
) -> Result<Value> {
    let archive_name = format!("proofstorm-{version}-{target}.tar.gz");
    let scratch = tempfile::tempdir()?;
    let extracted = scratch.path().join("verified");
    archive::extract(&directory.join(&archive_name), &extracted)?;
    let manifest = bundle::read_json(&extracted.join("proofstorm/manifest.json"))?;
    verify_manifest(&manifest, version, revision, target)?;
    let files = inventory(directory)?;
    let build = bundle::read_json(&directory.join("build-report.json"))?;
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
    let smoke = bundle::read_json(&directory.join("smoke-report.json"))?;
    let mac = target == "aarch64-apple-darwin";
    ensure!(
        smoke["integrity_verified"] == true
            && smoke["relocated_binaries_verified"] == true
            && smoke["source_read_access_denied"] == mac
            && smoke["release_ready"] == manifest["release_ready"],
        "relocation checks are missing or unsuccessful"
    );
    let install = bundle::read_json(&directory.join("install-smoke-report.json"))?;
    verify_install(&install, mac)?;
    ensure!(
        install["archive_sha256"] == files[&archive_name]
            && install["installer_sha256"] == files["install.sh"],
        "installer report checksum mismatch"
    );
    ensure!(
        file_digest(&metadata.join("source-install.sh"))? == files["install.sh"],
        "installer differs from selected source commit"
    );
    verify_installer_default(&directory.join("install.sh"), version)?;
    Ok(manifest)
}

fn verify_install(install: &Value, mac: bool) -> Result<()> {
    for key in ["local_install", "reinstall", "cli_mcp_metadata_match"] {
        ensure!(install[key] == true, "installer check missing: {key}");
    }
    for key in [
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
    for key in ["source_checkout_present", "build_tools_present"] {
        ensure!(
            install[key] == mac,
            "unexpected installer host evidence: {key}"
        );
    }
    if mac {
        ensure!(
            install["target"] == "aarch64-apple-darwin" && install["isolation"] == "macos-sandbox",
            "missing Mac isolation type"
        );
        for key in [
            "source_read_access_denied",
            "compiler_execution_denied",
            "outside_writes_denied",
        ] {
            ensure!(
                install[key] == true,
                "missing Mac isolation evidence: {key}"
            );
        }
    }
    Ok(())
}
