use super::*;
use proofstorm_core::{CellSpec, catalog_with_spark_test_images, resolve_lock, validate_cell};

/// Every component the Spark catalog images serve in the managed cell.
const SPARK_COMPONENTS: [&str; 6] = [
    "operators",
    "ssp-node",
    "ssp",
    "processor",
    "peer",
    "island",
];

fn spec() -> CellSpec {
    serde_json::from_value(document().unwrap()).unwrap()
}

#[test]
fn managed_fixture_validates_and_resolves_on_both_pinned_platforms() {
    let spec = spec();
    let validation = validate_cell(&spec);
    assert!(validation.valid, "{validation:?}");
    assert_eq!(spec.components.len(), 10);
    let component = |id: &str| {
        spec.components
            .iter()
            .find(|component| component.id == id)
            .unwrap()
    };
    assert_eq!(component("mint").config["input_fee_ppk"], json!(0));
    for id in ["peer", "island"] {
        assert_eq!(component(id).implementation, "ldk-server");
        assert_eq!(component(id).version.as_deref(), Some("0.1.0-762bc47"));
        assert_eq!(component(id).version, component("ssp-node").version);
    }
    // The island stays unreachable: no link other than its chain backend.
    let island = spec
        .links
        .iter()
        .filter(|link| link.from == "island" || link.to == "island")
        .collect::<Vec<_>>();
    assert_eq!(island.len(), 1);
    assert_eq!(island[0].id, "island-chain");
    for amd64 in [true, false] {
        assert!(resolve_lock(&spec, &catalog_with_spark_test_images(amd64)).is_ok());
    }
}

#[test]
fn prepared_images_are_digest_pinned_and_refuse_missing_catalog_entries() {
    let spec = spec();
    for amd64 in [true, false] {
        let mut catalog = catalog_with_spark_test_images(amd64);
        let lock = resolve_lock(&spec, &catalog).unwrap();
        let expected: Vec<_> = lock
            .entries
            .iter()
            .map(|entry| entry.image.clone())
            .collect();
        assert_eq!(images(&catalog).unwrap(), expected);
        assert_eq!(expected.len(), 10);
        for id in SPARK_COMPONENTS {
            let entry = lock
                .entries
                .iter()
                .find(|entry| entry.component_id == id)
                .unwrap();
            assert!(entry.image.contains("@sha256:"), "{id}");
            assert!(entry.build_provenance.is_some(), "{id}");
        }
        let published = json!({"lock":lock});
        STACK.check_pinned(&published, &PINNED).unwrap();
        // The peer's node is held to the same standard as the SSP's.
        for field in ["image", "build_provenance"] {
            let mut unpinned = published.clone();
            let peer = unpinned["lock"]["entries"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["component_id"] == "peer")
                .unwrap();
            peer[field] = if field == "image" {
                json!("proofstorm-registry.localhost:5000/ldk-server:latest")
            } else {
                Value::Null
            };
            assert!(STACK.check_pinned(&unpinned, &PINNED).is_err(), "{field}");
        }
        catalog.entries.retain(|entry| entry.id != OPEN_SSP);
        assert!(images(&catalog).is_err());
    }
}

#[test]
fn planned_spark_cases_instantiate_every_exact_component_and_image() {
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
    // Spark cases are planned only for platforms with pinned images, so this
    // checks nothing until spark_images.json is staged for publication.
    for case in plan.cases.into_iter().filter(
        |case| matches!(&case.scenario, Scenario::Gate { name, .. } if name == "spark-processor"),
    ) {
        assert!(case.required);
        let observer = crate::qualification::Observer::new(case.clone());
        let mut document = document().unwrap();
        observer.document(&mut document).unwrap();
        observer.finish().unwrap();
        let spec: CellSpec = serde_json::from_value(document).unwrap();
        let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
        let lock = resolve_lock(&spec, &catalog).unwrap();
        // Three components share the one planned LDK Server version.
        for entry in &lock.entries {
            assert!(case.components.iter().any(|component| {
                entry.catalog_id == component.implementation
                    && entry.version == component.version
                    && entry.image == component.image
            }));
        }
        for component in case.components {
            assert!(
                lock.entries
                    .iter()
                    .any(|entry| entry.catalog_id == component.implementation)
            );
        }
    }
}

