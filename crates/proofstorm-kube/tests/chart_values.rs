use std::{fs, path::PathBuf};

/// CPU cores in a Kubernetes quantity such as `250m` or `"2"`.
fn cores(quantity: &serde_yaml::Value) -> f64 {
    match quantity {
        serde_yaml::Value::Number(number) => number.as_f64().expect("CPU number"),
        serde_yaml::Value::String(text) => text.strip_suffix('m').map_or_else(
            || text.parse().expect("CPU cores"),
            |milli| milli.parse::<f64>().expect("CPU millicores") / 1000.0,
        ),
        other => panic!("CPU quantity {other:?}"),
    }
}

#[test]
fn controller_cpu_is_never_capped_by_default() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let values: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(root.join("charts/proofstorm/values.yaml")).expect("read values"),
    )
    .expect("parse values");
    let resources = &values["resources"];
    // At 24 agents a 250m controller was throttled in every period and native
    // starts missed their deadline. Setup installs these values unchanged.
    assert!(resources["limits"].get("cpu").is_none());
    assert!(cores(&resources["requests"]["cpu"]) >= 0.25);
    assert!(resources["limits"].get("memory").is_some());
}

#[test]
fn chart_defaults_match_the_runtime_settings_defaults() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let values: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(root.join("charts/proofstorm/values.yaml")).expect("read values"),
    )
    .expect("parse values");
    let resources = &values["resources"];
    assert_eq!(
        resources["requests"]["cpu"].as_str(),
        Some(proofstorm_core::CONTROLLER_CPU_REQUEST)
    );
    assert_eq!(
        resources["requests"]["memory"].as_str(),
        Some(proofstorm_core::CONTROLLER_MEMORY_REQUEST)
    );
    assert_eq!(
        resources["limits"]["memory"].as_str(),
        Some(proofstorm_core::CONTROLLER_MEMORY_LIMIT)
    );
    let limits = proofstorm_core::ControllerLimits::default();
    let controller = &values["controller"];
    for (key, expected) in [
        ("actionSlots", u64::from(limits.action_slots)),
        ("nativeStarts", u64::from(limits.native_starts)),
        (
            "nativeStartsPerComponent",
            u64::from(limits.native_starts_per_component),
        ),
        ("nativePolls", u64::from(limits.native_polls)),
        (
            "startDeadlineSeconds",
            u64::from(limits.start_deadline_seconds),
        ),
    ] {
        assert_eq!(controller[key].as_u64(), Some(expected), "{key}");
    }
    let template =
        fs::read_to_string(root.join("charts/proofstorm/templates/deployment.yaml")).unwrap();
    for (name, _) in limits.env() {
        assert!(template.contains(name), "{name}");
    }
}
