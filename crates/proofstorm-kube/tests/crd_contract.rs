use std::{fs, path::PathBuf};

use kube::CustomResourceExt;
use proofstorm_kube::{ProofstormCandidateBuild, ProofstormLab, ProofstormLabAction};

#[test]
fn native_action_fields_survive_the_structural_schema() {
    // CRD regeneration alone cannot catch a field omitted from the hand-written
    // structural union. Check the actual serialized request against that union.
    let action =
        proofstorm_kube::LabAction::ComponentExecLive(proofstorm_kube::ComponentExecLiveAction {
            component: "wallet".into(),
            script: String::new(),
            argv: vec!["cdk-cli".into(), "--version".into()],
            timeout_seconds: 10,
            output: proofstorm_core::native::NativeOutput {
                mode: proofstorm_core::native::OutputMode::JsonFields,
                fields: vec!["status".into()],
            },
        });
    let request = serde_json::to_value(action).unwrap();
    let crd = serde_json::to_value(ProofstormLabAction::crd()).unwrap();
    let properties = crd.pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/action/properties/parameters/properties").unwrap();
    for field in request["parameters"].as_object().unwrap().keys() {
        assert!(
            properties.get(field).is_some(),
            "CRD drops native field {field}"
        );
    }
    for field in request["parameters"]["output"].as_object().unwrap().keys() {
        assert!(
            properties["output"]["properties"].get(field).is_some(),
            "CRD drops output field {field}"
        );
    }
}

#[test]
fn checked_in_crds_match_typed_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = [
        (
            "proofstorm.dev_proofstormlabs.yaml",
            serde_yaml::to_string(&ProofstormLab::crd()).expect("serialize lab CRD"),
        ),
        (
            "proofstorm.dev_proofstormlabactions.yaml",
            serde_yaml::to_string(&ProofstormLabAction::crd()).expect("serialize action CRD"),
        ),
        (
            "proofstorm.dev_proofstormcandidatebuilds.yaml",
            serde_yaml::to_string(&ProofstormCandidateBuild::crd())
                .expect("serialize candidate build CRD"),
        ),
    ];
    for (name, generated) in cases {
        if name == "proofstorm.dev_proofstormlabs.yaml" {
            assert!(generated.contains("x-kubernetes-validations:"));
            assert!(generated.contains("chain bindings require only network"));
            assert!(generated.contains("backend links require a binding"));
        }
        let checked_in = fs::read_to_string(root.join("charts/proofstorm/crds").join(name))
            .unwrap_or_else(|error| panic!("read checked-in {name}: {error}"));
        assert_eq!(generated, checked_in, "regenerate CRD {name}");
    }
}
