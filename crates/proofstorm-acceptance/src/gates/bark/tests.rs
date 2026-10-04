use super::*;
use proofstorm_core::{
    CatalogPlatform, CellSpec, catalog_for_platform, resolve_lock, validate_cell,
};

#[test]
fn managed_fixture_is_valid_but_cannot_resolve_unpublished_images() {
    let spec: CellSpec = serde_json::from_value(document().unwrap()).unwrap();
    let validation = validate_cell(&spec);
    assert!(validation.valid, "{validation:?}");
    assert_eq!(spec.components.len(), 8);
    for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
        assert!(resolve_lock(&spec, &catalog_for_platform(platform)).is_err());
    }
}

#[test]
fn passive_wallet_must_have_no_pending_or_reserved_value() {
    let state =
        json!({"balance_sat":69_993,"reserved_sat":0,"pending_sat":0,"pending_spent_sat":0});
    check_balance(&state, 69_993).unwrap();
    for field in [
        "balance_sat",
        "reserved_sat",
        "pending_sat",
        "pending_spent_sat",
    ] {
        let mut bad = state.clone();
        bad[field] = json!(1);
        assert!(check_balance(&bad, 69_993).is_err());
        bad.as_object_mut().unwrap().remove(field);
        assert!(check_balance(&bad, 69_993).is_err());
    }
}
