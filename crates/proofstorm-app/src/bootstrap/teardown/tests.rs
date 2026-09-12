use super::*;
use std::path::PathBuf;

fn receipt() -> Resources {
    Resources {
        format_version: 1,
        installation_id: "a".repeat(32),
        daemon_id: "daemon-one".into(),
        containers: BTreeMap::from([(
            "k3d-proofstorm-aaaaaaaa-server-0".into(),
            "original-container".into(),
        )]),
        network_id: Some("original-network".into()),
        volumes: BTreeMap::from([("data".into(), serde_json::json!({"CreatedAt":"original"}))]),
    }
}

#[test]
fn replacement_or_foreign_resources_fail_closed_but_partial_cleanup_can_retry() {
    let expected = receipt();
    unchanged(&expected, &expected).unwrap();
    let mut current = expected.clone();
    current.containers.clear();
    current.network_id = None;
    current.volumes.clear();
    unchanged(&expected, &current).unwrap();
    for field in [
        "identity",
        "daemon",
        "container",
        "network",
        "volume",
        "extra",
    ] {
        let mut current = expected.clone();
        match field {
            "identity" => current.installation_id = "b".repeat(32),
            "daemon" => current.daemon_id = "another-daemon".into(),
            "container" => *current.containers.values_mut().next().unwrap() = "replacement".into(),
            "network" => current.network_id = Some("replacement".into()),
            "volume" => {
                *current.volumes.values_mut().next().unwrap() =
                    serde_json::json!({"CreatedAt":"replacement"});
            }
            _ => {
                current
                    .containers
                    .insert("foreign".into(), "foreign-id".into());
            }
        }
        assert!(unchanged(&expected, &current).is_err(), "{field}");
    }
}

#[test]
fn foreign_network_members_and_volume_users_are_refused() {
    let expected = receipt();
    let run = |args: &[&str]| -> Result<String> {
        Ok(match args {
            ["network", "ls", ..] => "original-network\n".into(),
            ["network", "inspect", _] => r#"[{"Containers":{"foreign-id":{}}}]"#.into(),
            _ => panic!("unexpected call {args:?}"),
        })
    };
    assert!(
        exclusive(&expected, &run)
            .unwrap_err()
            .to_string()
            .contains("foreign container")
    );
    let mut expected = expected;
    expected.network_id = None;
    expected.volumes.insert(
        "data".into(),
        volume_identity(&serde_json::json!({"CreatedAt":"original"})),
    );
    let run = |args: &[&str]| -> Result<String> {
        Ok(match args {
            ["volume", "ls", ..] => "data\n".into(),
            ["volume", "inspect", "data"] => r#"[{"CreatedAt":"original"}]"#.into(),
            ["ps", ..] => "foreign-id\n".into(),
            _ => panic!("unexpected call {args:?}"),
        })
    };
    assert!(
        exclusive(&expected, &run)
            .unwrap_err()
            .to_string()
            .contains("foreign container")
    );
}

#[test]
fn missing_wrong_home_and_wrong_identity_fail_without_docker() {
    let root = tempfile::tempdir().unwrap();
    assert!(retire(&root.path().join("missing"), &"a".repeat(32), &|_| {}).is_err());
    let installation = Installation::initialize(&root.path().join("home"), None, None).unwrap();
    assert!(retire(&installation.home, &"b".repeat(32), &|_| {}).is_err());
    assert!(
        retire(&installation.home, &installation.id, &|_| {})
            .unwrap_err()
            .to_string()
            .contains("receipt missing")
    );
    let copied = root.path().join("copied");
    fs::create_dir(&copied).unwrap();
    fs::copy(
        installation.home.join("installation.json"),
        copied.join("installation.json"),
    )
    .unwrap();
    assert!(retire(&copied, &installation.id, &|_| {}).is_err());
}

#[test]
fn changed_kubeconfig_blocks_retirement_before_docker_or_intent_write() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("home"), None, None).unwrap();
    let mut expected = receipt();
    expected.installation_id.clone_from(&installation.id);
    expected.containers.clear();
    process::save(
        &installation.home.join(RECEIPT),
        &serde_json::to_vec(&expected).unwrap(),
    )
    .unwrap();
    process::save(
        &installation.home.join("runtime-owner.json"),
        &serde_json::to_vec(&serde_json::json!({
        "installation_id":installation.id,"kubeconfig_sha256":"original"}))
        .unwrap(),
    )
    .unwrap();
    fs::write(installation.kubeconfig(), "foreign config").unwrap();
    assert!(
        retire(&installation.home, &installation.id, &|_| {})
            .unwrap_err()
            .to_string()
            .contains("kubeconfig changed")
    );
    assert!(!installation.home.join(RETIRED).exists());
}

