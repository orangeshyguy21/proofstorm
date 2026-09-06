use std::{collections::BTreeMap, fs, path::PathBuf};

use proofstorm_core::{
    API_VERSION, AuthenticationProtocol, BitcoinNetwork, Capability, ComponentKind, ComponentSpec,
    ControlClass, DatabaseRole, DependencyBinding, LabPolicy, LabSpec, LinkKind, LinkSpec,
    PaymentMethod, default_backend_registry, default_catalog, resolve_lock,
};
use proofstorm_kube::{
    ComponentForensicsAction, LabAction, ProofstormLab, ProofstormLabAction,
    ProofstormLabActionSpec, ProofstormLabSpec, RenderedComponent, compile_component_plans,
    render_attacker_component, render_bitcoin_component, render_cdk_component,
    render_cln_component, render_keycloak_component, render_lab, render_lab_action_job,
    render_lnd_component, render_nutshell_mint_component, render_postgres_component,
    render_redis_component, render_security_spine, render_wallet_component,
};
use serde_json::{Value, json};

const INSTANCE_KEY: &str = "i-golden-b2";
const REVISION_DIGEST: &str = "sha256:b2-golden-revision";

fn component(
    id: &str,
    kind: ComponentKind,
    implementation: &str,
    control: ControlClass,
) -> ComponentSpec {
    ComponentSpec {
        id: id.into(),
        kind,
        implementation: implementation.into(),
        version: (implementation == "cocod-wallet").then(|| "0.0.17-dev.44e5101c".into()),
        config_version: match implementation {
            "bitcoin-core" => "bitcoin-core/30/v1",
            "lnd" => "lnd/0.20/v1",
            "cln" => "cln/26.06/v1",
            "cdk" => "cdk-mintd/0.18/v1",
            "cdk-ldk" => "cdk-mintd-ldk/0.18/v1",
            "cdk-bdk" => "cdk-mintd-bdk/0.18/v1",
            "nutshell" => "nutshell-mint/0.20/v1",
            "postgresql" => "postgresql/17/v1",
            "redis" => "redis/8.10/v1",
            "keycloak" => "keycloak/25/v1",
            "nutshell-wallet" => "nutshell-wallet/0.20/v1",
            "cdk-cli-wallet" => "cdk-cli-wallet/0.18/v1",
            "cocod-wallet" => "cocod-wallet/0.0.17/v1",
            "attacker-workspace" => "attacker-workspace/0.1/v1",
            _ => panic!("unknown test implementation {implementation:?}"),
        }
        .into(),
        control,
        config: BTreeMap::new(),
    }
}

fn lab(name: &str, components: Vec<ComponentSpec>, links: Vec<LinkSpec>) -> LabSpec {
    LabSpec {
        api_version: API_VERSION.into(),
        name: name.into(),
        components,
        links,
        policy: LabPolicy::default(),
    }
}

fn chain_link(from: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: format!("{from}-{to}-chain"),
        kind: LinkKind::ChainBackend,
        from: from.into(),
        to: to.into(),
        binding: Some(DependencyBinding::Chain {
            network: BitcoinNetwork::Regtest,
        }),
    }
}

fn lightning_link(from: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: format!("{from}-{to}-lightning"),
        kind: LinkKind::PaymentBackend,
        from: from.into(),
        to: to.into(),
        binding: Some(DependencyBinding::Payment {
            method: PaymentMethod::Bolt11,
            unit: "sat".into(),
        }),
    }
}

fn database_link(from: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: format!("{from}-{to}-database"),
        kind: LinkKind::DatabaseBackend,
        from: from.into(),
        to: to.into(),
        binding: Some(DependencyBinding::Database {
            role: DatabaseRole::Primary,
        }),
    }
}

fn cache_link(from: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: format!("{from}-{to}-cache"),
        kind: LinkKind::DatabaseBackend,
        from: from.into(),
        to: to.into(),
        binding: Some(DependencyBinding::Database {
            role: DatabaseRole::Cache,
        }),
    }
}

