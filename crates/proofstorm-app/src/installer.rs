//! Verified, user-prefix installation. Does not initialize a cluster or agent config.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const REQUIRED: &[&str] = &[
    "bin/proofstorm",
    "bin/proofstorm-mcp",
    "LICENSE",
    "catalog.json",
    "release-info.json",
    "tools/versions.env",
    "chart/Chart.yaml",
    "chart/values.yaml",
    "chart/templates/deployment.yaml",
    "chart/templates/_helpers.tpl",
    "chart/templates/serviceaccount.yaml",
    "chart/templates/rbac.yaml",
    "chart/templates/private-pvc.yaml",
    "chart/crds/proofstorm.dev_proofstormlabs.yaml",
    "chart/crds/proofstorm.dev_proofstormlabactions.yaml",
    "chart/crds/proofstorm.dev_proofstormcandidatebuilds.yaml",
];

#[derive(Deserialize)]
struct FileReceipt {
    sha256: String,
    size: u64,
    mode: u32,
}

fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = vec![0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn inventory(root: &Path, directory: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "bundle symlink refused");
        if kind.is_dir() {
            inventory(root, &entry.path(), files)?;
        } else {
            ensure!(kind.is_file(), "bundle contains a non-regular file");
            files.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF-8 bundle path")?
                    .into(),
            );
        }
    }
    Ok(())
}

fn alpha_version(version: &str) -> bool {
    let Some((base, number)) = version.split_once("-alpha.") else {
        return false;
    };
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let parts: Vec<_> = base.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| numeric(part)) && numeric(number)
}

fn validate_alpha(manifest: &Value, info: &Value) -> Result<()> {
    let digest = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let controller = &info["controller"];
    ensure!(
        info["version"].as_str().is_some_and(alpha_version) && manifest["release_ready"] == false,
        "alpha channel requires an alpha version without stable release readiness"
    );
    ensure!(
        manifest["controller"] == *controller
            && controller["image"].as_str().is_some_and(|image| {
                image.starts_with("ghcr.io/")
                    && !image.chars().any(char::is_whitespace)
                    && image
                        .split_once("@sha256:")
                        .is_some_and(|(_, sha)| digest(sha))
            })
            && controller["platform"] == crate::platform::container_platform()?
            && controller["metadata"]["version"] == info["version"]
            && info["runtime_contract_sha256"].as_str().is_some_and(digest)
            && controller["metadata"]["runtime_contract_sha256"] == info["runtime_contract_sha256"],
        "alpha controller identity or compatibility mismatch"
    );
    ensure!(
        info["bootstrap_tools"]["target"] == info["target"]
            && info["bootstrap_tools"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
        "alpha requires platform-matching bootstrap tools"
    );
    Ok(())
}

pub(crate) fn verify(root: &Path, allow_development: bool, match_binary: bool) -> Result<Value> {
    let expected = match_binary.then(crate::release::describe);
    verify_with_metadata(root, allow_development, expected.as_ref())
}

fn verify_with_metadata(
    root: &Path,
    allow_development: bool,
    expected_info: Option<&Value>,
) -> Result<Value> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "bundle root must be a directory, not a symlink"
    );
    let mut observed = BTreeSet::new();
    inventory(root, root, &mut observed)?;
    let manifest: Value = serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
    ensure!(
        manifest["format_version"] == 1
            && manifest["target"] == crate::platform::target()
            && crate::platform::container_arch().is_ok(),
        "unsupported bundle format or platform"
    );
    ensure!(
        manifest["channel"] == "development"
            || manifest["channel"] == "alpha"
            || manifest["channel"] == "release",
        "unknown release channel"
    );
    if !allow_development && manifest["channel"] != "alpha" {
        ensure!(
            manifest["release_ready"] == true
                && manifest["channel"] == "release"
                && manifest["build_profile"] == "release"
                && manifest["source"]["dirty"] == false
                && manifest["release_blockers"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                && !manifest["controller"].is_null(),
            "bundle is not release-ready; local tests require --allow-development"
        );
    }
    let receipts: BTreeMap<String, FileReceipt> =
        serde_json::from_value(manifest["files"].clone())?;
    ensure!(
        REQUIRED.iter().all(|name| receipts.contains_key(*name)),
        "bundle is missing required payload"
    );
    let expected: BTreeSet<_> = receipts
        .keys()
        .cloned()
        .chain(["manifest.json".into()])
        .collect();
    ensure!(
        observed == expected,
        "bundle contains missing or unlisted files"
    );
    for (name, receipt) in receipts {
        ensure!(safe_relative(&name), "unsafe bundle path");
        let path = root.join(&name);
        ensure!(
            receipt.size > 0
                && fs::metadata(&path)?.len() == receipt.size
                && hash(&path)? == receipt.sha256,
            "bundle checksum mismatch: {name}"
        );
        let mode = if name.starts_with("bin/") {
            0o755
        } else {
            0o644
        };
        ensure!(
            receipt.mode == mode,
            "unsupported payload permissions: {name}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                fs::metadata(&path)?.permissions().mode() & 0o7777 == mode,
                "payload permissions mismatch: {name}"
            );
        }
    }
    let info: Value = serde_json::from_slice(&fs::read(root.join("release-info.json"))?)?;
    if manifest["channel"] == "alpha" {
        validate_alpha(&manifest, &info)?;
    }
    ensure!(
        info["target"] == manifest["target"]
            && info["version"] == manifest["version"]
            && info["source_revision"] == manifest["source"]["revision"]
            && info["source_sha256"] == manifest["source"]["sha256"]
            && info["build_profile"] == manifest["build_profile"],
        "release metadata mismatch"
    );
    ensure!(
        expected_info.is_none_or(|expected| info == *expected),
        "run install-bundle using the executable inside this bundle"
    );
    Ok(manifest)
}

