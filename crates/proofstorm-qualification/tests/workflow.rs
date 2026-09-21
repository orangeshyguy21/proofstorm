use std::collections::BTreeSet;

use serde_json::{Value, json};

fn workflow(source: &str) -> Value {
    serde_saphyr::from_str(source).unwrap()
}

#[test]
fn actual_merge_dependencies_reject_failure_cancellation_skip_and_missing_results() {
    let checks = workflow(include_str!("../../../.github/workflows/check.yml"));
    let job = &checks["jobs"]["merge-qualification"];
    assert_eq!(job["name"], "Merge qualification");
    assert_eq!(job["if"], "always()");
    let needs = job["needs"].as_array().unwrap();
    let good: Value = needs
        .iter()
        .map(|name| {
            (
                name.as_str().unwrap().to_owned(),
                json!({"result":"success"}),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    proofstorm_qualification::aggregate(&good).unwrap();
    for name in needs {
        let name = name.as_str().unwrap();
        for result in ["failure", "cancelled", "skipped", ""] {
            let mut broken = good.clone();
            broken[name]["result"] = result.into();
            assert!(proofstorm_qualification::aggregate(&broken).is_err());
        }
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(proofstorm_qualification::aggregate(&missing).is_err());
    }
    assert_eq!(
        checks["jobs"]["qualification"]["uses"],
        "./.github/workflows/qualification.yml"
    );
    assert!(checks["jobs"]["qualification"]["if"].is_null());
    assert_eq!(checks["on"]["push"]["branches"], json!(["main"]));
    assert_eq!(checks["on"]["pull_request"]["branches"], json!(["main"]));
    for event in ["pull_request", "push", "merge_group"] {
        assert!(checks["on"].get(event).is_some());
        assert!(checks["on"][event]["paths"].is_null());
        assert!(checks["on"][event]["paths-ignore"].is_null());
    }
    assert_eq!(
        checks["concurrency"]["cancel-in-progress"],
        "${{ github.event_name == 'pull_request' }}"
    );
    assert!(
        checks["concurrency"]["group"]
            .as_str()
            .unwrap()
            .contains("github.run_id")
    );
}

#[test]
fn native_execution_and_evidence_cannot_be_optional_or_publish_packages() {
    let qualification = workflow(include_str!("../../../.github/workflows/qualification.yml"));
    assert_eq!(qualification["permissions"], json!({"contents":"read"}));
    let jobs = qualification["jobs"].as_object().unwrap();
    for job in jobs.values() {
        assert!(job["continue-on-error"].is_null());
        assert!(job["permissions"].is_null());
        for step in job["steps"].as_array().unwrap() {
            assert!(step["continue-on-error"].is_null());
        }
    }
    assert_eq!(jobs["execute"]["strategy"]["fail-fast"], false);
    assert_eq!(jobs["verify"]["if"], "always()");
    let needs: BTreeSet<_> = jobs["verify"]["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(needs, ["plan", "build", "execute"].into());
    let platforms: BTreeSet<_> = jobs["build"]["strategy"]["matrix"]["include"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["platform"].as_str().unwrap())
        .collect();
    assert_eq!(platforms, ["linux/amd64", "linux/arm64"].into());
}
