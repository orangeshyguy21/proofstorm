use std::{fs, path::PathBuf};

use kube::CustomResourceExt;
use proofstorm_kube::{ProofstormCandidateBuild, ProofstormCell, ProofstormCellAction};

#[test]
fn retired_workflows_are_not_executable_but_history_is_readable() {
    let crd = serde_json::to_value(ProofstormCellAction::crd()).unwrap();
    let action = crd
        .pointer(
            "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/action/properties",
        )
        .unwrap();
    let kinds = action["kind"]["enum"].as_array().unwrap();
    for kind in [
        "wallet_balance",
        "wallet_initialize",
        "wallet_fund",
        "wallet_round_trip",
        "wallet_quote_claim",
        "wallet_melt_quote_refresh",
        "wallet_invoice",
        "wallet_pay",
        "conservation_oracle",
        "bootstrap_liquidity",
        "peer_connect",
        "peer_disconnect",
        "channel_open",
        "channel_policy_set",
        "channel_close",
        "channel_force_close",
        "channel_rebalance",
    ] {
        assert!(
            !kinds.contains(&serde_json::json!(kind)),
            "CRD admits {kind}"
        );
        let error = serde_json::from_value::<proofstorm_kube::CellAction>(
            serde_json::json!({"kind": kind, "parameters": {}}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown variant"), "{error}");
        let historic: proofstorm_core::OperationKind =
            serde_json::from_value(serde_json::json!(kind)).unwrap();
        assert_eq!(serde_json::to_value(historic).unwrap(), kind);
    }
    for field in [
        "payerLightning",
        "meltQuoteId",
        "wallet",
        "recipientWallet",
        "recipientMint",
        "mintQuoteId",
        "amountSat",
        "expectedSat",
        "toleranceSat",
        "baselineOperationId",
        "treatmentOperationId",
        "channelId",
        "fromLightning",
        "toLightning",
        "channelSat",
        "pushSat",
        "outgoingChannelId",
        "incomingChannelId",
        "baseFeeMsat",
        "feeRatePpm",
    ] {
        assert!(
            action["parameters"]["properties"].get(field).is_none(),
            "CRD retains {field}"
        );
    }
    assert!(kinds.contains(&serde_json::json!("component_exec_live")));
}

#[test]
fn native_action_fields_survive_the_structural_schema() {
    // CRD regeneration alone cannot catch a field omitted from the hand-written
    // structural union. Check the actual serialized request against that union.
    let action =
        proofstorm_kube::CellAction::ComponentExecLive(proofstorm_kube::ComponentExecLiveAction {
            private_payload: Some(proofstorm_core::private_io::PayloadBinding::Consume {
                reference: "payload-ref".into(),
                input: proofstorm_core::private_io::InputBinding::Argv { index: 2 },
            }),
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
    let crd = serde_json::to_value(ProofstormCellAction::crd()).unwrap();
    let properties = crd.pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/action/properties/parameters/properties").unwrap();
    for field in request["parameters"].as_object().unwrap().keys() {
        assert!(
            properties.get(field).is_some(),
            "CRD drops native field {field}"
        );
    }
    for field in ["kind", "reference", "input"] {
        assert!(
            properties["privatePayload"]["properties"]
                .get(field)
                .is_some()
        );
    }
    assert!(
        properties["privatePayload"]["properties"]["input"]["properties"]
            .get("index")
            .is_some()
    );
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
            "proofstorm.dev_proofstormcells.yaml",
            serde_yaml::to_string(&ProofstormCell::crd()).expect("serialize cell CRD"),
        ),
        (
            "proofstorm.dev_proofstormcellactions.yaml",
            serde_yaml::to_string(&ProofstormCellAction::crd()).expect("serialize action CRD"),
        ),
        (
            "proofstorm.dev_proofstormcandidatebuilds.yaml",
            serde_yaml::to_string(&ProofstormCandidateBuild::crd())
                .expect("serialize candidate build CRD"),
        ),
    ];
    for (name, generated) in cases {
        if name == "proofstorm.dev_proofstormcells.yaml" {
            assert!(generated.contains("x-kubernetes-validations:"));
            assert!(generated.contains("chain bindings require only network"));
            assert!(generated.contains("backend links require a binding"));
        }
        let checked_in = fs::read_to_string(root.join("charts/proofstorm/crds").join(name))
            .unwrap_or_else(|error| panic!("read checked-in {name}: {error}"));
        assert_eq!(generated, checked_in, "regenerate CRD {name}");
    }
}

#[test]
fn recipient_scope_and_handoff_survive_structural_schema() {
    let crd = serde_json::to_value(ProofstormCellAction::crd()).unwrap();
    let spec = crd
        .pointer("/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties")
        .unwrap();
    let scope = &spec["accessScope"]["properties"]["scope"];
    for field in [
        "issuer_principal_id",
        "component",
        "mint",
        "reference",
        "receive_command_digest",
    ] {
        assert!(scope["properties"].get(field).is_some());
        assert!(
            scope["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(field))
        );
    }
    let parameters = &spec["action"]["properties"]["parameters"]["properties"];
    assert!(parameters.get("recipientGrantId").is_some());
    assert!(
        parameters["transferMethod"]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("handoff"))
    );
}
