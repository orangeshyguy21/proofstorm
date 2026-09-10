use super::*;
use crate::config::Environment;

#[test]
fn installations_have_distinct_identity_ports_and_registry_routes() {
    let root = tempfile::tempdir().unwrap();
    let first = Installation::initialize(&root.path().join("first"), None, None).unwrap();
    let second = Installation::initialize(&root.path().join("second"), None, None).unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.cluster_name(), second.cluster_name());
    assert_ne!(first.network_name(), second.network_name());
    assert_ne!(first.registry_name(), second.registry_name());
    assert_ne!(first.database(), second.database());
    assert_ne!(first.kubeconfig(), second.kubeconfig());
    for installation in [&first, &second] {
        assert!(installation.cluster_name().len() <= 32);
        assert_ne!(installation.api_port, installation.registry_port);
        let config = installation.cluster_config();
        assert!(
            config["registries"]["config"]
                .as_str()
                .unwrap()
                .contains('\n')
        );
        let mirrors: Value =
            serde_json::from_str(config["registries"]["config"].as_str().unwrap()).unwrap();
        assert_eq!(
            mirrors["mirrors"][CATALOG_REGISTRY]["endpoint"][0],
            installation.registry_endpoint()
        );
        assert_eq!(
            config["options"]["kubeconfig"]["updateDefaultKubeconfig"],
            false
        );
        assert_eq!(
            config["options"]["kubeconfig"]["switchCurrentContext"],
            false
        );
        assert_eq!(config["kubeAPI"]["hostIP"], "127.0.0.1");
        assert_eq!(config["registries"]["create"]["host"], "127.0.0.1");
    }
}

#[test]
fn initialization_is_idempotent_and_does_not_overwrite_changed_configuration() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    assert_eq!(
        Installation::initialize(root.path(), None, None).unwrap(),
        installation
    );
    fs::write(installation.cluster_config_path(), b"user-owned changes").unwrap();
    assert!(Installation::initialize(root.path(), None, None).is_err());
    assert_eq!(
        fs::read(installation.cluster_config_path()).unwrap(),
        b"user-owned changes"
    );
    assert_eq!(Installation::load(root.path()).unwrap(), installation);
}

#[test]
fn copied_corrupt_and_unknown_manifests_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("first"), None, None).unwrap();
    fs::create_dir(root.path().join("second")).unwrap();
    fs::copy(
        installation.home.join(MANIFEST),
        root.path().join("second").join(MANIFEST),
    )
    .unwrap();
    assert!(Installation::load(&root.path().join("second")).is_err());
    let mut manifest = serde_json::to_value(&installation).unwrap();
    manifest["format_version"] = json!(999);
    fs::write(installation.home.join(MANIFEST), manifest.to_string()).unwrap();
    assert!(Installation::initialize(&installation.home, None, None).is_err());
    fs::write(installation.home.join(MANIFEST), "broken").unwrap();
    assert!(Installation::initialize(&installation.home, None, None).is_err());
    assert_eq!(
        fs::read_to_string(installation.home.join(MANIFEST)).unwrap(),
        "broken"
    );
}

#[test]
fn selected_ports_cannot_collide_or_change_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let occupied = available_port(None).unwrap();
    assert!(
        Installation::initialize(
            root.path(),
            Some(occupied.local_addr().unwrap().port()),
            None
        )
        .is_err()
    );
    assert!(!root.path().join(MANIFEST).exists());
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    assert!(Installation::initialize(root.path(), Some(installation.registry_port), None).is_err());
    assert!(Installation::initialize(root.path(), Some(0), None).is_err());
}

#[test]
fn lock_serializes_independent_connections_and_releases_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let guard = Installation::lock(root.path()).unwrap();
    assert!(Installation::lock(root.path()).is_err());
    drop(guard);
    assert!(Installation::lock(root.path()).is_ok());
}

