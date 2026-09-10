//! Installed runtime orchestration. Never selects the contributor context.
mod cluster;
mod local_controller;
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
    let value = crate::release::controller();
    validate_controller(&value)?;
    Ok(value)
}

fn validate_controller(value: &Value) -> Result<()> {
    let image = value["image"].as_str().with_context(|| {
        format!(
            "this build has no published controller for {}; install a matching bundle",
            crate::platform::container_platform().unwrap_or_default()
        )
    })?;
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
    Ok(())
}

fn selected_controller(home: &Path) -> Result<Value> {
    if let Some((_, sha)) = crate::artifacts::controller_source(home)? {
        local_controller::current(&Installation::load(home)?, &sha)
    } else {
        controller()
    }
}

fn controller_metadata(home: &Path, image: &str) -> Result<Value> {
    Ok(serde_json::from_str(&docker(
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
    )?)?)
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
    let platform = crate::platform::container_platform()?;
    let info: Value =
        serde_json::from_str(&docker(home, &["info", "--format", "{{json .}}"], 20)?)?;
    ensure!(
        crate::platform::docker_matches(
            crate::platform::target(),
            info["OSType"].as_str().unwrap_or_default(),
            info["Architecture"].as_str().unwrap_or_default()
        ),
        "Docker must run {platform} containers for this installation"
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
    let controller = selected_controller(&installation.home)?;
    cluster::owned(installation)?;
    healthy(installation, &controller)
}

/// Startup consumes a verified artifact snapshot; runtime ownership/health remain live checks.
pub(crate) fn check_verified_runtime(
    verified: &crate::artifacts::Verified,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let installation = &verified.installation;
    progress("Checking controller compatibility");
    let controller = match &verified.controller_sha256 {
        Some(sha) => local_controller::current(installation, sha)?,
        None => controller()?,
    };
    progress("Checking runtime ownership");
    cluster::owned(installation)?;
    progress("Checking controller health");
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
    check("controller_compatibility", selected_controller(home));
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
            healthy(&installation, &selected_controller(home)?)?;
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
    setup_with_progress(
        home,
        bundle,
        allow_development,
        prepare_only,
        prefetch_all,
        &|label| eprintln!("{label}..."),
    )
}

/// Setup with caller-owned progress; callbacks never change runtime behavior.
pub fn setup_with_progress(
    home: &Path,
    bundle: &Path,
    allow_development: bool,
    prepare_only: bool,
    prefetch_all: bool,
    progress: &dyn Fn(&str),
) -> Result<Value> {
    ensure!(
        home.is_absolute() && !home.as_os_str().is_empty(),
        "setup requires an absolute --home"
    );
    let allow_development = crate::artifacts::verify(home, bundle, allow_development)?;
    let checkout_source = crate::artifacts::controller_source(home)?;
    let mut controller = if checkout_source.is_some() || prepare_only {
        Value::Null
    } else {
        controller()?
    };
    if checkout_source.is_none() && !prepare_only {
        ensure!(
            allow_development || controller["release_ready"] == true,
            "controller is development-only; local tests require --allow-development"
        );
    }
    progress("Checking Docker");
    let capacity = preflight(home)?;
    let installation = Installation::initialize(home, None, None)?;
    let home = &installation.home;
    progress("Waiting for installation lock");
    let _guard = Installation::lock(home)?;
    let stage = |name, action: &mut dyn FnMut() -> Result<()>| {
        progress(match name {
            "tools" => "Preparing tools",
            "cluster" => "Preparing local runtime",
            "controller" => "Preparing controller",
            "images" => "Downloading catalog images",
            "deployment" => "Applying runtime configuration",
            "health" => "Checking runtime health",
            "permissions" => "Checking permissions",
            _ => "Preparing Proofstorm",
        });
        stage(home, name, action)
    };
    stage("tools", &mut || {
        for pin in tools::pins()? {
            progress(&format!("Checking {}", pin.name));
            tools::install(home, &pin)?;
        }
        Ok(())
    })?;
    if prepare_only {
        return Ok(
            json!({"prepared":true,"runtime_started":false,"home":home,"capacity":capacity}),
        );
    }
    stage("cluster", &mut || cluster::create(&installation, progress))?;
    stage("controller", &mut || {
        if let Some((source, sha)) = &checkout_source {
            controller = local_controller::prepare(&installation, source, sha, progress)?;
        } else {
            let image = controller["image"]
                .as_str()
                .context("controller image missing")?;
            progress("Downloading controller image");
            docker(
                home,
                &[
                    "pull",
                    "--platform",
                    &crate::platform::container_platform()?,
                    image,
                ],
                300,
            )?;
            progress("Verifying controller image");
            ensure!(
                controller_metadata(home, image)? == controller["metadata"],
                "downloaded controller metadata mismatch"
            );
        }
        Ok(())
    })?;
    if prefetch_all {
        stage("images", &mut || {
            mirror_with_progress(&installation, images(), progress)
        })?;
    }
    stage("deployment", &mut || {
        deploy(&installation, bundle, &controller, progress)
    })?;
    stage("health", &mut || healthy(&installation, &controller))?;
    stage("permissions", &mut || initialize_permissions(&installation))?;
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
    mirror_with_progress(installation, selected, &|_| {})
}

fn mirror_with_progress(
    installation: &Installation,
    selected: BTreeSet<String>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    cluster::owned(installation)?;
    cluster::verify_kubeconfig(installation)?;
    let total = selected.len();
    for (index, image) in selected.into_iter().enumerate() {
        progress(&format!("Checking catalog image {} of {total}", index + 1));
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

fn deploy(
    installation: &Installation,
    bundle: &Path,
    controller: &Value,
    progress: &dyn Fn(&str),
) -> Result<()> {
    progress("Checking existing runtime configuration");
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
    progress("Applying lab resource schemas");
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
    let inputs = json!({"installation_id":installation.id, "image":expected,
        "chart_sha256":crate::artifacts::tree_sha256(&bundle.join("chart"))?});
    let receipt = installation.home.join("deployment-inputs.json");
    let same_inputs = std::fs::symlink_metadata(&receipt).is_ok_and(|meta| meta.is_file())
        && std::fs::read(&receipt)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .as_ref()
            == Some(&inputs);
    // Skip Helm entirely on a healthy, identical deployment: no restarts/revisions.
    if same_inputs && healthy(installation, controller).is_ok() {
        progress("Reusing healthy controller");
        return Ok(());
    }
    progress("Waiting for controller to become ready");
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
    healthy(installation, controller)?;
    process::save(&receipt, &serde_json::to_vec(&inputs)?)?;
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
