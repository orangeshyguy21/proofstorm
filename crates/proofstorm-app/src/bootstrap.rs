//! Installed runtime orchestration. Never selects the contributor context.
mod cluster;
mod process;
mod tools;

use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn controller() -> Result<Value> {
    let value: Value = serde_json::from_str(include_str!("../../../release/controller.json"))?;
    let image = value["image"]
        .as_str()
        .context("this build has no published controller; install a newer bundle")?;
    ensure!(
        image
            .strip_prefix("ghcr.io/orangeshyguy21/proofstorm/proofstormd@sha256:")
            .is_some_and(digest),
        "controller must be pinned to the confirmed registry"
    );
    ensure!(
        value["metadata"]["runtime_contract_sha256"] == crate::release::runtime_contract_sha256()
            && value["metadata"]["version"] == env!("CARGO_PKG_VERSION"),
        "controller/client compatibility mismatch; install a matching bundle"
    );
    Ok(value)
}

fn images() -> BTreeSet<String> {
    let mut images: BTreeSet<_> = proofstorm_core::default_catalog()
        .entries
        .iter()
        .map(|entry| entry.image.clone())
        .collect();
    images.extend(
        proofstorm_kube::images::HELPER_IMAGES
            .iter()
            .map(ToString::to_string),
    );
    images
}

fn source(image: &str) -> Result<String> {
    let (repository, sha) = image
        .split_once("@sha256:")
        .context("runtime image must be pinned")?;
    ensure!(digest(sha), "invalid runtime image digest");
    let source = if let Some(repository) =
        repository.strip_prefix("proofstorm-registry.localhost:5000/upstream/")
    {
        repository.to_owned()
    } else if let Some(repository) = repository.strip_prefix("proofstorm-registry.localhost:5000/")
    {
        format!("ghcr.io/orangeshyguy21/proofstorm/{repository}")
    } else {
        repository.to_owned()
    };
    Ok(format!("{source}@sha256:{sha}"))
}

fn tool(home: &Path, name: &str) -> Result<PathBuf> {
    tools::verified(
        home,
        &tools::pins()?
            .into_iter()
            .find(|tool| tool.name == name)
            .context("unknown helper")?,
    )
}

fn docker(home: &Path, args: &[&str], seconds: u64) -> Result<String> {
    process::run(home, Path::new("docker"), args, seconds)
}

fn kube(installation: &Installation, args: &[&str]) -> Result<String> {
    cluster::verify_kubeconfig(installation)?;
    let config = installation.kubeconfig();
    let context = installation.context();
    let mut all = vec![
        "--kubeconfig",
        config.to_str().context("non-UTF-8 home")?,
        "--context",
        &context,
        "--request-timeout=30s",
    ];
    all.extend(args);
    process::run(
        &installation.home,
        &tool(&installation.home, "kubectl")?,
        &all,
        150,
    )
}

fn preflight(home: &Path) -> Result<Value> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "setup currently supports macOS Apple Silicon"
    );
    let info: Value =
        serde_json::from_str(&docker(home, &["info", "--format", "{{json .}}"], 20)?)?;
    ensure!(
        info["OSType"] == "linux"
            && matches!(info["Architecture"].as_str(), Some("aarch64" | "arm64")),
        "Docker must run Linux arm64 containers"
    );
    let buildx = docker(home, &["buildx", "version"], 15)?;
    docker(home, &["buildx", "imagetools", "create", "--help"], 15).and_then(|help| {
        ensure!(
            help.contains("--prefer-index"),
            "Docker buildx lacks digest-preserving copy support"
        );
        Ok(())
    })?;
    let ancestor = home
        .ancestors()
        .find(|path| path.exists())
        .context("no existing installation parent")?;
    let disk = process::run(
        home,
        Path::new("df"),
        &["-Pk", ancestor.to_str().context("non-UTF-8 home")?],
        10,
    )?;
    ensure!(
        info["MemTotal"].as_u64().is_some_and(|n| n > 0),
        "cannot determine Docker memory allocation"
    );
    Ok(
        json!({"docker_memory_bytes":info["MemTotal"],"docker_cpus":info["NCPU"],"buildx":buildx.trim(),"disk":disk.trim(),
        "resource_note":"Observed capacity only; minimum lab requirements have not yet been measured."}),
    )
}