#[test]
fn command_builders_carry_only_the_admin_header_file() {
    let admin = [
        ssp_status(),
        deposit_address(),
        claim_deposit(&"ab".repeat(100), 1),
    ];
    for command in admin.iter().chain([&ssp_identity()]) {
        for secret in [
            "Authorization",
            "Bearer",
            "admin-token",
            "SPARK_ADMIN_TOKEN",
            "/ssp-identity",
            "/processor-identity",
            "/spark-operators",
            "mnemonic",
            ".key",
        ] {
            assert!(!command.contains(secret), "{command} contains {secret}");
        }
        assert!(command.contains("http://127.0.0.1:5000/"), "{command}");
        // Every recipe step must at least parse as a shell command.
        let syntax = std::process::Command::new("sh")
            .args(["-n", "-c", command])
            .status()
            .unwrap();
        assert!(syntax.success(), "{command}");
    }
    for command in &admin {
        assert_eq!(
            command.matches("-H @/run/open-ssp/admin.header").count(),
            1,
            "{command}"
        );
    }
    assert!(!ssp_identity().contains("admin.header"));
    assert!(
        deposit_address().contains("-X POST http://127.0.0.1:5000/admin/spark/deposit-address")
    );
    let claim = claim_deposit("00ff", 3);
    let body = claim
        .split("-d '")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap(),
        json!({"transaction_hex":"00ff","vout":3})
    );
}