fn directory(path: &Path) -> Result<()> {
    if path.try_exists()? || fs::symlink_metadata(path).is_ok() {
        ensure!(
            fs::symlink_metadata(path)?.is_dir(),
            "refusing non-directory install path: {}",
            path.display()
        );
    } else {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    Ok(())
}

fn create_parents(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if !parent.try_exists()? {
            create_parents(parent)?;
        }
    }
    directory(path)
}

fn shell_quote(path: &Path) -> Result<String> {
    let path = path.to_str().context("installation paths must be UTF-8")?;
    ensure!(
        !path.contains(['\n', '\r']),
        "installation path contains a newline"
    );
    Ok(format!("'{}'", path.replace('\'', "'\"'\"'")))
}

fn launcher(managed: &Path, binary: &str) -> Result<String> {
    Ok(format!(
        "#!/bin/sh\n# Proofstorm managed launcher v1\nif [ \"${{PROOFSTORM_HOME+x}}\" != x ]; then\n  PROOFSTORM_HOME={}\n  export PROOFSTORM_HOME\nfi\nexec {} \"$@\"\n",
        shell_quote(&managed.join("state"))?,
        shell_quote(&managed.join("current/bin").join(binary))?
    ))
}

fn write_new(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(if executable {
                0o755
            } else {
                0o600
            }))?;
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    Ok(())
}

/// All files are verified before activation. One symlink switch activates both binaries.
pub fn install(bundle: &Path, prefix: &Path, allow_development: bool) -> Result<Value> {
    install_with_metadata(
        bundle,
        prefix,
        allow_development,
        &crate::release::describe(),
    )
}

fn install_with_metadata(
    bundle: &Path,
    prefix: &Path,
    allow_development: bool,
    expected: &Value,
) -> Result<Value> {
    let manifest = verify_with_metadata(bundle, allow_development, Some(expected))?;
    ensure!(prefix.is_absolute(), "installation prefix must be absolute");
    shell_quote(prefix)?;
    create_parents(prefix)?;
    let prefix = prefix.canonicalize()?;
    directory(&prefix.join("bin"))?;
    directory(&prefix.join("lib"))?;
    let managed = prefix.join("lib/proofstorm");
    let existed = managed.try_exists()? || fs::symlink_metadata(&managed).is_ok();
    if existed {
        directory(&managed)?;
        let owner: Value = serde_json::from_slice(
            &fs::read(managed.join("install.json"))
                .context("unowned installation directory; refusing to adopt it")?,
        )?;
        ensure!(
            owner == json!({"format_version":1,"prefix":prefix}),
            "installation ownership does not match prefix"
        );
    }
    for name in ["proofstorm", "proofstorm-mcp"] {
        let path = prefix.join("bin").join(name);
        if fs::symlink_metadata(&path).is_ok() {
            ensure!(
                existed
                    && fs::symlink_metadata(&path)?.is_file()
                    && fs::read_to_string(&path)? == launcher(&managed, name)?,
                "refusing to overwrite unrelated executable: {}",
                path.display()
            );
        }
    }
    directory(&managed)?;
    let _guard = crate::installation::Installation::lock(&managed)?;
    if !existed {
        write_new(
            &managed.join("install.json"),
            &serde_json::to_vec(&json!({"format_version":1,"prefix":prefix}))?,
            false,
        )?;
    }
    let versions = managed.join("versions");
    directory(&versions)?;
    let current = managed.join("current");
    if let Ok(metadata) = fs::symlink_metadata(&current) {
        ensure!(
            metadata.file_type().is_symlink(),
            "current installation pointer is not owned"
        );
        let target = fs::read_link(&current)?;
        let parts: Vec<_> = target.components().collect();
        ensure!(
            parts.len() == 2
                && parts[0].as_os_str() == "versions"
                && parts[1]
                    .as_os_str()
                    .to_str()
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                && fs::symlink_metadata(managed.join(target))?.is_dir(),
            "unsafe current installation pointer"
        );
    }
    let id = hash(&bundle.join("manifest.json"))?;
    let version = versions.join(&id);
    if fs::symlink_metadata(&version).is_err() {
        let stage = tempfile::tempdir_in(&versions)?;
        let mut files = BTreeSet::new();
        inventory(bundle, bundle, &mut files)?;
        for name in files {
            let target = stage.path().join(&name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(bundle.join(name), target)?;
        }
        verify_with_metadata(stage.path(), allow_development, Some(expected))?;
        fs::rename(stage.path(), &version)?;
    } else {
        verify_with_metadata(&version, allow_development, Some(expected))?;
        ensure!(
            hash(&version.join("manifest.json"))? == id,
            "installed version was changed"
        );
    }
    for name in ["proofstorm", "proofstorm-mcp"] {
        let path = prefix.join("bin").join(name);
        if !path.try_exists()? {
            write_new(&path, launcher(&managed, name)?.as_bytes(), true)?;
        }
    }
    activate(&managed, &id)?;
    Ok(
        json!({"installed":true,"version":manifest["version"],"prefix":prefix,"executable":prefix.join("bin/proofstorm"),
        "home":managed.join("state"),"release_ready":manifest["release_ready"],"runtime_initialized":false}),
    )
}

fn activate(managed: &Path, id: &str) -> Result<()> {
    #[cfg(unix)]
    {
        let staging = tempfile::tempdir_in(managed)?;
        let next = staging.path().join("current");
        std::os::unix::fs::symlink(PathBuf::from("versions").join(id), &next)?;
        fs::rename(next, managed.join("current"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    anyhow::bail!("installer currently requires macOS");
}

#[cfg(test)]
mod tests;