fn authentication_link(from: &str, to: &str) -> LinkSpec {
    LinkSpec {
        id: format!("{from}-{to}-authentication"),
        kind: LinkKind::AuthenticationBackend,
        from: from.into(),
        to: to.into(),
        binding: Some(DependencyBinding::Authentication {
            protocol: AuthenticationProtocol::Oidc,
        }),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the golden matrix intentionally keeps every backend fixture in one exhaustive match"
)]
fn backend_lab(backend_id: &str) -> (LabSpec, &'static str) {
    match backend_id {
        "bitcoin-core" => (
            lab(
                "golden-bitcoin",
                vec![component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                )],
                vec![],
            ),
            "chain",
        ),
        "lnd" | "cln" => (
            lab(
                &format!("golden-{backend_id}"),
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        backend_id,
                        ControlClass::Laboratory,
                    ),
                ],
                vec![chain_link("lightning", "chain")],
            ),
            "lightning",
        ),
        "cdk" => (
            lab(
                "golden-cdk",
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        "lnd",
                        ControlClass::Laboratory,
                    ),
                    component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
                ],
                vec![
                    chain_link("lightning", "chain"),
                    lightning_link("mint", "lightning"),
                ],
            ),
            "mint",
        ),
        "cdk-ldk" => cdk_ldk_backend_lab(),
        "cdk-bdk" => cdk_bdk_backend_lab(),
        "nutshell" => (
            lab(
                "golden-nutshell",
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Laboratory,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        "lnd",
                        ControlClass::Laboratory,
                    ),
                    component(
                        "mint",
                        ComponentKind::Mint,
                        "nutshell",
                        ControlClass::Target,
                    ),
                ],
                vec![
                    chain_link("lightning", "chain"),
                    lightning_link("mint", "lightning"),
                ],
            ),
            "mint",
        ),
        "nutshell-wallet" | "cdk-cli-wallet" | "cocod-wallet" => (
            lab(
                "golden-wallet",
                vec![component(
                    "wallet",
                    ComponentKind::Wallet,
                    backend_id,
                    ControlClass::Laboratory,
                )],
                vec![],
            ),
            "wallet",
        ),
        "postgresql" => (
            lab(
                "golden-postgresql",
                vec![component(
                    "database",
                    ComponentKind::Database,
                    "postgresql",
                    ControlClass::Laboratory,
                )],
                vec![],
            ),
            "database",
        ),
        "redis" => (
            lab(
                "golden-redis",
                vec![component(
                    "cache",
                    ComponentKind::Database,
                    "redis",
                    ControlClass::Laboratory,
                )],
                vec![],
            ),
            "cache",
        ),
        "keycloak" => (
            lab(
                "golden-keycloak",
                vec![
                    component(
                        "database",
                        ComponentKind::Database,
                        "postgresql",
                        ControlClass::Laboratory,
                    ),
                    component(
                        "identity",
                        ComponentKind::IdentityProvider,
                        "keycloak",
                        ControlClass::Laboratory,
                    ),
                ],
                vec![database_link("identity", "database")],
            ),
            "identity",
        ),
        "attacker-workspace" => (
            lab(
                "golden-attacker",
                vec![component(
                    "attacker",
                    ComponentKind::Attacker,
                    "attacker-workspace",
                    ControlClass::Attacker,
                )],
                vec![],
            ),
            "attacker",
        ),
        _ => panic!("uncharacterized backend {backend_id}"),
    }
}

fn cdk_ldk_backend_lab() -> (LabSpec, &'static str) {
    (
        lab(
            "golden-cdk-ldk",
            vec![
                component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component("mint", ComponentKind::Mint, "cdk-ldk", ControlClass::Target),
            ],
            vec![chain_link("mint", "chain")],
        ),
        "mint",
    )
}

fn cdk_bdk_backend_lab() -> (LabSpec, &'static str) {
    (
        lab(
            "golden-cdk-bdk",
            vec![
                component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component("mint", ComponentKind::Mint, "cdk-bdk", ControlClass::Target),
            ],
            vec![chain_link("mint", "chain")],
        ),
        "mint",
    )
}

