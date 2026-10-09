//! Managed Bark topology and native AMD64/ARM64 admission.
use proofstorm_core::{
    BitcoinNetwork, CatalogPlatform, CellSpec, ComponentKind, DatabaseRole, DependencyBinding,
    LinkKind, PaymentMethod, catalog_for_platform, resolve_lock, validate_cell,
};
use serde_json::json;
use std::collections::BTreeSet;

fn fixture() -> CellSpec {
    serde_json::from_str(include_str!("fixtures/bark-topology.json")).unwrap()
}

fn refuses(cell: &CellSpec, code: &str) {
    let report = validate_cell(cell);
    assert!(
        !report.valid && report.issues.iter().any(|issue| issue.code == code),
        "{code}: {report:?}"
    );
}

#[test]
fn complete_graph_resolves_on_both_qualified_platforms() {
    let cell = fixture();
    let report = validate_cell(&cell);
    assert!(report.valid, "{report:?}");
    for (platform, name) in [
        (CatalogPlatform::LinuxArm64, "linux/arm64"),
        (CatalogPlatform::LinuxAmd64, "linux/amd64"),
    ] {
        let catalog = catalog_for_platform(platform);
        assert!(resolve_lock(&cell, &catalog).is_ok());
        for id in ["bark-server", "cln-hold", "cdk-bark-processor"] {
            let entry = catalog.entries.iter().find(|entry| entry.id == id).unwrap();
            assert_eq!(
                entry.support_lifecycle,
                proofstorm_core::SupportLifecycle::Experimental
            );
            assert_eq!(entry.build_provenance.as_ref().unwrap().platform, name);
            assert!(entry.image.contains("@sha256:"));
            let methods: BTreeSet<_> = if id == "cdk-bark-processor" {
                [
                    PaymentMethod::Bolt11,
                    PaymentMethod::Onchain,
                    PaymentMethod::Custom("arkoor".into()),
                ]
                .into()
            } else {
                [PaymentMethod::Bolt11].into()
            };
            assert_eq!(entry.support_matrix.payment_methods, methods);
        }
    }
    let encoded = serde_json::to_string(&cell).unwrap();
    assert_eq!(serde_json::from_str::<CellSpec>(&encoded).unwrap(), cell);
}

#[test]
fn every_bark_dependency_is_required_and_unambiguous() {
    let cell = fixture();
    for link in cell.links.iter().filter(|link| link.from != "mint") {
        let mut missing = cell.clone();
        missing.links.retain(|candidate| candidate.id != link.id);
        refuses(&missing, "bark_dependency_required");
        let mut duplicate = cell.clone();
        let mut extra = link.clone();
        extra.id = format!("{}-duplicate", link.id);
        duplicate.links.push(extra);
        refuses(&duplicate, "bark_dependency_required");
        let mut missing_binding = cell.clone();
        missing_binding
            .links
            .iter_mut()
            .find(|candidate| candidate.id == link.id)
            .unwrap()
            .binding = None;
        refuses(&missing_binding, "missing_dependency_binding");
        refuses(&missing_binding, "bark_dependency_required");
    }
}

#[test]
fn role_unit_method_and_target_implementation_are_exact() {
    let cell = fixture();
    for id in ["ark", "cln", "processor"] {
        let mut invalid = cell.clone();
        invalid
            .components
            .iter_mut()
            .find(|component| component.id == id)
            .unwrap()
            .kind = ComponentKind::Wallet;
        refuses(&invalid, "bark_component_kind_mismatch");
    }
    for id in ["chain", "postgres", "cln", "ark"] {
        let mut invalid = cell.clone();
        invalid
            .components
            .iter_mut()
            .find(|component| component.id == id)
            .unwrap()
            .implementation = "other-implementation".into();
        refuses(&invalid, "bark_dependency_required");
    }
    for binding in [
        DependencyBinding::Payment {
            method: PaymentMethod::Bolt12,
            unit: "sat".into(),
        },
        DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit: "msat".into(),
        },
        DependencyBinding::Payment {
            method: PaymentMethod::Onchain,
            unit: "sat".into(),
        },
    ] {
        let mut invalid = cell.clone();
        invalid
            .links
            .iter_mut()
            .find(|link| link.id == "ark-lightning")
            .unwrap()
            .binding = Some(binding);
        refuses(&invalid, "bark_dependency_required");
    }
    for role in [DatabaseRole::Cache, DatabaseRole::Authentication] {
        let mut invalid = cell.clone();
        invalid
            .links
            .iter_mut()
            .find(|link| link.id == "ark-database")
            .unwrap()
            .binding = Some(DependencyBinding::Database {
            role,
            database: Some("bark".into()),
        });
        refuses(&invalid, "bark_dependency_required");
    }
}

