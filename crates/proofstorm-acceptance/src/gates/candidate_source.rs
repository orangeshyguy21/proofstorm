//! Derive binary expectations from the candidate's immutable source identity.
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use toml_edit::DocumentMut;

pub(super) fn cdk_version(record: &Value, receipt: &Value) -> Result<String> {
    cdk_version_with(record, receipt, |repository, commit, path| {
        let url = format!("https://raw.githubusercontent.com/{repository}/{commit}/{path}");
        let mut command = Command::new("curl");
        command.args([
            "-q",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--max-time",
            "30",
            "--max-filesize",
            "1048576",
            &url,
        ]);
        let output = crate::process::capture(command, 35)?;
        ensure!(
            output.status.success(),
            "pinned candidate manifest unavailable: {path}"
        );
        Ok(String::from_utf8(output.stdout)?)
    })
}

fn cdk_version_with<F>(record: &Value, receipt: &Value, mut fetch: F) -> Result<String>
where
    F: FnMut(&str, &str, &str) -> Result<String>,
{
    ensure!(
        record["phase"] == "succeeded"
            && record["implementation"] == "cdk"
            && record["id"] == receipt["candidate_id"]
            && record["commit_sha"] == receipt["commit_sha"]
            && record["image"]
                .as_str()
                .is_some_and(|image| !image.is_empty())
            && record["image"] == receipt["image"]
            && record["version"] == receipt["catalog_entry"]["version"],
        "candidate source record does not match the built image receipt"
    );
    let commit = record["commit_sha"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("candidate record requires a pinned commit")?;
    let repository = record["repository"]
        .as_str()
        .and_then(|url| url.strip_prefix("https://github.com/"))
        .and_then(|name| name.strip_suffix(".git"))
        .filter(|name| {
            name.split('/').count() == 2
                && name.split('/').all(|part| {
                    !part.is_empty()
                        && part != "."
                        && part != ".."
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                })
        })
        .context("candidate record requires a public GitHub source repository")?;
    let manifest =
        fetch(repository, commit, "crates/cdk-mintd/Cargo.toml")?.parse::<DocumentMut>()?;
    let package = manifest
        .get("package")
        .context("candidate package missing")?;
    ensure!(
        package.get("name").and_then(toml_edit::Item::as_str) == Some("cdk-mintd"),
        "unexpected candidate package"
    );
    let version = package
        .get("version")
        .context("candidate package version missing")?;
    let version = if let Some(version) = version.as_str() {
        version.to_owned()
    } else {
        ensure!(
            version.get("workspace").and_then(toml_edit::Item::as_bool) == Some(true),
            "unsupported candidate version declaration"
        );
        let workspace = fetch(repository, commit, "Cargo.toml")?.parse::<DocumentMut>()?;
        workspace
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(toml_edit::Item::as_str)
            .context("candidate workspace package version missing")?
            .to_owned()
    };
    semver::Version::parse(&version).context("candidate package version is not semantic")?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identities(commit: &str) -> (Value, Value) {
        (
            json!({"id":"candidate", "phase":"succeeded", "implementation":"cdk", "repository":"https://github.com/contributor/cdk.git", "commit_sha":commit, "image":"registry/cdk@sha256:example", "version":"candidate-test"}),
            json!({"candidate_id":"candidate", "commit_sha":commit, "image":"registry/cdk@sha256:example", "catalog_entry":{"version":"candidate-test"}}),
        )
    }

    #[test]
    fn versions_follow_the_recorded_commit_and_workspace_inheritance() {
        for (commit, version, inherited) in [
            ("a".repeat(40), "0.18.0", false),
            ("b".repeat(40), "0.19.2-rc.1", true),
        ] {
            let (record, receipt) = identities(&commit);
            let mut paths = Vec::new();
            let actual = cdk_version_with(&record, &receipt, |repository, sha, path| {
                assert_eq!(repository, "contributor/cdk");
                assert_eq!(sha, commit);
                paths.push(path.to_owned());
                Ok(match path {
                    "crates/cdk-mintd/Cargo.toml" if inherited => {
                        "[package]\nname = 'cdk-mintd'\nversion.workspace = true".into()
                    }
                    "crates/cdk-mintd/Cargo.toml" => {
                        format!("[package]\nname = 'cdk-mintd'\nversion = '{version}'")
                    }
                    "Cargo.toml" => format!("[workspace.package]\nversion = '{version}'"),
                    _ => panic!("unexpected source path"),
                })
            })
            .unwrap();
            assert_eq!(actual, version);
            assert_eq!(paths.len(), if inherited { 2 } else { 1 });
        }
    }

    #[test]
    fn mismatched_or_unpinned_records_fail_before_fetching() {
        let (record, receipt) = identities(&"a".repeat(40));
        for (field, value) in [
            ("commit_sha", json!("main")),
            ("image", json!("different")),
            ("repository", json!("https://github.com/../cdk.git")),
            ("phase", json!("failed")),
        ] {
            let mut changed = record.clone();
            changed[field] = value;
            assert!(
                cdk_version_with(&changed, &receipt, |_, _, _| panic!("must not fetch")).is_err()
            );
        }
    }

    #[test]
    fn missing_invalid_or_unavailable_versions_never_fall_back_to_catalog() {
        let (record, receipt) = identities(&"a".repeat(40));
        for manifest in [
            "[package]\nname='cdk-mintd'",
            "[package]\nname='cdk-mintd'\nversion='main'",
            "[package]\nname='other'\nversion='0.18.1'",
            "[package]\nname='cdk-mintd'\nversion.workspace=true",
        ] {
            assert!(cdk_version_with(&record, &receipt, |_, _, _| Ok(manifest.into())).is_err());
        }
        assert!(
            cdk_version_with(&record, &receipt, |_, _, _| anyhow::bail!("unavailable")).is_err()
        );
    }
}
