use std::{fs, path::PathBuf};

use kube::CustomResourceExt;
use proofstorm_kube::{ProofstormCandidateBuild, ProofstormLab, ProofstormLabAction};

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