#[test]
fn bitcoin_dependencies_must_share_one_component_not_just_a_network_name() {
    let mut cell = fixture();
    let mut other_chain = cell.components[0].clone();
    other_chain.id = "other-chain".into();
    cell.components.push(other_chain);
    for id in ["processor-chain", "ark-chain", "cln-chain"] {
        let mut invalid = cell.clone();
        invalid
            .links
            .iter_mut()
            .find(|link| link.id == id)
            .unwrap()
            .to = "other-chain".into();
        refuses(&invalid, "bark_shared_chain_required");
        invalid.links.reverse();
        refuses(&invalid, "bark_shared_chain_required");
    }
    // A consistent graph can point at either regtest component.
    for link in &mut cell.links {
        if link.kind == LinkKind::ChainBackend {
            link.to = "other-chain".into();
        }
    }
    assert!(validate_cell(&cell).valid);
}

#[test]
fn indexed_local_bitcoin_is_required() {
    for value in [json!(false), json!("true"), json!(null)] {
        let mut invalid = fixture();
        invalid.components[0].config.insert("txindex".into(), value);
        refuses(&invalid, "bark_txindex_required");
    }
    let mut explicit = fixture();
    explicit.components[0]
        .config
        .insert("txindex".into(), json!(true));
    assert!(validate_cell(&explicit).valid);
}

#[test]
fn ark_binding_is_distinct_from_chain_and_payment_dependencies() {
    let cell = fixture();
    for binding in [
        DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        },
        DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit: "sat".into(),
        },
    ] {
        let mut invalid = cell.clone();
        invalid
            .links
            .iter_mut()
            .find(|link| link.id == "processor-ark")
            .unwrap()
            .binding = Some(binding);
        refuses(&invalid, "incompatible_dependency_binding");
        refuses(&invalid, "bark_dependency_required");
    }
    for (from, to) in [
        ("mint", "ark"),
        ("processor", "chain"),
        ("ark", "processor"),
    ] {
        let mut invalid = cell.clone();
        let link = invalid
            .links
            .iter_mut()
            .find(|link| link.id == "processor-ark")
            .unwrap();
        link.from = from.into();
        link.to = to.into();
        refuses(&invalid, "incompatible_link_kinds");
    }
    let mut unused = cell.clone();
    let mut extra = unused
        .links
        .iter()
        .find(|link| link.id == "ark-lightning")
        .unwrap()
        .clone();
    extra.id = "processor-lightning".into();
    extra.from = "processor".into();
    unused.links.push(extra);
    refuses(&unused, "bark_unexpected_dependency");
}

#[test]
fn ark_binding_refuses_undeclared_networks_and_irrelevant_fields() {
    for binding in [
        json!({"type":"ark"}),
        json!({"type":"ark","network":"bitcoin"}),
        json!({"type":"ark","network":"regtest","unit":"sat"}),
        json!({"type":"ark","network":"regtest","endpoint":"http://public.example"}),
    ] {
        assert!(serde_json::from_value::<DependencyBinding>(binding).is_err());
    }
}

