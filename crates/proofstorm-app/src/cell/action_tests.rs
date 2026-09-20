use super::{
    component_reference::component_image_any,
    private_transfer::{PrivateTransferInput, validate_private_transfer_endpoints},
};
use proofstorm_core::{
    API_VERSION, CellPolicy, CellSpec, ComponentKind, ComponentSpec, PublishedRevision,
    default_catalog, digest_json,
};
use std::collections::BTreeMap;

fn component_reference_revision() -> PublishedRevision {
    let catalog = default_catalog();
    let component = |id: &str, implementation: &str| {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == implementation)
            .expect("fixture implementation");
        ComponentSpec {
            id: id.into(),
            kind: entry.kind,
            implementation: entry.id.clone(),
            version: Some(entry.version.clone()),
            config_version: entry.config_version.clone(),
            control: entry.allowed_control[0],
            config: BTreeMap::new(),
        }
    };
    let authored = CellSpec {
        api_version: API_VERSION.into(),
        name: "component-references".into(),
        components: vec![
            component("wallet-b", "nutshell-wallet"),
            component("chain", "bitcoin-core"),
            component("wallet-a", "nutshell-wallet"),
        ],
        links: vec![],
        policy: CellPolicy::default(),
    };
    let cell = proofstorm_core::resolve_effective_cell(&authored, catalog)
        .expect("effective component fixture");
    let lock = proofstorm_core::resolve_lock(&cell, catalog).expect("locked component fixture");
    PublishedRevision {
        workspace_id: "alpha".into(),
        digest: digest_json(&(&cell, &lock)),
        cell,
        lock,
    }
}

#[test]
fn component_references_return_typed_recovery_alternatives() {
    let revision = component_reference_revision();
    let expected_image = default_catalog()
        .entries
        .iter()
        .find(|entry| entry.id == "nutshell-wallet")
        .expect("wallet catalog entry")
        .image
        .clone();
    assert_eq!(
        component_image_any(&revision, "wallet-a", ComponentKind::Wallet)
            .expect("known typed component"),
        expected_image
    );

    let unknown = component_image_any(&revision, "invented-wallet", ComponentKind::Wallet)
        .expect_err("unknown component must fail closed");
    let unknown_data = unknown.details.expect("structured unknown-ID error");
    assert_eq!(unknown_data["code"], "component_id_unknown");
    assert_eq!(unknown_data["requested_id"], "invented-wallet");
    assert_eq!(unknown_data["expected_kind"], "wallet");
    assert_eq!(
        unknown_data["valid_component_ids"],
        serde_json::json!(["wallet-a", "wallet-b"])
    );

    let wrong_kind = component_image_any(&revision, "chain", ComponentKind::Wallet)
        .expect_err("wrong component kind must fail closed");
    let wrong_kind_data = wrong_kind.details.expect("structured kind error");
    assert_eq!(wrong_kind_data["code"], "component_kind_mismatch");
    assert_eq!(wrong_kind_data["actual_kind"], "bitcoin");
    assert_eq!(wrong_kind_data["expected_kind"], "wallet");
    assert_eq!(
        wrong_kind_data["valid_component_ids"],
        serde_json::json!(["wallet-a", "wallet-b"])
    );
}

#[test]
fn private_transfer_methods_preserve_the_controller_wire_contract() {
    for input in [
        serde_json::json!({"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536}),
        serde_json::json!({"transferMethod":"handoff","component":"wallet-a","reference":"opaque-ref","recipientGrantId":"child"}),
        serde_json::json!({"transferMethod":"status","component":"wallet-a","reference":"opaque-ref"}),
        serde_json::json!({"transferMethod":"deliver","component":"wallet-a","reference":"opaque-ref"}),
        serde_json::json!({"transferMethod":"release","component":"wallet-a","reference":"opaque-ref"}),
    ] {
        let old: proofstorm_kube::PrivateTransferAction =
            serde_json::from_value(input.clone()).unwrap();
        let public: PrivateTransferInput = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(public.action().unwrap(), old);
        assert_eq!(serde_json::to_value(public).unwrap(), input);
    }
}

#[test]
fn private_transfer_preflight_enforces_native_input_limits_and_endpoints() {
    let prepare = |size, destination: &str| PrivateTransferInput::Prepare {
        component: "wallet-a".into(),
        destination_component: destination.into(),
        maximum_bytes: size,
    };
    for size in [0, 1_048_577, u32::MAX] {
        assert!(
            prepare(size, "wallet-b")
                .action()
                .unwrap_err()
                .message
                .contains("maximumBytes")
        );
    }
    assert!(
        prepare(65536, "wallet-a")
            .action()
            .unwrap_err()
            .message
            .contains("must differ")
    );
    assert!(prepare(65536, " ").action().is_err());
    let recipient = |implementation: &str| {
        let mut revision = component_reference_revision();
        let entry = default_catalog()
            .entries
            .iter()
            .find(|e| e.id == implementation)
            .unwrap();
        let component = revision
            .cell
            .components
            .iter_mut()
            .find(|c| c.id == "wallet-b")
            .unwrap();
        component.implementation = entry.id.clone();
        component.version = Some(entry.version.clone());
        component.config_version = entry.config_version.clone();
        component.control = entry.allowed_control[0];
        revision.lock = proofstorm_core::resolve_lock(&revision.cell, default_catalog()).unwrap();
        revision
    };
    let revision = recipient("cdk-cli-wallet");
    for size in [1, 65536] {
        validate_private_transfer_endpoints(
            &prepare(size, "wallet-b").action().unwrap(),
            &revision,
        )
        .unwrap();
    }
    for size in [65537, 1_048_576] {
        let error = validate_private_transfer_endpoints(
            &prepare(size, "wallet-b").action().unwrap(),
            &revision,
        )
        .unwrap_err();
        assert!(
            error.message.contains("65536") && error.message.contains("no operation was created")
        );
    }
    for destination in ["missing", "chain"] {
        assert!(
            validate_private_transfer_endpoints(
                &prepare(1, destination).action().unwrap(),
                &revision
            )
            .is_err()
        );
    }
    let revision = recipient("cocod-wallet");
    validate_private_transfer_endpoints(
        &prepare(1_048_576, "wallet-b").action().unwrap(),
        &revision,
    )
    .unwrap();
    assert!(
        PrivateTransferInput::Status {
            component: "wallet-a".into(),
            reference: String::new()
        }
        .action()
        .is_err()
    );
}
