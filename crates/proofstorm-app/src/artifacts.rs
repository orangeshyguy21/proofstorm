//! Select code/resources independently of installation state. Releases retain
//! their verified bundle contract; checkout builds require explicit registration.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const RECORD: &str = "checkout-artifacts.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkout {
    format_version: u32,
    installation_id: String,
    source: PathBuf,
    resources: PathBuf,
    web_dist: PathBuf,
    cli: PathBuf,
    mcp: PathBuf,
    cli_sha256: String,
    mcp_sha256: String,
    files: BTreeMap<String, String>,
    metadata: Value,
}

pub(crate) fn hash(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file(),
        "artifact must be a regular non-linked file: {}",
        path.display()
    );
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 16384];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn inventory(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    ensure!(
        fs::symlink_metadata(directory)?.is_dir(),
        "linked artifact directory refused"
    );
    for file in fs::read_dir(directory)? {
        let path = file?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            inventory(root, &path, files)?;
        } else {
            files.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .context("non-UTF-8 resource")?
                    .into(),
                hash(&path)?,
            );
        }
    }
    Ok(())
}

pub(crate) fn tree_sha256(root: &Path) -> Result<String> {
    let mut files = BTreeMap::new();
    inventory(root, root, &mut files)?;
    let mut digest = Sha256::new();
    for (name, sha) in files {
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(sha.as_bytes());
        digest.update(b"\n");
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn read(home: &Path) -> Result<Option<Checkout>> {
    let path = home.join(RECORD);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        meta.is_file() && meta.len() < 2 * 1024 * 1024,
        "invalid checkout registration"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.nlink() == 1 && meta.permissions().mode().trailing_zeros() >= 6,
            "checkout registration must be private and unlinked"
        );
    }
    let record: Checkout = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        record.format_version == 1
            && crate::installation::Installation::load(home)?.id == record.installation_id,
        "checkout registration belongs to a different installation"
    );
    for path in [
        &record.source,
        &record.resources,
        &record.web_dist,
        &record.cli,
        &record.mcp,
    ] {
        ensure!(
            path.is_absolute(),
            "checkout artifact paths must be absolute"
        );
    }
    for name in record.files.keys() {
        ensure!(
            !name.is_empty()
                && Path::new(name)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "invalid checkout resource path"
        );
    }
    Ok(Some(record))
}

/// Short-lived proof of a complete verification for one GUI startup operation.
/// Never persisted or shared with another process: the child verifies independently.
/// New requests still verify changed binaries, resources and registration normally.
pub(crate) struct Verified {
    pub installation: crate::installation::Installation,
    pub root: PathBuf,
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub allow_development: bool,
    pub controller_sha256: Option<String>,
}

impl Verified {
    pub fn load(home: &Path, allow_development: bool) -> Result<Self> {
        let installation = crate::installation::Installation::load(home)?;
        let executable = std::env::current_exe()?.canonicalize()?;
        if let Some(record) = read(home)? {
            record.verify(&executable)?;
            ensure!(
                record.installation_id == installation.id,
                "installation changed during verification"
            );
            let executable_sha256 = if executable == record.cli {
                record.cli_sha256.clone()
            } else {
                record.mcp_sha256.clone()
            };
            ensure!(
                fs::symlink_metadata(record.resources.join("controller-source"))?.is_dir(),
                "controller source snapshot missing or linked; run make dev-build"
            );
            // The inventory was just hashed and verified. Derive the controller
            // snapshot digest from those receipts, without rereading every file.
            let files = record.files.iter().filter_map(|(name, sha)| {
                name.strip_prefix("controller-source/")
                    .map(|name| (name, sha))
            });
            let mut digest = Sha256::new();
            for (name, sha) in files {
                digest.update(name.as_bytes());
                digest.update(b"\0");
                digest.update(sha.as_bytes());
                digest.update(b"\n");
            }
            let controller_sha256 = format!("{:x}", digest.finalize());
            let manifest: Value = serde_json::from_slice(
                &fs::read(record.resources.join("controller-source.json"))
                    .context("checkout controller snapshot missing; run make dev-build")?,
            )?;
            ensure!(
                manifest["format_version"] == 1 && manifest["sha256"] == controller_sha256,
                "controller source snapshot changed; run make dev-build"
            );
            Ok(Self {
                installation,
                root: record.resources,
                executable,
                executable_sha256,
                allow_development: true,
                controller_sha256: Some(controller_sha256),
            })
        } else {
            let root = executable
                .parent()
                .and_then(Path::parent)
                .context("cannot locate release bundle")?
                .to_path_buf();
            let manifest = crate::installer::verify(&root, allow_development, true)?;
            let name = executable
                .strip_prefix(&root)?
                .to_str()
                .context("non-UTF-8 executable")?;
            let executable_sha256 = manifest["files"][name]["sha256"]
                .as_str()
                .context("executable is not a verified bundle member")?
                .to_owned();
            Ok(Self {
                installation,
                root,
                executable,
                executable_sha256,
                allow_development: allow_development || manifest["channel"] == "alpha",
                controller_sha256: None,
            })
        }
    }
}