#[test]
fn home_resolution_ignores_working_directory_and_explicit_flags_win() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    let vars = |key: &str| match key {
        "PROOFSTORM_HOME" => Some(installation.home.to_str().unwrap().to_owned()),
        "PROOFSTORM_PRINCIPAL" => Some("agent".into()),
        _ => None,
    };
    let first = Environment::resolve(vars, Path::new("/a/project")).unwrap();
    let second = Environment::resolve(vars, Path::new("/another/project")).unwrap();
    assert_eq!(first.database, second.database);
    assert_eq!(first.database, installation.database());
    assert_eq!(first.context, installation.context());
    assert_eq!(first.kubeconfig, Some(installation.kubeconfig()));
    let explicit = Environment::resolve(
        |key| match key {
            "PROOFSTORM_DB" => Some("override.sqlite3".into()),
            "PROOFSTORM_CONTEXT" => Some("explicit-context".into()),
            "PROOFSTORM_KUBECONFIG" => Some("explicit-config".into()),
            _ => vars(key),
        },
        Path::new("/explicit"),
    )
    .unwrap();
    assert_eq!(explicit.database, Path::new("/explicit/override.sqlite3"));
    assert_eq!(explicit.context, "explicit-context");
    assert_eq!(
        explicit.kubeconfig,
        Some(PathBuf::from("/explicit/explicit-config"))
    );
}

#[tokio::test]
async fn missing_private_kubeconfig_never_reads_ambient_configuration() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    let environment = Environment::resolve(
        |key| match key {
            "PROOFSTORM_HOME" => Some(installation.home.to_str().unwrap().into()),
            "PROOFSTORM_PRINCIPAL" => Some("agent".into()),
            _ => None,
        },
        Path::new("/unrelated"),
    )
    .unwrap();
    let error = environment.runtime().await.err().unwrap();
    assert!(error.to_string().contains("read selected kubeconfig"));
    assert!(
        error
            .to_string()
            .contains(installation.kubeconfig().to_str().unwrap())
    );
}

#[tokio::test]
async fn explicit_kubeconfig_is_loaded_without_changing_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    let bytes = b"apiVersion: v1\nkind: Config\ncurrent-context: decoy\ncontexts:\n- name: selected\n  context: {cluster: local, user: local}\nclusters:\n- name: local\n  cluster: {server: 'http://127.0.0.1:1'}\nusers:\n- name: local\n  user: {}\n";
    fs::write(&path, bytes).unwrap();
    let environment = Environment::resolve(
        |key| match key {
            "PROOFSTORM_KUBECONFIG" => Some(path.to_str().unwrap().into()),
            "PROOFSTORM_CONTEXT" => Some("selected".into()),
            "PROOFSTORM_PRINCIPAL" => Some("agent".into()),
            _ => None,
        },
        root.path(),
    )
    .unwrap();
    assert_eq!(
        environment
            .kubernetes_config()
            .await
            .unwrap()
            .cluster_url
            .to_string(),
        "http://127.0.0.1:1/"
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn command_arguments_keep_paths_with_spaces_separate_and_pin_kubeconfig() {
    let root = tempfile::tempdir().unwrap();
    let installation =
        Installation::initialize(&root.path().join("home with spaces"), None, None).unwrap();
    let command = installation.creation_command(Path::new("/tools with spaces/k3d"));
    let args = command.get_args().collect::<Vec<_>>();
    assert_eq!(args[3], installation.cluster_config_path());
    assert!(args.contains(&std::ffi::OsStr::new("--kubeconfig-update-default=false")));
    assert!(
        command.get_envs().any(|(key, value)| key == "KUBECONFIG"
            && value == Some(installation.kubeconfig().as_os_str()))
    );
}

#[cfg(unix)]
#[test]
fn new_configuration_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("private"), None, None).unwrap();
    assert_eq!(
        fs::metadata(&installation.home)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for path in [
        installation.home.join(MANIFEST),
        installation.cluster_config_path(),
        installation.home.join("installation-lock.sqlite3"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
