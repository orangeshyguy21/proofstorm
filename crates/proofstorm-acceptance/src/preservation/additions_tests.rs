use super::*;

fn resource(name: &str) -> Value {
    json!({"name":name,"owner":null,"cluster":"","created":"original","driver":"local"})
}

fn inventory() -> Value {
    json!({"format_version":2,
        "containers":{"existing":{"name":"/existing","owner":null,"cluster":"","running":true,"started":"original","restarts":1,"mounts":[],"networks":{}}},
        "networks":{"network-id":resource("existing-network")},
        "volumes":{"existing-volume":resource("existing-volume")},
        "configuration_sha256":{"config":"original"}})
}

#[test]
fn unrelated_additions_are_counted_without_changing_either_inventory() {
    let before = inventory();
    let mut after = before.clone();
    for kind in ["containers", "networks", "volumes"] {
        after[kind]["new"] = resource("other-compose-service");
    }
    let retained = after.clone();
    assert_eq!(
        verify_run(&before, &after, &json!({})).unwrap(),
        json!({"containers":1,"networks":1,"volumes":1})
    );
    assert_eq!(after, retained);
    assert!(verify_with_exclusions(&before, &after, &json!({})).is_err());
    // Additions while collecting the baseline are also allowed and retained.
    assert_eq!(exclusions(&before, &after, None).unwrap(), json!({}));
}

#[test]
fn unrelated_additions_never_hide_existing_changes_removal_or_replacement() {
    let before = inventory();
    for pointer in [
        "/containers/existing/running",
        "/containers/existing/mounts",
        "/containers/existing/networks",
        "/containers/existing/owner",
        "/networks/network-id/owner",
        "/networks/network-id/created",
        "/volumes/existing-volume/created",
        "/configuration_sha256/config",
    ] {
        let mut after = before.clone();
        after["containers"]["new"] = resource("unrelated");
        *after.pointer_mut(pointer).unwrap() = json!("changed");
        assert!(
            verify_run(&before, &after, &json!({})).is_err(),
            "{pointer}"
        );
    }
    for (kind, id) in [
        ("containers", "existing"),
        ("networks", "network-id"),
        ("volumes", "existing-volume"),
    ] {
        let mut after = before.clone();
        let original = after[kind].as_object_mut().unwrap().remove(id).unwrap();
        assert!(verify_run(&before, &after, &json!({})).is_err());
        after[kind]["replacement"] = original;
        assert!(verify_run(&before, &after, &json!({})).is_err());
    }
}

#[test]
fn installation_labels_and_current_or_legacy_helpers_block_exemption() {
    let before = inventory();
    for kind in ["containers", "networks", "volumes"] {
        for (key, value) in [
            ("owner", "any-installation"),
            ("cluster", "proofstorm-12345678"),
            ("cluster", "pst-legacy"),
            ("name", "/k3d-proofstorm-12345678-tools"),
            ("name", "k3d-pst-legacy-images"),
        ] {
            let mut after = before.clone();
            after[kind]["private-new-id"] = resource("unrelated-name");
            after[kind]["private-new-id"][key] = json!(value);
            let error = verify_run(&before, &after, &json!({}))
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private-new-id"));
        }
    }
}

#[test]
fn missing_or_invalid_ownership_cannot_mean_unrelated() {
    let before = inventory();
    for field in ["name", "owner", "cluster"] {
        for value in [None, Some(json!(42))] {
            let mut after = before.clone();
            let mut new = resource("other");
            if let Some(value) = value {
                new[field] = value;
            } else {
                new.as_object_mut().unwrap().remove(field);
            }
            after["containers"]["new"] = new;
            assert!(verify_run(&before, &after, &json!({})).is_err());
        }
    }
}

#[test]
fn lifecycle_exclusions_still_require_prior_observation() {
    let first = inventory();
    let mut before = first.clone();
    before["containers"]["existing"]["restarts"] = json!(2);
    let excluded = exclusions(&first, &before, None).unwrap();
    let mut after = before.clone();
    after["containers"]["existing"]["restarts"] = json!(3);
    after["volumes"]["new"] = resource("other-volume");
    assert!(verify_run(&before, &after, &json!({})).is_err());
    verify_run(&before, &after, &excluded).unwrap();
    after["containers"]["existing"]["running"] = json!(false);
    assert!(verify_run(&before, &after, &excluded).is_err());
}

#[test]
fn older_inventories_remain_strict_and_cannot_claim_the_new_policy() {
    let old = json!({"containers":{},"networks":[],"volumes":[],"configuration_sha256":{}});
    verify_with_exclusions(&old, &old, &json!({})).unwrap();
    assert!(verify_run(&old, &old, &json!({})).is_err());
    assert!(verify_run(&inventory(), &old, &json!({})).is_err());
    let mut changed = old.clone();
    changed["containers"]["new"] = resource("unrelated");
    assert!(exclusions(&old, &changed, None).is_err());
}

#[test]
#[ignore = "requires local Docker; read-only inventory, no resources created or removed"]
fn live_ownership_inventory_matches_the_docker_inspect_schema() {
    let observed = snapshot(None).unwrap();
    assert_eq!(
        verify_run(&observed, &observed, &json!({})).unwrap(),
        json!({"containers":0,"networks":0,"volumes":0})
    );
    for kind in ["containers", "networks", "volumes"] {
        for resource in observed[kind].as_object().unwrap().values() {
            unowned(resource).unwrap();
        }
    }
}
