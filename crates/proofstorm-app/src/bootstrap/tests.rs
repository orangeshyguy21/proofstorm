use super::*;
use std::fs;

fn sample_lock() -> proofstorm_core::ResolvedLock {
    let spec = serde_json::from_str::<proofstorm_core::LabSpec>(include_str!(
        "../../../../examples/developer-lab.json"
    ))
    .unwrap();
    let catalog = proofstorm_core::default_catalog();
    let effective = proofstorm_core::resolve_effective_lab(&spec, catalog).unwrap();
    proofstorm_core::resolve_lock(&effective, catalog).unwrap()
}

fn fixture_installation(home: &Path) -> Installation {
    Installation {
        format_version: 1,
        id: "d".repeat(32),
        home: home.into(),
        api_port: 42101,
        registry_port: 42102,
    }
}

#[test]
fn on_demand_selects_only_locked_components_and_probe() {
    let home = tempfile::tempdir().unwrap();
    let installation = fixture_installation(home.path());
    let mut lock = sample_lock();
    lock.entries.push(lock.entries[0].clone());
    let selected = selected_images(&installation, &lock).unwrap();
    assert_eq!(selected.len(), 3);
    assert!(selected.contains(proofstorm_kube::images::PROBE_IMAGE));
    assert!(!selected.contains(proofstorm_kube::images::BUILDKIT_IMAGE));
    assert!(!selected.contains(proofstorm_kube::images::GIT_IMAGE));
    assert!(selected.len() < images().len());
    for entry in &lock.entries {
        assert!(selected.contains(&entry.image));
    }
    assert!(fs::read_dir(home.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn unshipped_or_mutable_images_fail_before_home_or_docker_access() {
    let home = tempfile::tempdir().unwrap();
    let installation = fixture_installation(&home.path().join("must-not-exist"));
    for image in [
        "example.org/untrusted@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "proofstorm-registry.localhost:5000/bitcoin-core:latest",
        "proofstorm-registry.localhost:5000/candidates/unowned@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let mut lock = sample_lock();
        lock.entries[0].image = image.into();
        assert!(prepare_images(installation.clone(), lock).await.is_err());
        assert!(!installation.home.exists());
    }
}

#[test]
fn public_sources_preserve_every_shipped_digest() {
    for image in images() {
        let public = source(&image).unwrap();
        assert!(!public.contains("proofstorm-registry.localhost"));
        assert_eq!(
            image.split_once('@').unwrap().1,
            public.split_once('@').unwrap().1
        );
    }
    assert!(source("example.org/image:latest").is_err());
    assert!(source("example.org/image@sha256:bad").is_err());
}

#[test]
fn bootstrap_pins_are_complete_and_immutable() {
    let pins = tools::pins().unwrap();
    assert_eq!(pins.len(), 3);
    assert!(
        pins.iter()
            .all(|pin| pin.url.starts_with("https://") && digest(&pin.sha256))
    );
}

#[test]
fn published_controller_matches_compiled_contract() {
    if crate::platform::target() == crate::platform::LINUX_AMD64 {
        assert!(crate::release::controller().is_null());
        assert!(
            controller()
                .unwrap_err()
                .to_string()
                .contains("linux/amd64")
        );
        return;
    }
    let info = controller().unwrap();
    assert_eq!(
        info["metadata"]["runtime_contract_sha256"],
        crate::release::runtime_contract_sha256()
    );
    assert_eq!(info["release_ready"], false);
}

#[test]
fn failed_stage_is_recorded_without_sensitive_error_and_can_be_retried() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        stage(home.path(), "images", || anyhow::bail!(
            "private failure details"
        ))
        .is_err()
    );
    let progress = home.path().join("setup-progress.json");
    let receipt = fs::read_to_string(&progress).unwrap();
    assert!(receipt.contains("failed"));
    assert!(!receipt.contains("private failure details"));
    stage(home.path(), "images", || Ok(())).unwrap();
    assert!(fs::read_to_string(progress).unwrap().contains("complete"));
}

#[test]
fn changed_private_kubeconfig_is_refused_before_any_cluster_call() {
    let home = tempfile::tempdir().unwrap();
    let installation = Installation {
        format_version: 1,
        id: "a".repeat(32),
        home: home.path().canonicalize().unwrap(),
        api_port: 42101,
        registry_port: 42102,
    };
    let config = installation.kubeconfig();
    fs::write(&config, "private fixture").unwrap();
    process::save(
        &home.path().join("runtime-owner.json"),
        &serde_json::to_vec(&json!({
            "installation_id":installation.id,"kubeconfig_sha256":tools::hash(&config).unwrap()
        }))
        .unwrap(),
    )
    .unwrap();
    cluster::verify_kubeconfig(&installation).unwrap();
    fs::write(&config, "foreign context").unwrap();
    assert!(cluster::verify_kubeconfig(&installation).is_err());
    assert!(kube(&installation, &["get", "namespaces"]).is_err());
}

#[test]
fn damaged_helpers_fail_without_adoption_or_download() {
    let home = tempfile::tempdir().unwrap();
    let pin = tools::pins().unwrap().remove(0);
    fs::create_dir(home.path().join("tools")).unwrap();
    let file = tools::path(home.path(), &pin);
    fs::write(&file, "unrelated executable").unwrap();
    assert!(tools::install(home.path(), &pin).is_err());
    assert_eq!(fs::read_to_string(file).unwrap(), "unrelated executable");
}

#[test]
fn state_writes_refuse_symlinks() {
    let home = tempfile::tempdir().unwrap();
    let foreign = home.path().join("foreign");
    fs::write(&foreign, "keep").unwrap();
    let link = home.path().join("setup-progress.json");
    std::os::unix::fs::symlink(&foreign, &link).unwrap();
    assert!(process::save(&link, b"replace").is_err());
    assert_eq!(fs::read_to_string(foreign).unwrap(), "keep");
}

#[test]
fn retry_does_not_regrant_revoked_permissions() {
    let home = tempfile::tempdir().unwrap();
    let installation = Installation {
        format_version: 1,
        id: "b".repeat(32),
        home: home.path().canonicalize().unwrap(),
        api_port: 42103,
        registry_port: 42104,
    };
    initialize_permissions(&installation).unwrap();
    let store = proofstorm_store::Store::open(installation.database()).unwrap();
    let workspace = crate::config::DEFAULT_WORKSPACE;
    store
        .authorize(
            workspace,
            "developer",
            proofstorm_core::Capability::LabCreate,
        )
        .unwrap();
    store
        .revoke(
            workspace,
            "developer",
            proofstorm_core::Capability::LabCreate,
        )
        .unwrap();
    initialize_permissions(&installation).unwrap();
    assert!(
        store
            .authorize(
                workspace,
                "developer",
                proofstorm_core::Capability::LabCreate
            )
            .is_err()
    );
}

#[test]
fn commands_have_a_deadline_and_do_not_print_private_output() {
    let home = tempfile::tempdir().unwrap();
    assert!(process::run(home.path(), Path::new("sh"), &["-c", "exec sleep 2"], 0).is_err());
    let error = process::run(
        home.path(),
        Path::new("sh"),
        &["-c", "echo private-secret >&2; exit 1"],
        2,
    )
    .unwrap_err();
    assert!(!error.to_string().contains("private-secret"));
}