fn select(cell: &mut CellSpec, methods: &serde_json::Value, links: &[&str]) {
    cell.components
        .iter_mut()
        .find(|component| component.id == "processor")
        .unwrap()
        .config
        .insert("payment_methods".into(), methods.clone());
    cell.links.retain(|link| {
        link.from != "mint"
            || link.kind != LinkKind::PaymentBackend
            || links.contains(&link.id.as_str())
    });
}

#[test]
fn mint_binds_exactly_the_methods_the_processor_advertises() {
    let catalog = catalog_for_platform(CatalogPlatform::LinuxArm64);
    // The default advertises every method; dropping any binding is refused.
    for missing in ["mint-bolt11", "mint-onchain", "mint-arkoor"] {
        let mut invalid = fixture();
        invalid.links.retain(|link| link.id != missing);
        refuses(&invalid, "bark_processor_payment_bindings");
    }
    for (methods, links) in [
        (json!(["bolt11"]), &["mint-bolt11"][..]),
        (
            json!(["arkoor", "onchain"]),
            &["mint-onchain", "mint-arkoor"][..],
        ),
        (
            json!(["bolt11", "onchain", "arkoor"]),
            &["mint-bolt11", "mint-onchain", "mint-arkoor"][..],
        ),
    ] {
        let mut cell = fixture();
        select(&mut cell, &methods, links);
        let report = validate_cell(&cell);
        assert!(report.valid, "{methods}: {report:?}");
        resolve_lock(&cell, &catalog).unwrap();
        // Extra bindings to a method the processor does not advertise are refused.
        let mut extra = fixture();
        select(
            &mut extra,
            &methods,
            &["mint-bolt11", "mint-onchain", "mint-arkoor"],
        );
        if links.len() < 3 {
            refuses(&extra, "bark_processor_payment_bindings");
        }
    }
    let mut duplicate = fixture();
    duplicate.links.retain(|link| link.id != "mint-onchain");
    duplicate
        .links
        .iter_mut()
        .find(|link| link.id == "mint-arkoor")
        .unwrap()
        .binding = Some(DependencyBinding::Payment {
        method: PaymentMethod::Bolt11,
        unit: "sat".into(),
    });
    refuses(&duplicate, "bark_processor_payment_bindings");
}

#[test]
fn embedded_bdk_can_serve_onchain_beside_a_bark_subset() {
    let mut cell = fixture();
    select(
        &mut cell,
        &json!(["bolt11", "arkoor"]),
        &["mint-bolt11", "mint-arkoor"],
    );
    let mint = cell
        .components
        .iter_mut()
        .find(|component| component.id == "mint")
        .unwrap();
    mint.config.insert("embedded_onchain".into(), json!("bdk"));
    cell.links.push(
        serde_json::from_value(json!({"id":"mint-chain","kind":"chain_backend","from":"mint","to":"chain","binding":{"type":"chain","network":"regtest"}}))
            .unwrap(),
    );
    let report = validate_cell(&cell);
    assert!(report.valid, "{report:?}");
    resolve_lock(&cell, &catalog_for_platform(CatalogPlatform::LinuxArm64)).unwrap();
    // The processor's default also advertises onchain, which BDK already claims.
    let mut conflict = cell.clone();
    select(
        &mut conflict,
        &json!(["bolt11", "onchain", "arkoor"]),
        &["mint-bolt11", "mint-arkoor"],
    );
    conflict.links.push(
        fixture()
            .links
            .into_iter()
            .find(|link| link.id == "mint-onchain")
            .unwrap(),
    );
    refuses(&conflict, "cdk_payment_method_conflict");
}

#[test]
fn arkoor_bindings_only_target_a_payment_processor() {
    let mut invalid = fixture();
    invalid
        .links
        .iter_mut()
        .find(|link| link.id == "ark-lightning")
        .unwrap()
        .binding = Some(DependencyBinding::Payment {
        method: PaymentMethod::Custom("arkoor".into()),
        unit: "sat".into(),
    });
    refuses(&invalid, "incompatible_link_kinds");
}