/// Read-only checks, including before an installation exists. No grants or state writes.
pub fn check_installed_runtime(installation: &Installation) -> Result<()> {
    let controller = controller()?;
    cluster::owned(installation)?;
    healthy(installation, &controller)
}

/// Read-only checks, including before an installation exists. No grants or state writes.
#[must_use]
pub fn doctor(home: &Path) -> Value {
    let mut checks = Vec::new();
    let mut check = |name: &str, result: Result<Value>| match result {
        Ok(details) => checks.push(json!({"name":name,"ok":true,"details":details})),
        Err(error) => checks.push(json!({"name":name,"ok":false,"message":format!("{error:#}")})),
    };
    check("docker", preflight(home));
    check("controller_compatibility", controller());
    check(
        "tools",
        (|| {
            for pin in tools::pins()? {
                tools::verified(home, &pin)?;
            }
            Ok(json!({"checksums_verified":true}))
        })(),
    );
    check(
        "runtime",
        (|| {
            let installation = Installation::load(home)?;
            cluster::owned(&installation)?;
            healthy(&installation, &controller()?)?;
            Ok(json!({"cluster":installation.cluster_name(),"controller_ready":true}))
        })(),
    );
    json!({"ok":checks.iter().all(|c| c["ok"] == true),"checks":checks,
        "mcp_server":"not checked","harness":"not checked","image_pulls":"not checked by read-only doctor; lab creation verifies selected pulls (setup --prefetch-all verifies the full catalog)"})
}

/// Setup is explicit, serialized per home, and reconciles each stage on retry.
pub fn setup(
    home: &Path,
    bundle: &Path,
    allow_development: bool,
    prepare_only: bool,
    prefetch_all: bool,
) -> Result<Value> {
    ensure!(
        home.is_absolute() && !home.as_os_str().is_empty(),
        "setup requires an absolute --home"
    );
    let allow_development = crate::artifacts::verify(home, bundle, allow_development)?;
    let controller = controller()?;
    ensure!(
        allow_development || controller["release_ready"] == true,
        "controller is development-only; local tests require --allow-development"
    );
    let capacity = preflight(home)?;
    let installation = Installation::initialize(home, None, None)?;
    let home = &installation.home;
    let _guard = Installation::lock(home)?;
    stage(home, "tools", || {
        for pin in tools::pins()? {
            tools::install(home, &pin)?;
        }
        Ok(())
    })?;
    if prepare_only {
        return Ok(
            json!({"prepared":true,"runtime_started":false,"home":home,"capacity":capacity}),
        );
    }
    stage(home, "controller", || {
        let image = controller["image"]
            .as_str()
            .context("controller image missing")?;
        docker(home, &["pull", "--platform", "linux/arm64", image], 300)?;
        let metadata: Value = serde_json::from_str(&docker(
            home,
            &[
                "run",
                "--rm",
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
                image,
                "--release-info",
            ],
            30,
        )?)?;
        ensure!(
            metadata == controller["metadata"],
            "downloaded controller metadata mismatch"
        );
        Ok(())
    })?;
    stage(home, "cluster", || cluster::create(&installation))?;
    if prefetch_all {
        stage(home, "images", || mirror(&installation, images()))?;
    }
    stage(home, "deployment", || {
        deploy(&installation, bundle, &controller)
    })?;
    stage(home, "health", || healthy(&installation, &controller))?;
    stage(home, "permissions", || {
        initialize_permissions(&installation)
    })?;
    Ok(
        json!({"ready":true,"home":home,"cluster":installation.cluster_name(),"capacity":capacity,
        "permissions_initialized":true,"image_policy":if prefetch_all {"prefetch_all"} else {"on_demand"},
        "next":"Runtime ready. Run proofstorm gui, or proofstorm open codex, proofstorm open opencode, or proofstorm open claude from your project. Create a lab with proofstorm up FILE; its images download on first use."}),
    )
}

