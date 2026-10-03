//! Benchmark-only host activity reporting. Never grants teardown authority.
use super::{differences, unowned, verify_with_exclusions};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const POLICY: &str = "benchmark-shared-host-v1";

pub fn verify(before: &Value, after: &Value, excluded: &Value) -> Result<Value> {
    ensure!(
        before["format_version"] == 2 && after["format_version"] == 2,
        "shared-host policy requires ownership-aware preservation inventories"
    );
    let mut protected_before = before.clone();
    let mut protected_after = after.clone();
    let mut activity_before = json!({});
    let mut activity_after = json!({});
    for kind in ["containers", "networks", "volumes"] {
        let old = before[kind]
            .as_object()
            .context("preservation inventory missing")?;
        let new = after[kind]
            .as_object()
            .context("preservation inventory missing")?;
        activity_before[kind] = json!({});
        activity_after[kind] = json!({});
        for id in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            // Either observation can protect an identity. Removing a label or
            // changing a reserved name must never turn it into an exemption.
            let old_unowned = old.get(id).map(unowned).transpose()?.unwrap_or(true);
            let new_unowned = new.get(id).map(unowned).transpose()?.unwrap_or(true);
            if old_unowned && new_unowned {
                for (protected, activity) in [
                    (&mut protected_before, &mut activity_before),
                    (&mut protected_after, &mut activity_after),
                ] {
                    if let Some(value) = protected[kind].as_object_mut().unwrap().remove(id) {
                        activity[kind][id] = value;
                    }
                }
            }
        }
    }
    let old = before["configuration_sha256"]
        .as_object()
        .context("configuration inventory missing")?;
    let new = after["configuration_sha256"]
        .as_object()
        .context("configuration inventory missing")?;
    activity_before["configuration_sha256"] = json!({});
    activity_after["configuration_sha256"] = json!({});
    for file in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
        // Benchmark adapters retain their own controlled configuration. The
        // live checkout's database, ownership receipts and kubeconfig stay strict.
        if personal_configuration(file) {
            for (protected, activity) in [
                (&mut protected_before, &mut activity_before),
                (&mut protected_after, &mut activity_after),
            ] {
                if let Some(value) = protected["configuration_sha256"]
                    .as_object_mut()
                    .unwrap()
                    .remove(file)
                {
                    activity["configuration_sha256"][file] = value;
                }
            }
        }
    }
    verify_with_exclusions(&protected_before, &protected_after, excluded)?;
    Ok(differences(&activity_before, &activity_after))
}

fn personal_configuration(file: &str) -> bool {
    [
        "/.codex/config.toml",
        "/.claude.json",
        "/.config/opencode/opencode.json",
        "/.kube/config",
    ]
    .iter()
    .any(|suffix| file.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &str) -> Value {
        json!({"name":name,"owner":null,"cluster":"","created":"original"})
    }

    fn inventory() -> Value {
        json!({"format_version":2,
            "containers":{"orchard":resource("/orchard"),"protected":{"name":"/k3d-proofstorm-live-server","owner":"live","cluster":"proofstorm-live"}},
            "networks":{"orchard-net":resource("orchard-net")},
            "volumes":{"orchard-data":resource("orchard-data")},
            "configuration_sha256":{"/user/.codex/config.toml":"original","/checkout/state/proofstorm.sqlite3":"original"}})
    }

    #[test]
    fn unrelated_rebuilds_and_personal_settings_are_reported_without_mutation() {
        let before = inventory();
        let mut after = before.clone();
        after["containers"]
            .as_object_mut()
            .unwrap()
            .remove("orchard");
        after["containers"]["replacement"] = resource("/orchard");
        after["networks"].as_object_mut().unwrap().clear();
        after["volumes"]["orchard-data"]["created"] = json!("recreated");
        after["configuration_sha256"]["/user/.codex/config.toml"] = json!("new-model");
        let original = after.clone();
        assert_eq!(
            verify(&before, &after, &json!({})).unwrap(),
            json!({
            "containers":{"added":1,"removed":1,"changed":0},
            "networks":{"added":0,"removed":1,"changed":0},
            "volumes":{"added":0,"removed":0,"changed":1},
            "configuration_sha256":{"added":0,"removed":0,"changed":1}})
        );
        assert_eq!(after, original);
        assert!(super::super::verify_run(&before, &after, &json!({})).is_err());
    }

    #[test]
    fn proofstorm_identity_checkout_state_and_cleanup_leaks_stay_strict() {
        let before = inventory();
        for kind in ["containers", "networks", "volumes"] {
            for (field, value) in [
                ("owner", "this-run"),
                ("cluster", "proofstorm-run"),
                ("name", "k3d-pst-legacy-images"),
            ] {
                let mut after = before.clone();
                after[kind]["leak"] = resource("new");
                after[kind]["leak"][field] = json!(value);
                assert!(verify(&before, &after, &json!({})).is_err());
            }
        }
        let mut after = before.clone();
        after["containers"]
            .as_object_mut()
            .unwrap()
            .remove("protected");
        assert!(verify(&before, &after, &json!({})).is_err());
        after["containers"]["protected"] = resource("renamed");
        assert!(verify(&before, &after, &json!({})).is_err());
        after = before.clone();
        after["configuration_sha256"]["/checkout/state/proofstorm.sqlite3"] = json!("changed");
        assert!(verify(&before, &after, &json!({})).is_err());
    }

    #[test]
    fn missing_ownership_and_unknown_inventories_cannot_gain_exemptions() {
        let before = inventory();
        for field in ["name", "owner", "cluster"] {
            let mut after = before.clone();
            after["containers"]["orchard"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(verify(&before, &after, &json!({})).is_err());
        }
        assert!(verify(&json!({}), &before, &json!({})).is_err());
        assert!(verify(&before, &before, &json!({"protected":["owner"]})).is_err());
    }
}
