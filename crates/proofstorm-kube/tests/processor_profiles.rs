//! Mint-side contracts for reserved Bark support, without fabricating a catalog
//! image or making the incomplete managed Bark stack installable.
use proofstorm_core::{
    CellSpec, ComponentKind, ComponentPlanContract, DependencyBinding, LinkKind, PaymentMethod,
    ProcessorProfile, default_catalog, resolve_lock,
};
use proofstorm_kube::{compile_component_plans, render_cdk_component};

fn mint_plan(profile: ProcessorProfile) -> ComponentPlanContract {
    let spec: CellSpec =
        serde_json::from_str(include_str!("../../../examples/ldk-server-cell.json")).unwrap();
    let lock = resolve_lock(&spec, default_catalog()).unwrap();
    let mut mint = compile_component_plans("i-profile-test", "sha256:profile-test", &spec, &lock)
        .unwrap()
        .into_iter()
        .find(|plan| plan.component_id == "mint")
        .unwrap();
    if profile == ProcessorProfile::Bark {
        mint.relevant_links.retain(|link| link.id != "mint-bolt12");
        mint.linked_targets.remove("mint-bolt12");
        let link = mint
            .relevant_links
            .iter_mut()
            .find(|link| link.id == "mint-bolt11")
            .unwrap();
        link.to = "bark-processor".into();
        let target = mint.linked_targets.get_mut(&link.id).unwrap();
        target.backend_id = profile.implementation().into();
        target.component_id.clone_from(&link.to);
    }
    mint
}

#[test]
fn bark_mint_selects_its_own_authenticated_readiness_profile() {
    let rendered = render_cdk_component(&mint_plan(ProcessorProfile::Bark)).unwrap();
    let config = &rendered.config_maps[0].data.as_ref().unwrap()["config.toml"];
    for required in [
        "backend = \"grpcprocessor\"",
        "supported_units = [\"sat\"]",
        "address = \"bark-processor\"",
        "allow_insecure = false",
    ] {
        assert!(config.contains(required), "missing {required}");
    }
    let deployment = serde_json::to_value(&rendered.deployments[0]).unwrap();
    let pod = &deployment["spec"]["template"]["spec"];
    let wait = pod["initContainers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|init| init["name"] == "wait-for-payment-processor")
        .unwrap();
    let command = wait["command"][2].as_str().unwrap();
    assert!(command.contains(
        "processor-settings https://bark-processor:50051 /payment-processor/tls cdk-bark-processor"
    ));
    assert!(!command.contains("cdk-ldk-server-processor"));
    let volume = pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == "payment-processor")
        .unwrap();
    assert_eq!(volume["secret"]["secretName"], "bark-processor-payment-tls");
    assert_eq!(volume["secret"]["defaultMode"], 288);
    let keys: Vec<_> = volume["secret"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["ca.pem", "client.pem", "client.key"]);
    let claims: Vec<_> = pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|volume| volume["persistentVolumeClaim"]["claimName"].as_str())
        .collect();
    assert_eq!(claims, ["mint-data"]);
    for container in [&pod["containers"][0], wait] {
        let mount = container["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mount| mount["name"] == "payment-processor")
            .unwrap();
        assert_eq!(mount["readOnly"], true);
    }
}