fn render_backend(backend_id: &str) -> Value {
    let (lab, component_id) = backend_lab(backend_id);
    let lock = resolve_lock(&lab, default_catalog()).expect("backend lock");
    let plans =
        compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &lab, &lock).expect("backend plans");
    let plan = plans
        .iter()
        .find(|plan| plan.component_id == component_id)
        .expect("target plan");
    let rendered = match backend_id {
        "bitcoin-core" => render_bitcoin_component(plan),
        "lnd" => render_lnd_component(plan),
        "cln" => render_cln_component(plan),
        "cdk" | "cdk-ldk" | "cdk-bdk" => render_cdk_component(plan),
        "nutshell" => render_nutshell_mint_component(plan),
        "nutshell-wallet" => render_wallet_component(plan),
        "cdk-cli-wallet" => proofstorm_kube::render_cdk_wallet_component(plan),
        "cocod-wallet" => proofstorm_kube::render_cocod_wallet_component(plan),
        "postgresql" => render_postgres_component(plan),
        "redis" => render_redis_component(plan),
        "keycloak" => render_keycloak_component(plan),
        "attacker-workspace" => render_attacker_component(plan),
        _ => panic!("uncharacterized backend {backend_id}"),
    }
    .expect("backend render");
    assert_component_security(&rendered);
    component_snapshot(plan, &rendered)
}

fn component_snapshot(
    plan: &proofstorm_core::ComponentPlanContract,
    rendered: &RenderedComponent,
) -> Value {
    json!({
        "plan": plan,
        "resources": {
            "configMaps": &rendered.config_maps,
            "secrets": &rendered.secrets,
            "services": &rendered.services,
            "statefulSets": &rendered.stateful_sets,
            "deployments": &rendered.deployments,
            "persistentVolumeClaims": &rendered.persistent_volume_claims,
        }
    })
}

fn assert_component_security(rendered: &RenderedComponent) {
    for workload in rendered
        .stateful_sets
        .iter()
        .map(|resource| serde_json::to_value(resource).expect("StatefulSet JSON"))
        .chain(
            rendered
                .deployments
                .iter()
                .map(|resource| serde_json::to_value(resource).expect("Deployment JSON")),
        )
    {
        let controller_owned =
            workload.pointer("/metadata/name") == Some(&json!("proofstorm-protocol-prober"));
        assert!(
            controller_owned
                || workload
                    .pointer("/metadata/annotations/proofstorm.dev~1backend-id")
                    .is_some(),
            "backend workload must retain backend identity"
        );
        let pod = workload.pointer("/spec/template/spec").expect("Pod spec");
        assert_eq!(pod["automountServiceAccountToken"], json!(false));
        assert_eq!(pod["serviceAccountName"], json!("proofstorm-workload"));
        assert_eq!(pod["securityContext"]["runAsNonRoot"], json!(true));
        assert_eq!(
            pod["securityContext"]["seccompProfile"]["type"],
            json!("RuntimeDefault")
        );
        for container in pod["containers"].as_array().expect("containers") {
            assert!(
                container["image"]
                    .as_str()
                    .is_some_and(|image| image.contains("@sha256:")),
                "container image must be immutable"
            );
            assert_eq!(
                container["securityContext"]["allowPrivilegeEscalation"],
                json!(false)
            );
            assert_eq!(
                container["securityContext"]["capabilities"]["drop"],
                json!(["ALL"])
            );
        }
    }
}