#[test]
fn retried_recipe_steps_print_their_json_or_a_pending_marker() {
    let run = |command: &str| {
        let output = std::process::Command::new("sh")
            .args(["-c", command])
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    assert_eq!(
        run(&retryable(r#"printf '{"ok":true}'"#, r#"{"ok":false}"#)),
        json!({"ok":true})
    );
    // A failed step's body is discarded: only the marker is printed.
    assert_eq!(
        run(&retryable(
            r#"printf '{"error":"dkg"}'; false"#,
            r#"{"address":null}"#
        )),
        json!({"address":null})
    );
}

#[test]
fn ssp_liquidity_counts_only_available_sats_of_a_healthy_wallet() {
    assert_eq!(
        available_sats(&json!({"spark":{"available_sats":500_000},"spark_error":null})).unwrap(),
        500_000
    );
    assert_eq!(
        available_sats(&json!({"spark":null,"spark_error":"not ready"})).unwrap(),
        0
    );
    assert!(available_sats(&json!({"spark":{}})).is_err());
}

#[test]
fn deposit_claim_uses_the_single_output_paying_the_ssp() {
    let output = |n: u64, address: &str| json!({"n":n,"scriptPubKey":{"address":address}});
    let transaction = json!({"vout":[output(0, "bcrt1change"), output(1, "bcrt1ssp")]});
    assert_eq!(
        funding::deposit_output(&transaction, "bcrt1ssp").unwrap(),
        1
    );
    assert!(funding::deposit_output(&transaction, "bcrt1other").is_err());
    let twice = json!({"vout":[output(0, "bcrt1ssp"), output(1, "bcrt1ssp")]});
    assert!(funding::deposit_output(&twice, "bcrt1ssp").is_err());
}

#[test]
fn channel_must_be_ready_and_usable_with_the_counterparty() {
    let channel = json!({"counterparty_node_id":"peer","is_channel_ready":true,"is_usable":true});
    assert!(funding::usable(&json!({"channels":[channel.clone()]}), "peer").unwrap());
    assert!(!funding::usable(&json!({"channels":[channel.clone()]}), "other").unwrap());
    assert!(!funding::usable(&json!({"channels":[]}), "peer").unwrap());
    for field in ["is_channel_ready", "is_usable"] {
        let mut pending = channel.clone();
        pending[field] = json!(false);
        assert!(!funding::usable(&json!({"channels":[pending]}), "peer").unwrap());
    }
    assert!(funding::usable(&json!({}), "peer").is_err());
}

#[test]
fn ldk_payment_records_settle_only_the_exact_payment() {
    let record = |status: &str| json!({"payment":{"payment_id":"hash","kind":{"kind":{"bolt11":{"hash":"hash","preimage":null}}},"amount_msat":30_000_000,"direction":"INBOUND","status":status}});
    assert!(!payments::settled(&record("PENDING"), "hash", "INBOUND", 30_000_000).unwrap());
    assert!(payments::settled(&record("SUCCEEDED"), "hash", "INBOUND", 30_000_000).unwrap());
    assert!(payments::settled(&record("FAILED"), "hash", "INBOUND", 30_000_000).is_err());
    assert!(payments::settled(&record("SUCCEEDED"), "hash", "INBOUND", 30_000_001).is_err());
    assert!(payments::settled(&record("SUCCEEDED"), "other", "INBOUND", 30_000_000).is_err());
    assert!(payments::settled(&record("PENDING"), "hash", "OUTBOUND", 30_000_000).is_err());
    assert!(payments::settled(&Value::Null, "hash", "INBOUND", 30_000_000).is_err());
    // A node with no record of the payment yet has not received it.
    assert!(!payments::settled(&json!({"payment":null}), "hash", "INBOUND", 30_000_000).unwrap());
    let unpaid = |details: &Value| payments::receive_unpaid(details, "hash", 30_000_000);
    assert!(unpaid(&json!({"payment":null})).unwrap());
    assert!(unpaid(&record("PENDING")).unwrap());
    assert!(!unpaid(&record("SUCCEEDED")).unwrap());
    assert!(unpaid(&record("FAILED")).is_err());
}

#[test]
fn inbound_payments_are_found_by_hash_in_the_payment_list() {
    // LDK Server 762bc47 gives inbound payments a payment_id other than their
    // hash: get-payment-details <hash> stays null even after one succeeds.
    let payment = |id: &str, hash: &str| json!({"payment_id":id,"kind":{"kind":{"bolt11":{"hash":hash}}},"direction":"INBOUND","status":"SUCCEEDED","amount_msat":1});
    let list = json!({"list":[payment("a", "other"), payment("b", "hash")]});
    assert_eq!(
        payments::inbound(&list, "hash").unwrap()["payment"]["payment_id"],
        "b"
    );
    assert_eq!(
        payments::inbound(&list, "missing").unwrap(),
        json!({"payment":null})
    );
    assert_eq!(
        payments::inbound(&json!({}), "hash").unwrap(),
        json!({"payment":null})
    );
    let duplicate = json!({"list":[payment("a", "hash"), payment("b", "hash")]});
    assert!(payments::inbound(&duplicate, "hash").is_err());
    assert!(payments::inbound(&json!({"list":"x"}), "hash").is_err());
}

#[test]
fn invoices_must_pay_the_ssp_node_the_quoted_amount() {
    let decoded = json!({"destination":"ssp","payment_hash":"hash","amount_msat":2_000_000});
    assert_eq!(
        payments::check_invoice(&decoded, 2_000_000, "ssp").unwrap(),
        "hash"
    );
    assert!(payments::check_invoice(&decoded, 2_000_001, "ssp").is_err());
    assert!(payments::check_invoice(&decoded, 2_000_000, "peer").is_err());
    assert!(payments::check_invoice(&json!({"destination":"ssp"}), 2_000_000, "ssp").is_err());
}

#[test]
fn unreachable_melt_is_never_paid_and_preimages_are_only_recorded() {
    let state = |state: &str| json!({"quote":"q","state":state});
    assert!(payments::unpaid(&state("UNPAID"), "q").unwrap());
    assert!(!payments::unpaid(&state("PENDING"), "q").unwrap());
    assert!(payments::unpaid(&state("PAID"), "q").is_err());
    assert!(payments::unpaid(&state("UNPAID"), "other").is_err());
    assert!(payments::carries_preimage(
        &json!({"state":"PAID","payment_preimage":"00"})
    ));
    for quote in [
        json!({"state":"PAID"}),
        json!({"state":"PAID","payment_preimage":null}),
        json!({"state":"PAID","payment_preimage":""}),
    ] {
        assert!(!payments::carries_preimage(&quote));
    }
}

#[test]
fn processor_and_mint_advertise_only_bolt11_in_sat() {
    let settings = json!({
        "unit":"sat",
        "bolt11":{"mpp":false,"amountless":false,"invoice_description":false},
        "bolt12":null
    });
    check_settings(&settings).unwrap();
    for (field, value) in [
        ("unit", json!("msat")),
        ("bolt11", Value::Null),
        (
            "bolt12",
            json!({"amountless":false,"invoice_description":false}),
        ),
        ("onchain", json!({"confirmations":1})),
        ("custom", json!({"arkoor":"{}"})),
    ] {
        let mut bad = settings.clone();
        bad[field] = value;
        assert!(check_settings(&bad).is_err(), "{field}");
    }
    let info = |methods: Value| json!({"nuts":{"4":{"methods":methods},"5":{"methods":methods}}});
    stack::check_registered(&info(json!([{"method":"bolt11","unit":"sat"}])), METHODS).unwrap();
    for bad in [
        json!([{"method":"bolt11","unit":"msat"}]),
        json!([{"method":"bolt11","unit":"sat"},{"method":"bolt12","unit":"sat"}]),
        json!([]),
    ] {
        assert!(
            stack::check_registered(&info(bad.clone()), METHODS).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn transport_probes_use_a_foreign_authority_for_each_endpoint() {
    let [processor, operators] = transport_targets();
    assert_eq!(processor.forward, "service/processor");
    assert!(processor.own.identity.is_some());
    assert_eq!(
        processor.foreign.identity,
        Some(("server-0.crt", "server-0.key"))
    );
    assert_ne!(processor.own.secret, processor.foreign.secret);
    // The operators take no client certificate; only their authority is probed.
    assert_eq!(operators.forward, "pod/operators-0");
    assert!(operators.own.identity.is_none());
    assert_eq!(operators.own.ca, "ca.crt");
    assert_ne!(operators.own.secret, operators.foreign.secret);
}