impl Checkout {
    fn verify(&self, executable: &Path) -> Result<()> {
        ensure!(
            executable == self.cli || executable == self.mcp,
            "this executable is not registered to the selected checkout installation; use its development launcher"
        );
        ensure!(
            hash(&self.cli)? == self.cli_sha256 && hash(&self.mcp)? == self.mcp_sha256,
            "checkout binaries changed; run make dev-build to register a coherent build"
        );
        ensure!(
            self.metadata == crate::release::describe(),
            "checkout metadata differs from this executable; rebuild both CLI and MCP together"
        );
        let mut files = BTreeMap::new();
        inventory(&self.resources, &self.resources, &mut files)?;
        ensure!(
            files == self.files,
            "checkout resources changed; run make dev-build"
        );
        Ok(())
    }
}

/// Enforce checkout identity on every selected CLI/MCP startup. Returns false for
/// ordinary installations; it never relaxes the release bundle verifier.
pub fn check_checkout(home: &Path) -> Result<bool> {
    let Some(record) = read(home)? else {
        return Ok(false);
    };
    record.verify(&std::env::current_exe()?.canonicalize()?)?;
    Ok(true)
}

/// Resources for either artifact source; no assumption about checkout bin layout.
pub fn root(home: &Path) -> Result<PathBuf> {
    if let Some(record) = read(home)? {
        record.verify(&std::env::current_exe()?.canonicalize()?)?;
        Ok(record.resources)
    } else {
        std::env::current_exe()?
            .canonicalize()?
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .context("cannot locate release bundle")
    }
}

/// Returns whether verified artifacts permit a preview controller. Published
/// alpha bundles do, without requiring a user-facing development override.
pub(crate) fn verify(home: &Path, root: &Path, allow_development: bool) -> Result<bool> {
    if let Some(record) = read(home)? {
        record.verify(&std::env::current_exe()?.canonicalize()?)?;
        ensure!(
            root == record.resources,
            "resource root differs from checkout registration"
        );
        Ok(true)
    } else {
        let manifest = crate::installer::verify(root, allow_development, true)?;
        Ok(allow_development || manifest["channel"] == "alpha")
    }
}

pub(crate) fn checkout_mcp(home: &Path) -> Result<Option<PathBuf>> {
    read(home)?
        .map(|record| {
            record.verify(&std::env::current_exe()?.canonicalize()?)?;
            Ok(record.mcp)
        })
        .transpose()
}

/// Explicitly registered build output, never an arbitrary directory from HTTP.
pub(crate) fn web_dist(home: &Path) -> Result<Option<PathBuf>> {
    read(home)?
        .map(|record| {
            ensure!(
                fs::symlink_metadata(&record.web_dist)?.is_dir(),
                "linked web output refused"
            );
            Ok(record.web_dist)
        })
        .transpose()
}

/// Checkout controller input is immutable registered build output, not the live tree.
pub(crate) fn controller_source(home: &Path) -> Result<Option<(PathBuf, String)>> {
    let Some(record) = read(home)? else {
        return Ok(None);
    };
    record.verify(&std::env::current_exe()?.canonicalize()?)?;
    let root = record.resources.join("controller-source");
    let manifest: Value = serde_json::from_slice(
        &fs::read(record.resources.join("controller-source.json"))
            .context("checkout controller snapshot missing; run make dev-build")?,
    )?;
    let sha = tree_sha256(&root)?;
    ensure!(
        manifest["format_version"] == 1 && manifest["sha256"] == sha,
        "controller source snapshot changed; run make dev-build"
    );
    Ok(Some((root, sha)))
}