fn full_baseline_lab() -> LabSpec {
    lab(
        "golden-full-baseline",
        vec![
            component(
                "chain-a",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "chain-b",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lnd",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            component(
                "cln",
                ComponentKind::Lightning,
                "cln",
                ControlClass::Laboratory,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
            component(
                "wallet",
                ComponentKind::Wallet,
                "nutshell-wallet",
                ControlClass::Laboratory,
            ),
            component(
                "attacker",
                ComponentKind::Attacker,
                "attacker-workspace",
                ControlClass::Attacker,
            ),
        ],
        vec![
            chain_link("lnd", "chain-a"),
            chain_link("cln", "chain-a"),
            lightning_link("mint", "lnd"),
        ],
    )
}

fn cdk_cln_lab() -> LabSpec {
    lab(
        "golden-cdk-cln",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "mint-cln",
                ComponentKind::Lightning,
                "cln",
                ControlClass::Laboratory,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ],
        vec![
            chain_link("mint-cln", "chain"),
            lightning_link("mint", "mint-cln"),
        ],
    )
}

fn nutshell_cln_lab() -> LabSpec {
    lab(
        "golden-nutshell-cln",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "mint-cln",
                ComponentKind::Lightning,
                "cln",
                ControlClass::Laboratory,
            ),
            component(
                "mint",
                ComponentKind::Mint,
                "nutshell",
                ControlClass::Target,
            ),
        ],
        vec![
            chain_link("mint-cln", "chain"),
            lightning_link("mint", "mint-cln"),
        ],
    )
}

fn assert_postgres_bootstrap_env(container: &Value) {
    let environment = container["env"]
        .as_array()
        .expect("CDK container environment");
    let postgres_url = environment
        .iter()
        .find(|entry| entry["name"] == "CDK_MINTD_POSTGRES_URL")
        .expect("PostgreSQL bootstrap URL");
    assert_eq!(
        postgres_url["valueFrom"]["secretKeyRef"],
        json!({"name": "database-credentials", "key": "DATABASE_URL"})
    );
}

#[test]
fn cdk_postgres_binding_materializes_secret_backed_native_configuration() {
    let spec = lab(
        "golden-cdk-postgres",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Laboratory,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("mint", "database"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("PostgreSQL lab lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("PostgreSQL lab render");
    let secret = rendered
        .secrets
        .iter()
        .find(|secret| secret.metadata.name.as_deref() == Some("database-credentials"))
        .expect("database credential template");
    assert_eq!(
        secret.string_data.as_ref().unwrap()["POSTGRES_DB"],
        "cdk_mint"
    );
    assert!(
        !secret
            .string_data
            .as_ref()
            .unwrap()
            .contains_key("POSTGRES_PASSWORD")
    );
    let mint_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .and_then(|data| data.get("config.toml"))
        .expect("mint public config");
    assert!(!mint_config.contains("postgresql://"));
    assert!(mint_config.contains("[database]\nengine = \"postgres\""));
    assert!(mint_config.contains("url = \"env:CDK_MINTD_POSTGRES_URL\""));
    let mint = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .expect("mint deployment");
    let mint = serde_json::to_value(mint).expect("mint JSON");
    assert_eq!(
        mint.pointer("/spec/template/spec/initContainers/0/name"),
        Some(&json!("initialize-config"))
    );
    assert_postgres_bootstrap_env(&mint["spec"]["template"]["spec"]["containers"][0]);
    assert_postgres_bootstrap_env(&mint["spec"]["template"]["spec"]["initContainers"][0]);
    assert_golden(
        "cdk-postgres-lab",
        &json!({
            "plans": &rendered.plans,
            "resources": {
                "configMaps": &rendered.config_maps,
                "secrets": &rendered.secrets,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            }
        }),
    );
}

#[test]
fn nutshell_postgres_binding_keeps_database_and_mint_secrets_out_of_public_config() {
    let spec = lab(
        "golden-nutshell-postgres",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Laboratory,
            ),
            component(
                "mint",
                ComponentKind::Mint,
                "nutshell",
                ControlClass::Target,
            ),
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("mint", "database"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell PostgreSQL lock");
    let rendered = render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock)
        .expect("Nutshell PostgreSQL render");
    let public_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .expect("Nutshell public configuration");
    assert!(!public_config.contains_key("MINT_DATABASE"));
    assert!(!public_config.contains_key("MINT_PRIVATE_KEY"));
    assert_eq!(public_config["MINT_AUTH_DATABASE"], "/app/data");
    let mint_secret = rendered
        .secrets
        .iter()
        .find(|secret| secret.metadata.name.as_deref() == Some("mint-credentials"))
        .expect("Nutshell generated secret template");
    assert_eq!(
        mint_secret.string_data.as_ref().unwrap()["PROOFSTORM_SECRET_KIND"],
        "nutshell-mint"
    );
    assert!(
        !mint_secret
            .string_data
            .as_ref()
            .unwrap()
            .contains_key("MINT_PRIVATE_KEY")
    );
    let deployment = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .expect("Nutshell deployment");
    let deployment = serde_json::to_value(deployment).expect("deployment JSON");
    let env = deployment
        .pointer("/spec/template/spec/containers/0/env")
        .and_then(Value::as_array)
        .expect("secret-backed environment");
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_DATABASE"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "database-credentials"
            && entry["valueFrom"]["secretKeyRef"]["key"] == "DATABASE_URL"
    }));
    assert!(
        !env.iter()
            .any(|entry| entry["name"] == "MINT_AUTH_DATABASE")
    );
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_PRIVATE_KEY"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "mint-credentials"
    }));
}

