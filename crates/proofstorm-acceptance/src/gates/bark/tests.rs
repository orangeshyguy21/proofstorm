use super::*;
use proofstorm_core::{
    CatalogPlatform, CellSpec, catalog_for_platform, resolve_lock, validate_cell,
};

#[test]
fn managed_fixture_resolves_on_both_native_platforms() {
    let spec: CellSpec = serde_json::from_value(document().unwrap()).unwrap();
    let validation = validate_cell(&spec);
    assert!(validation.valid, "{validation:?}");
    assert_eq!(spec.components.len(), 8);
    assert_eq!(
        spec.components
            .iter()
            .find(|component| component.id == "mint")
            .unwrap()
            .config["input_fee_ppk"],
        json!(0)
    );
    assert!(resolve_lock(&spec, &catalog_for_platform(CatalogPlatform::LinuxArm64)).is_ok());
    assert!(resolve_lock(&spec, &catalog_for_platform(CatalogPlatform::LinuxAmd64)).is_ok());
}

#[test]
fn prepared_images_match_the_fixture_lock_and_refuse_missing_catalog_entries() {
    let spec: CellSpec = serde_json::from_value(document().unwrap()).unwrap();
    for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
        let mut catalog = catalog_for_platform(platform);
        let expected: Vec<_> = resolve_lock(&spec, &catalog)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.image)
            .collect();
        assert_eq!(images(&catalog).unwrap(), expected);
        assert_eq!(expected.len(), 8);
        catalog.entries.retain(|entry| entry.id != "cln-hold");
        assert!(images(&catalog).is_err());
    }
}

#[test]
fn planned_bark_cases_instantiate_every_exact_component_and_image() {
    use proofstorm_qualification::{Identity, Mode, Scenario};
    let plan = proofstorm_qualification::plan(
        Identity {
            revision: "a".repeat(40),
            run_id: "1".into(),
            attempt: 1,
        },
        Mode::Compatibility,
    )
    .unwrap();
    let cases: Vec<_> = plan.cases.into_iter().filter(|case| {
        matches!(&case.scenario, Scenario::Gate { name, .. } if name == "bark-processor")
    }).collect();
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert!(case.required);
        let observer = crate::qualification::Observer::new(case.clone());
        let mut document = document().unwrap();
        observer.document(&mut document).unwrap();
        observer.finish().unwrap();
        let spec: CellSpec = serde_json::from_value(document).unwrap();
        let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
        let lock = resolve_lock(&spec, &catalog).unwrap();
        assert_eq!(lock.entries.len(), case.components.len());
        for component in case.components {
            assert!(
                lock.entries
                    .iter()
                    .any(|entry| entry.catalog_id == component.implementation
                        && entry.version == component.version
                        && entry.image == component.image)
            );
        }
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