/// Explicit contributor action. No Docker, cluster mutation, grants, or agent
/// attachment. Installation identity survives subsequent builds.
pub fn register(
    home: &Path,
    source: &Path,
    resources: &Path,
    mcp: &Path,
    web_dist: &Path,
) -> Result<Value> {
    let source = source.canonicalize()?;
    let resources = resources.canonicalize()?;
    let web_dist = web_dist.canonicalize()?;
    ensure!(
        web_dist.is_dir() && web_dist.join("index.html").is_file(),
        "build checkout web assets first"
    );
    let cli = std::env::current_exe()?.canonicalize()?;
    let mcp = mcp.canonicalize()?;
    ensure!(
        source.join("crates/proofstorm-app/Cargo.toml").is_file(),
        "select a Proofstorm checkout"
    );
    ensure!(cli != mcp, "CLI and MCP executables must be distinct");
    let metadata = crate::release::describe();
    let supplied: Value = serde_json::from_slice(&fs::read(resources.join("release-info.json"))?)?;
    ensure!(
        supplied == metadata,
        "checkout resources were generated by a different CLI build"
    );
    let peer: Value = serde_json::from_str(
        &crate::harness::launch::capture(&mcp, &["--release-info"])
            .context("inspect checkout MCP build")?,
    )?;
    ensure!(
        peer == metadata,
        "checkout CLI and MCP build metadata differ"
    );
    let mut files = BTreeMap::new();
    inventory(&resources, &resources, &mut files)?;
    for required in [
        "release-info.json",
        "chart/Chart.yaml",
        "chart/values.yaml",
        "chart/crds/proofstorm.dev_proofstormlabs.yaml",
        "chart/crds/proofstorm.dev_proofstormlabactions.yaml",
        "chart/crds/proofstorm.dev_proofstormcandidatebuilds.yaml",
    ] {
        ensure!(
            files.contains_key(required),
            "missing checkout resource: {required}"
        );
    }
    let installation = crate::installation::Installation::initialize(home, None, None)?;
    let _guard = crate::installation::Installation::lock(&installation.home)?;
    if let Some(previous) = read(&installation.home)? {
        ensure!(
            previous.source == source && previous.cli == cli && previous.mcp == mcp,
            "registration would replace another checkout or toolchain; select a new development home"
        );
    } else {
        ensure!(
            !installation.home.join("runtime-owner.json").exists()
                && !installation.database().exists(),
            "cannot adopt existing runtime/state as a checkout installation; select a new development home"
        );
    }
    let record = Checkout {
        format_version: 1,
        installation_id: installation.id.clone(),
        source,
        cli_sha256: hash(&cli)?,
        mcp_sha256: hash(&mcp)?,
        cli,
        mcp,
        resources,
        web_dist,
        files,
        metadata,
    };
    let mut file = tempfile::NamedTempFile::new_in(&installation.home)?;
    file.write_all(&serde_json::to_vec_pretty(&record)?)?;
    file.as_file().sync_all()?;
    file.persist(installation.home.join(RECORD))?;
    Ok(
        json!({"registered":true,"source":"checkout","home":installation.home,"installation_id":installation.id,
        "cli":record.cli,"mcp":record.mcp,"resources":record.resources,"runtime_started":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        root: tempfile::TempDir,
        home: PathBuf,
        source: PathBuf,
        resources: PathBuf,
        mcp: PathBuf,
        web: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let source = base.join("source");
            let resources = base.join("resources");
            let web = base.join("web");
            fs::create_dir_all(source.join("crates/proofstorm-app")).unwrap();
            fs::write(source.join("crates/proofstorm-app/Cargo.toml"), "fixture").unwrap();
            fs::create_dir_all(resources.join("chart/crds")).unwrap();
            for name in [
                "Chart.yaml",
                "values.yaml",
                "crds/proofstorm.dev_proofstormlabs.yaml",
                "crds/proofstorm.dev_proofstormlabactions.yaml",
                "crds/proofstorm.dev_proofstormcandidatebuilds.yaml",
            ] {
                fs::write(resources.join("chart").join(name), "fixture").unwrap();
            }
            let metadata = crate::release::describe().to_string();
            fs::write(resources.join("release-info.json"), &metadata).unwrap();
            let mcp = base.join("proofstorm-mcp");
            fs::write(
                &mcp,
                format!(
                    "#!/bin/sh\ncat <<'PROOFSTORM_METADATA'\n{metadata}\nPROOFSTORM_METADATA\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&mcp, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(&web).unwrap();
            fs::write(web.join("index.html"), "first build").unwrap();
            Self {
                root,
                home: base.join("home"),
                source,
                resources,
                mcp,
                web,
            }
        }
        fn register(&self) -> Result<Value> {
            register(
                &self.home,
                &self.source,
                &self.resources,
                &self.mcp,
                &self.web,
            )
        }
    }

    #[test]
    fn gui_verification_reuses_receipts_but_never_caches_between_requests() {
        let fixture = Fixture::new();
        let source = fixture.resources.join("controller-source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("Dockerfile.proofstormd"), "recipe").unwrap();
        let sha = tree_sha256(&source).unwrap();
        fs::write(
            fixture.resources.join("controller-source.json"),
            json!({"format_version":1,"sha256":sha}).to_string(),
        )
        .unwrap();
        fixture.register().unwrap();
        let verified = Verified::load(&fixture.home, false).unwrap();
        assert_eq!(verified.root, fixture.resources);
        assert_eq!(verified.controller_sha256.as_deref(), Some(sha.as_str()));
        assert_eq!(
            verified.executable_sha256,
            hash(&verified.executable).unwrap()
        );
        assert!(verified.allow_development);
        // A fresh request must reject edits, even when the previous request verified.
        fs::write(source.join("Dockerfile.proofstormd"), "tampered").unwrap();
        assert!(Verified::load(&fixture.home, false).is_err());
        fs::write(source.join("Dockerfile.proofstormd"), "recipe").unwrap();
        fs::write(&fixture.mcp, "tampered peer").unwrap();
        assert!(Verified::load(&fixture.home, false).is_err());
    }

    #[test]
    fn controller_snapshot_is_bound_to_registration_and_detects_tampering() {
        let fixture = Fixture::new();
        let source = fixture.resources.join("controller-source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("Dockerfile.proofstormd"), "recorded recipe").unwrap();
        let sha = tree_sha256(&source).unwrap();
        fs::write(
            fixture.resources.join("controller-source.json"),
            json!({"format_version":1,"sha256":sha}).to_string(),
        )
        .unwrap();
        fixture.register().unwrap();
        assert_eq!(
            controller_source(&fixture.home).unwrap(),
            Some((source.clone(), sha))
        );
        fs::write(source.join("Dockerfile.proofstormd"), "changed recipe").unwrap();
        assert!(controller_source(&fixture.home).is_err());
    }

    #[test]
    fn checkout_registration_preserves_identity_and_state_on_rebuild() {
        let fixture = Fixture::new();
        let first = fixture.register().unwrap();
        assert_eq!(first["runtime_started"], false);
        assert!(check_checkout(&fixture.home).unwrap());
        let installation = crate::installation::Installation::load(&fixture.home).unwrap();
        fs::write(installation.database(), "existing state").unwrap();
        fs::write(
            fixture.resources.join("chart/values.yaml"),
            "rebuilt resources",
        )
        .unwrap();
        assert!(check_checkout(&fixture.home).is_err());
        let second = fixture.register().unwrap();
        assert_eq!(first["installation_id"], second["installation_id"]);
        assert_eq!(
            fs::read_to_string(installation.database()).unwrap(),
            "existing state"
        );
        assert!(check_checkout(&fixture.home).unwrap());
        assert_eq!(root(&fixture.home).unwrap(), fixture.resources);
        assert_eq!(
            checkout_mcp(&fixture.home).unwrap(),
            Some(fixture.mcp.clone())
        );
        assert!(verify(&fixture.home, &fixture.resources, false).unwrap());
        assert!(verify(&fixture.home, fixture.root.path(), true).is_err());
    }

    #[test]
    fn checkout_rejects_stale_binaries_and_foreign_executables_but_allows_web_watch() {
        let fixture = Fixture::new();
        fixture.register().unwrap();
        fs::write(fixture.web.join("index.html"), "live web rebuild").unwrap();
        assert!(check_checkout(&fixture.home).unwrap());
        let record = read(&fixture.home).unwrap().unwrap();
        assert!(record.verify(Path::new("/foreign/proofstorm")).is_err());
        fs::write(&fixture.mcp, "different binary").unwrap();
        assert!(check_checkout(&fixture.home).is_err());
        assert!(fixture.register().is_err());
    }

    #[test]
    fn checkout_refuses_mismatched_metadata_and_existing_unregistered_state() {
        let fixture = Fixture::new();
        fs::write(fixture.resources.join("release-info.json"), "{}").unwrap();
        assert!(fixture.register().is_err());
        assert!(!fixture.home.exists());
        fs::write(
            fixture.resources.join("release-info.json"),
            crate::release::describe().to_string(),
        )
        .unwrap();
        let installation =
            crate::installation::Installation::initialize(&fixture.home, None, None).unwrap();
        fs::write(installation.database(), "do not adopt").unwrap();
        assert!(fixture.register().is_err());
        assert!(!fixture.home.join(RECORD).exists());
    }

    #[test]
    fn checkout_refuses_linked_resources_and_non_private_registration() {
        let fixture = Fixture::new();
        fixture.register().unwrap();
        std::os::unix::fs::symlink(&fixture.mcp, fixture.resources.join("linked")).unwrap();
        assert!(check_checkout(&fixture.home).is_err());
        fs::remove_file(fixture.resources.join("linked")).unwrap();
        fs::set_permissions(fixture.home.join(RECORD), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_checkout(&fixture.home).is_err());
        assert!(!check_checkout(&fixture.root.path().join("ordinary-home")).unwrap());
    }
}