#[test]
fn nutshell_oidc_auth_projects_exact_upstream_contract_and_persistent_auth_ledger() {
    let mut mint = component(
        "mint",
        ComponentKind::Mint,
        "nutshell",
        ControlClass::Target,
    );
    mint.config.insert(
        "oidc_discovery_url".into(),
        json!("http://identity:8080/realms/proofstorm/.well-known/openid-configuration"),
    );
    mint.config
        .insert("oidc_client_id".into(), json!("proofstorm-wallet"));
    mint.config
        .insert("auth_rate_limit_per_minute".into(), json!(7));
    mint.config
        .insert("auth_max_blind_tokens".into(), json!(64));
    let spec = lab(
        "golden-nutshell-oidc",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            mint,
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell OIDC lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell OIDC render");
    let public_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .expect("Nutshell public configuration");
    assert_eq!(public_config["MINT_REQUIRE_AUTH"], "TRUE");
    assert_eq!(
        public_config["MINT_AUTH_OICD_DISCOVERY_URL"],
        "http://identity:8080/realms/proofstorm/.well-known/openid-configuration"
    );
    assert_eq!(
        public_config["MINT_AUTH_OICD_CLIENT_ID"],
        "proofstorm-wallet"
    );
    assert_eq!(public_config["MINT_AUTH_RATE_LIMIT_PER_MINUTE"], "7");
    assert_eq!(public_config["MINT_AUTH_MAX_BLIND_TOKENS"], "64");
    assert_eq!(public_config["MINT_AUTH_DATABASE"], "/app/data");
    assert_golden(
        "nutshell-oidc-lab",
        &json!({
            "plans": &rendered.plans,
            "resources": {
                "configMaps": &rendered.config_maps,
                "secrets": &rendered.secrets,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            }
        }),
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the OIDC golden keeps provider topology, secret boundaries, and mint projection in one contract"
)]
fn nutshell_keycloak_link_derives_oidc_topology_and_keeps_provider_credentials_private() {
    let spec = lab(
        "golden-nutshell-keycloak",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            component(
                "identity-db",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Laboratory,
            ),
            component(
                "identity",
                ComponentKind::IdentityProvider,
                "keycloak",
                ControlClass::Laboratory,
            ),
            component(
                "mint",
                ComponentKind::Mint,
                "nutshell",
                ControlClass::Target,
            ),
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("identity", "identity-db"),
            authentication_link("mint", "identity"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell Keycloak lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell Keycloak render");
    let mint_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .expect("Nutshell public configuration");
    assert_eq!(mint_config["MINT_REQUIRE_AUTH"], "TRUE");
    assert_eq!(mint_config["MINT_AUTH_DATABASE"], "/app/data");
    assert_eq!(
        mint_config["MINT_AUTH_OICD_DISCOVERY_URL"],
        "http://identity:8080/realms/proofstorm/.well-known/openid-configuration"
    );
    let identity_secret = rendered
        .secrets
        .iter()
        .find(|secret| secret.metadata.name.as_deref() == Some("identity-credentials"))
        .expect("Keycloak generated secret template");
    assert_eq!(
        identity_secret.string_data.as_ref().unwrap(),
        &BTreeMap::from([
            ("OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS".into(), "300".into(),),
            ("PROOFSTORM_SECRET_KIND".into(), "keycloak-oidc".into()),
        ])
    );
    let identity = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("identity"))
        .map(serde_json::to_value)
        .transpose()
        .expect("Keycloak deployment JSON")
        .expect("Keycloak deployment");
    assert_eq!(
        identity.pointer("/spec/template/spec/volumes/0/secret/secretName"),
        Some(&json!("identity-credentials"))
    );
    let mint = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .map(serde_json::to_value)
        .transpose()
        .expect("Nutshell deployment JSON")
        .expect("Nutshell deployment");
    assert_eq!(
        mint.pointer("/spec/template/spec/initContainers/0/name"),
        Some(&json!("wait-for-oidc"))
    );
    assert_eq!(
        mint.pointer("/spec/template/spec/containers/0/command/2"),
        Some(&json!("from cashu.mint.main import main; main()"))
    );
    assert_eq!(
        mint.pointer("/spec/template/spec/containers/0/readinessProbe/exec/command/3"),
        Some(&json!("http://127.0.0.1:3338/v1/info")),
        "Nutshell readiness must use its rate-limit-exempt loopback path"
    );
    assert!(
        mint.pointer("/spec/template/spec/containers/0/readinessProbe/httpGet")
            .is_none(),
        "a kubelet HTTP probe would consume Nutshell's global request quota"
    );
    let protocol_prober = rendered
        .deployments
        .iter()
        .find(|deployment| {
            deployment.metadata.name.as_deref() == Some("proofstorm-protocol-prober")
        })
        .map(serde_json::to_value)
        .transpose()
        .expect("protocol prober JSON")
        .expect("protocol prober deployment");
    let mint_probe = protocol_prober
        .pointer("/spec/template/spec/containers")
        .and_then(Value::as_array)
        .and_then(|containers| {
            containers.iter().find(|container| {
                container
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.starts_with("probe-mint-"))
            })
        })
        .expect("Nutshell protocol probe");
    assert_eq!(
        mint_probe.pointer("/readinessProbe/exec/command/0"),
        Some(&json!("nc")),
        "the remote probe must verify reachability without making an HTTP request"
    );
    assert_golden(
        "nutshell-keycloak-lab",
        &json!({
            "plans": &rendered.plans,
            "resources": {
                "configMaps": &rendered.config_maps,
                "secrets": &rendered.secrets,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            }
        }),
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the Redis golden contract keeps topology, public settings, and secret projections together"
)]
fn nutshell_redis_binding_is_private_typed_and_independent_of_primary_storage() {
    let mut mint = component(
        "mint",
        ComponentKind::Mint,
        "nutshell",
        ControlClass::Target,
    );
    mint.config
        .insert("redis_cache_ttl_seconds".into(), json!(900));
    let spec = lab(
        "golden-nutshell-redis",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Laboratory,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Laboratory,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Laboratory,
            ),
            component(
                "cache",
                ComponentKind::Database,
                "redis",
                ControlClass::Laboratory,
            ),
            mint,
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("mint", "database"),
            cache_link("mint", "cache"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell Redis lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell Redis render");
    let public_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .expect("Nutshell public configuration");
    assert_eq!(public_config["MINT_REDIS_CACHE_ENABLED"], "TRUE");
    assert_eq!(public_config["MINT_REDIS_CACHE_CLUSTER"], "FALSE");
    assert_eq!(public_config["MINT_REDIS_CACHE_TTL"], "900");
    assert!(!public_config.contains_key("MINT_REDIS_CACHE_URL"));
    assert!(!public_config.contains_key("MINT_DATABASE"));
    let cache_secret = rendered
        .secrets
        .iter()
        .find(|secret| secret.metadata.name.as_deref() == Some("cache-credentials"))
        .expect("Redis generated secret template");
    assert_eq!(
        cache_secret.string_data.as_ref().unwrap(),
        &BTreeMap::from([("PROOFSTORM_SECRET_KIND".into(), "redis-cache".into())])
    );
    let deployment = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .expect("Nutshell deployment");
    let deployment = serde_json::to_value(deployment).expect("deployment JSON");
    let env = deployment
        .pointer("/spec/template/spec/containers/0/env")
        .and_then(Value::as_array)
        .expect("secret-backed environment");
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_DATABASE"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "database-credentials"
    }));
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_REDIS_CACHE_URL"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "cache-credentials"
            && entry["valueFrom"]["secretKeyRef"]["key"] == "REDIS_URL"
    }));
    assert_golden(
        "nutshell-redis-lab",
        &json!({
            "plans": &rendered.plans,
            "resources": {
                "configMaps": &rendered.config_maps,
                "secrets": &rendered.secrets,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            }
        }),
    );
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.json"))
}