fn corrupt_plan(plan: &mut ComponentPlanContract, mutation: &str) {
    let index = plan
        .relevant_links
        .iter()
        .position(|l| l.id == "mint-bolt11")
        .unwrap();
    match mutation {
        "missing-binding" => plan.relevant_links[index].binding = None,
        "wrong-unit" => {
            plan.relevant_links[index].binding = Some(DependencyBinding::Payment {
                method: PaymentMethod::Bolt11,
                unit: "msat".into(),
            });
        }
        "onchain" => {
            plan.relevant_links[index].binding = Some(DependencyBinding::Payment {
                method: PaymentMethod::Onchain,
                unit: "sat".into(),
            });
        }
        "duplicate-binding" => {
            let mut duplicate = plan.relevant_links[index].clone();
            duplicate.id = "duplicate".into();
            plan.linked_targets.insert(
                duplicate.id.clone(),
                plan.linked_targets["mint-bolt11"].clone(),
            );
            plan.relevant_links.push(duplicate);
        }
        "wrong-source" => plan.relevant_links[index].from = "other-mint".into(),
        "wrong-endpoint" => plan.relevant_links[index].to = "other-processor".into(),
        "unknown-profile" | "wrong-kind" => {
            // Apply to every descriptor, so the check cannot be satisfied by
            // selecting the second valid descriptor in an LDK plan.
            for target in plan.linked_targets.values_mut() {
                if mutation == "unknown-profile" {
                    target.backend_id = "unknown-processor".into();
                } else {
                    target.kind = ComponentKind::Lightning;
                }
            }
        }
        _ => panic!("unknown mutation {mutation}"),
    }
}

#[test]
fn forged_plans_cannot_bypass_payment_binding_validation() {
    for profile in [ProcessorProfile::LdkServer, ProcessorProfile::Bark] {
        let plan = mint_plan(profile);
        assert!(render_cdk_component(&plan).is_ok());
        for mutation in [
            "missing-binding",
            "wrong-unit",
            "onchain",
            "duplicate-binding",
            "wrong-source",
            "wrong-endpoint",
            "unknown-profile",
            "wrong-kind",
        ] {
            let mut invalid = plan.clone();
            corrupt_plan(&mut invalid, mutation);
            assert!(
                render_cdk_component(&invalid).is_err(),
                "{profile:?}, {mutation}"
            );
            invalid.relevant_links.reverse();
            assert!(
                render_cdk_component(&invalid).is_err(),
                "{profile:?}, {mutation}, reversed"
            );
        }
    }
}

#[test]
fn missing_and_undeclared_rails_cannot_change_the_selected_profile() {
    let mut ldk = mint_plan(ProcessorProfile::LdkServer);
    ldk.relevant_links.retain(|link| link.id != "mint-bolt12");
    assert!(render_cdk_component(&ldk).is_err());
    let bark = mint_plan(ProcessorProfile::Bark);
    for method in [PaymentMethod::Bolt12, PaymentMethod::Onchain] {
        let mut invalid = bark.clone();
        let mut extra = invalid.relevant_links[0].clone();
        assert_eq!(extra.kind, LinkKind::PaymentBackend);
        extra.id = "extra".into();
        extra.binding = Some(DependencyBinding::Payment {
            method,
            unit: "sat".into(),
        });
        invalid.linked_targets.insert(
            extra.id.clone(),
            invalid.linked_targets["mint-bolt11"].clone(),
        );
        invalid.relevant_links.push(extra);
        assert!(render_cdk_component(&invalid).is_err(), "{method:?}");
    }
}

#[test]
fn ldk_methods_cannot_mix_profiles_endpoints_or_descriptors() {
    let plan = mint_plan(ProcessorProfile::LdkServer);
    for mutation in ["profile", "endpoint", "descriptor"] {
        let mut invalid = plan.clone();
        let target = invalid.linked_targets.get_mut("mint-bolt12").unwrap();
        match mutation {
            "profile" => target.backend_id = ProcessorProfile::Bark.implementation().into(),
            "endpoint" => {
                target.component_id = "another-ldk".into();
                invalid
                    .relevant_links
                    .iter_mut()
                    .find(|l| l.id == "mint-bolt12")
                    .unwrap()
                    .to
                    .clone_from(&target.component_id);
            }
            "descriptor" => {
                // Same component id and implementation still need one descriptor.
                target.ports.insert("grpc".into(), 50052);
            }
            _ => unreachable!(),
        }
        assert!(render_cdk_component(&invalid).is_err(), "{mutation}");
    }
}