fn initialize_permissions(installation: &Installation) -> Result<()> {
    let database = installation.database();
    if let Ok(metadata) = std::fs::symlink_metadata(&database) {
        ensure!(
            metadata.is_file(),
            "refusing linked or foreign database path"
        );
        return Ok(()); // Never replace or silently regrant an existing principal.
    }
    let file = tempfile::NamedTempFile::new_in(&installation.home)?;
    {
        let store = proofstorm_store::Store::open(file.path())?;
        crate::developer::configure(&store, crate::config::DEFAULT_WORKSPACE, "developer")?;
    } // Last SQLite connection closes/checkpoints before activating the file.
    file.persist_noclobber(database)?;
    Ok(())
}

fn stage(home: &Path, name: &str, action: impl FnOnce() -> Result<()>) -> Result<()> {
    eprintln!("Proofstorm setup: {name}");
    let path = home.join("setup-progress.json");
    process::save(
        &path,
        &serde_json::to_vec(&json!({"stage":name,"status":"running"}))?,
    )?;
    let result = action();
    process::save(
        &path,
        &serde_json::to_vec(
            &json!({"stage":name,"status":if result.is_ok() {"complete"} else {"failed"}}),
        )?,
    )?;
    result.with_context(|| format!("setup stage {name} failed; rerun the same setup command to retry (no resources were deleted)"))
}

/// Only explicit, authorized lab mutations call this. Reads never start downloads.
pub(crate) async fn prepare_images(
    installation: Installation,
    lock: proofstorm_core::ResolvedLock,
) -> Result<()> {
    let selected = selected_images(&installation, &lock)?;
    tokio::task::spawn_blocking(move || {
        let _guard = Installation::lock(&installation.home)?;
        ensure!(
            Installation::load(&installation.home)? == installation,
            "installation identity changed"
        );
        mirror(&installation, selected)
    })
    .await?
}

fn selected_images(
    installation: &Installation,
    lock: &proofstorm_core::ResolvedLock,
) -> Result<BTreeSet<String>> {
    let shipped = images();
    let candidate_prefixes = [
        "proofstorm-registry.localhost:5000/candidates/".to_owned(),
        format!("{}:5000/candidates/", installation.registry_name()),
    ];
    let mut selected = BTreeSet::from([proofstorm_kube::images::PROBE_IMAGE.to_owned()]);
    for entry in &lock.entries {
        // A stored lock may also select a candidate built into this private registry.
        // Never attempt to fetch candidate names from the public publisher namespace.
        ensure!(
            shipped.contains(&entry.image)
                || (entry.source.is_some()
                    && candidate_prefixes
                        .iter()
                        .any(|p| entry.image.starts_with(p))),
            "lab image is neither shipped nor an installation-local candidate"
        );
        source(&entry.image)?; // Require an immutable digest for every image.
        selected.insert(entry.image.clone());
    }
    Ok(selected)
}