fn assert_golden(name: &str, actual: &Value) {
    let path = golden_path(name);
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(actual).expect("golden JSON")
    );
    if std::env::var("UPDATE_GOLDENS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().expect("golden directory")).expect("create goldens");
        fs::write(&path, &rendered).expect("write golden");
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "cannot read golden {}: {error}; run UPDATE_GOLDENS=1 cargo test -p proofstorm-kube --test golden_rendering",
            path.display()
        )
    });
    assert_eq!(rendered, expected, "golden drift for {}", path.display());
}

#[test]
fn every_registered_backend_matches_its_golden_contract() {
    let characterized = [
        "attacker-workspace",
        "bitcoin-core",
        "cdk",
        "cdk-bdk",
        "cdk-cli-wallet",
        "cdk-ldk",
        "cln",
        "cocod-wallet",
        "keycloak",
        "lnd",
        "nutshell",
        "nutshell-wallet",
        "postgresql",
        "redis",
    ];
    assert_eq!(
        default_backend_registry().ids().collect::<Vec<_>>(),
        characterized
    );
    for backend_id in characterized {
        assert_golden(backend_id, &render_backend(backend_id));
    }
}

#[test]
fn full_baseline_lab_matches_its_golden_contract() {
    let spec = full_baseline_lab();
    let lock = resolve_lock(&spec, default_catalog()).expect("full baseline lock");
    let rendered = render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("full render");
    for workload in &rendered.stateful_sets {
        let one = RenderedComponent {
            stateful_sets: vec![workload.clone()],
            ..RenderedComponent::default()
        };
        assert_component_security(&one);
    }
    for workload in &rendered.deployments {
        let one = RenderedComponent {
            deployments: vec![workload.clone()],
            ..RenderedComponent::default()
        };
        assert_component_security(&one);
    }

    let lab = ProofstormLab::new(
        "golden-lab",
        ProofstormLabSpec {
            workspace_id: "workspace-golden".into(),
            instance_id: "instance-golden".into(),
            instance_key: INSTANCE_KEY.into(),
            revision_digest: REVISION_DIGEST.into(),
            lock,
            lab: spec,
        },
    );
    let action = ProofstormLabAction::new(
        "golden-native-exec",
        ProofstormLabActionSpec {
            lab_name: "golden-lab".into(),
            workspace_id: "workspace-golden".into(),
            instance_id: "instance-golden".into(),
            instance_key: INSTANCE_KEY.into(),
            experiment_id: "experiment-golden".into(),
            lease_id: "lease-golden".into(),
            principal_id: "principal-golden".into(),
            sequence: 1,
            operation_id: "operation-golden".into(),
            request_digest: "sha256:golden-native-exec".into(),
            capability: Capability::ComponentForensics,
            accepted_at_unix: 1,
            action: LabAction::ComponentForensics(ComponentForensicsAction {
                component: "chain-a".into(),
                target_component: "chain-b".into(),
                script: "bitcoin-cli getblockchaininfo".into(),
                timeout_seconds: 30,
            }),
        },
    );
    let native_exec = render_lab_action_job(&action, &lab).expect("cross-target native exec");
    let native_json = serde_json::to_value(&native_exec).expect("native exec JSON");
    let env = native_json
        .pointer("/spec/template/spec/containers/0/env")
        .and_then(Value::as_array)
        .expect("native exec environment");
    let env = env
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().expect("environment name"),
                entry["value"].as_str().unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(env["PROOFSTORM_EXEC_COMPONENT"], "chain-a");
    assert_eq!(env["PROOFSTORM_TARGET_COMPONENT"], "chain-b");
    assert_eq!(env["PROOFSTORM_TARGET_HOST"], "chain-b");
    assert_eq!(env["BITCOIN_RPC_PORT"], "18443");
    assert_eq!(
        native_json["spec"]["template"]["spec"]["automountServiceAccountToken"],
        json!(false)
    );

    let spine = render_security_spine(INSTANCE_KEY);
    assert_golden(
        "full-baseline-lab",
        &json!({
            "plans": &rendered.plans,
            "inventory": rendered.inventory(),
            "resources": {
                "configMaps": &rendered.config_maps,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            },
            "securitySpine": {
                "namespace": spine.namespace,
                "quota": spine.quota,
                "limits": spine.limits,
                "defaultDeny": spine.default_deny,
                "serviceAccount": spine.service_account,
                "role": spine.role,
                "roleBinding": spine.role_binding,
            },
            "crossTargetNativeExec": native_exec,
        }),
    );
}