#[test]
fn names_include_auxiliary_resources_without_default_cluster() {
    let installation = Installation {
        format_version: 2,
        id: "a".repeat(32),
        home: PathBuf::from("/unused"),
        api_port: 1234,
        registry_port: 1235,
    };
    assert_eq!(container_names(&installation).len(), 5);
    assert!(
        container_names(&installation)
            .iter()
            .all(|name| name.starts_with("k3d-proofstorm-aaaaaaaa-"))
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keep the fake Docker transcript and partial-failure retry assertions together"
)]
fn deletion_uses_recorded_ids_and_retries_after_partial_failure() {
    use std::cell::RefCell;
    let installation = Installation {
        format_version: 2,
        id: "a".repeat(32),
        home: PathBuf::from("/unused"),
        api_port: 1234,
        registry_port: 1235,
    };
    let mut expected = receipt();
    expected.containers.insert(
        format!("{}-agent-0", installation.context()),
        "second-container".into(),
    );
    expected.volumes.insert(
        "data".into(),
        volume_identity(
            &serde_json::json!({"Name":"data","CreatedAt":"original","Driver":"local"}),
        ),
    );
    let state = RefCell::new(expected.clone());
    let fail_once = RefCell::new(true);
    let removals = RefCell::new(Vec::new());
    let run = |args: &[&str]| -> Result<String> {
        let mut state = state.borrow_mut();
        let json = |value: Value| Ok(value.to_string());
        match args {
            ["info", ..] => Ok(state.daemon_id.clone()),
            ["ps", ..] if args.contains(&"--filter") => Ok(state
                .containers
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")),
            ["ps", ..] => Ok(state
                .containers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")),
            ["network", "ls", ..] => Ok(state.network_id.as_ref().map_or_else(String::new, |id| {
                if args.contains(&"--no-trunc") {
                    id.clone()
                } else {
                    installation.network_name()
                }
            })),
            ["network", "inspect", _] => json(
                serde_json::json!([{"Id":state.network_id,"Labels":{"app":"k3d"},"Containers":state.containers.values().map(|id| (id.clone(), serde_json::json!({}))).collect::<BTreeMap<_, _>>()}]),
            ),
            ["inspect", ..] => {
                let name = *args.last().unwrap();
                json(
                    serde_json::json!({"id":state.containers[name],"owner":installation.id,
                    "cluster":installation.cluster_name(),"mounts":state.volumes.keys().map(|name| serde_json::json!({"Type":"volume","Name":name})).collect::<Vec<_>>()}),
                )
            }
            ["volume", "ls", ..] => {
                Ok(state.volumes.keys().cloned().collect::<Vec<_>>().join("\n"))
            }
            ["volume", "inspect", name] => json(serde_json::json!([state.volumes[*name]])),
            ["rm", "--force", id] => {
                // Fail once after the lexically first (agent) node was deleted.
                if *id == "original-container" && *fail_once.borrow() {
                    *fail_once.borrow_mut() = false;
                    anyhow::bail!("simulated Docker failure");
                }
                let name = state
                    .containers
                    .iter()
                    .find(|(_, value)| value.as_str() == *id)
                    .unwrap()
                    .0
                    .clone();
                state.containers.remove(&name);
                removals.borrow_mut().push((*id).to_string());
                Ok(String::new())
            }
            ["network", "rm", id] => {
                assert_eq!(Some(*id), state.network_id.as_deref());
                assert!(state.containers.is_empty());
                state.network_id = None;
                removals.borrow_mut().push((*id).to_string());
                Ok(String::new())
            }
            ["volume", "rm", name] => {
                assert!(state.containers.is_empty());
                state.volumes.remove(*name);
                removals.borrow_mut().push((*name).to_string());
                Ok(String::new())
            }
            _ => panic!("unexpected Docker command {args:?}"),
        }
    };
    assert!(remove(&expected, &installation, &run, &|_| {}).is_err());
    assert_eq!(removals.borrow().as_slice(), ["second-container"]);
    remove(&expected, &installation, &run, &|_| {}).unwrap();
    assert_eq!(
        removals.borrow().as_slice(),
        [
            "second-container",
            "original-container",
            "original-network",
            "data"
        ]
    );
    remove(&expected, &installation, &run, &|_| {}).unwrap();
    assert_eq!(
        removals.borrow().len(),
        4,
        "already absent resources must not be deleted again"
    );
}