fn mirror(installation: &Installation, selected: BTreeSet<String>) -> Result<()> {
    cluster::owned(installation)?;
    cluster::verify_kubeconfig(installation)?;
    for image in selected {
        eprintln!("Checking image: {image}");
        if let Some(local) = image
            .strip_prefix("proofstorm-registry.localhost:5000/")
            .filter(|local| !local.starts_with("candidates/"))
        {
            let (repository, sha) = local
                .split_once("@sha256:")
                .context("unpinned catalog image")?;
            let destination = format!("{}/{repository}@sha256:{sha}", installation.host_registry());
            if docker(
                &installation.home,
                &["buildx", "imagetools", "inspect", &destination],
                30,
            )
            .is_err()
            {
                let tag = format!(
                    "{}/{repository}:catalog-{sha}",
                    installation.host_registry()
                );
                docker(
                    &installation.home,
                    &[
                        "buildx",
                        "imagetools",
                        "create",
                        "--prefer-index=false",
                        "--tag",
                        &tag,
                        &source(&image)?,
                    ],
                    900,
                )?;
            }
            let manifest: Value = serde_json::from_str(&docker(
                &installation.home,
                &[
                    "buildx",
                    "imagetools",
                    "inspect",
                    &destination,
                    "--format",
                    "{{json .Manifest}}",
                ],
                30,
            )?)?;
            ensure!(
                manifest["digest"] == format!("sha256:{sha}"),
                "mirror changed the catalog digest"
            );
        }
        for node in cluster::nodes(installation) {
            cluster::owned(installation)?;
            docker(
                &installation.home,
                &["exec", &node, "crictl", "--timeout=120s", "pull", &image],
                150,
            )?;
        }
    }
    Ok(())
}

fn deploy(installation: &Installation, bundle: &Path, controller: &Value) -> Result<()> {
    cluster::owned(installation)?;
    // Check existing objects before applying schemas or touching the controller.
    let crds: Value = serde_json::from_str(&kube(installation, &["get", "crds", "-o", "json"])?)?;
    if crds["items"]
        .as_array()
        .context("CRD list missing")?
        .iter()
        .any(|item| item["metadata"]["name"] == "proofstormlabs.proofstorm.dev")
    {
        let labs: Value = serde_json::from_str(&kube(
            installation,
            &["get", "proofstormlabs", "-A", "-o", "json"],
        )?)?;
        for lab in labs["items"].as_array().context("lab list missing")? {
            serde_json::from_value::<proofstorm_kube::ProofstormLabSpec>(lab["spec"].clone())
                .context(
                    "existing lab schema is incompatible; no automatic migration or deletion",
                )?;
        }
    }
    kube(
        installation,
        &[
            "apply",
            "--server-side",
            "--field-manager=proofstorm-installed",
            "-f",
            bundle.join("chart/crds").to_str().context("chart path")?,
        ],
    )?;
    let image = controller["image"].as_str().context("controller image")?;
    let (repository, digest) = image.split_once('@').context("controller digest")?;
    let expected = format!("{repository}@{digest}");
    // Skip Helm entirely on a healthy, identical deployment: no restarts/revisions.
    if healthy(installation, controller).is_ok() {
        return Ok(());
    }
    process::run(
        &installation.home,
        &tool(&installation.home, "helm")?,
        &[
            "upgrade",
            "--install",
            "proofstorm",
            bundle.join("chart").to_str().context("chart path")?,
            "--kubeconfig",
            installation
                .kubeconfig()
                .to_str()
                .context("kubeconfig path")?,
            "--kube-context",
            &installation.context(),
            "--namespace",
            "proofstorm-system",
            "--create-namespace",
            "--skip-crds",
            "--set-string",
            &format!("image.repository={repository}"),
            "--set-string",
            &format!("image.digest={digest}"),
            "--wait",
            "--timeout",
            "120s",
            "--rollback-on-failure",
        ],
        150,
    )?;
    ensure!(!expected.is_empty(), "missing deployment image");
    Ok(())
}

fn healthy(installation: &Installation, controller: &Value) -> Result<()> {
    let deployment: Value = serde_json::from_str(&kube(
        installation,
        &[
            "get",
            "deployment/proofstormd",
            "-n",
            "proofstorm-system",
            "-o",
            "json",
        ],
    )?)?;
    ensure!(
        deployment["spec"]["template"]["spec"]["containers"][0]["image"] == controller["image"],
        "controller image differs from this installed bundle"
    );
    ensure!(
        deployment["status"]["observedGeneration"] == deployment["metadata"]["generation"]
            && deployment["status"]["readyReplicas"] == 1
            && deployment["status"]["availableReplicas"] == 1,
        "controller is not ready; inspect the isolated runtime or retry setup"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