#[test]
fn cdk_cln_lab_matches_its_golden_contract() {
    let spec = cdk_cln_lab();
    let lock = resolve_lock(&spec, default_catalog()).expect("CDK+CLN lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("CDK+CLN full render");
    assert_golden(
        "cdk-cln-lab",
        &json!({
            "plans": &rendered.plans,
            "inventory": rendered.inventory(),
            "resources": {
                "configMaps": &rendered.config_maps,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            },
        }),
    );
}

#[test]
fn nutshell_cln_lab_uses_restricted_runtime_rune_contract() {
    let spec = nutshell_cln_lab();
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell+CLN lock");
    let rendered =
        render_lab(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell+CLN full render");
    let mint_config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .and_then(|config| config.data.as_ref())
        .expect("Nutshell+CLN configuration");
    assert_eq!(mint_config["MINT_BACKEND_BOLT11_SAT"], "CLNRestWallet");
    assert_eq!(mint_config["MINT_CLNREST_URL"], "http://mint-cln:3010");
    assert_eq!(
        mint_config["MINT_CLNREST_RUNE"],
        "/app/data/.proofstorm/cln.rune"
    );
    assert!(!mint_config.contains_key("MINT_LND_REST_MACAROON"));
    let mint = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .expect("Nutshell deployment");
    let mint = serde_json::to_value(mint).expect("Nutshell deployment JSON");
    let command = mint
        .pointer("/spec/template/spec/containers/0/command/2")
        .and_then(Value::as_str)
        .expect("Nutshell CLN bootstrap command");
    for method in [
        "listfunds",
        "invoice",
        "pay",
        "listinvoices",
        "listpays",
        "waitanyinvoice",
    ] {
        assert!(command.contains(&format!("method={method}")));
    }
    for forbidden_method in ["createrune", "withdraw", "stop"] {
        assert!(!command.contains(&format!("method={forbidden_method}")));
    }
    assert_golden(
        "nutshell-cln-lab",
        &json!({
            "plans": &rendered.plans,
            "inventory": rendered.inventory(),
            "resources": {
                "configMaps": &rendered.config_maps,
                "services": &rendered.services,
                "statefulSets": &rendered.stateful_sets,
                "deployments": &rendered.deployments,
                "persistentVolumeClaims": &rendered.persistent_volume_claims,
                "networkPolicies": &rendered.network_policies,
            },
        }),
    );
}
