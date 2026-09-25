use std::{collections::BTreeMap, fs, path::PathBuf};

use proofstorm_core::{
    API_VERSION, AuthenticationProtocol, BitcoinNetwork, Capability, CatalogPlatform,
    CatalogResponse, CellPolicy, CellSpec, ComponentKind, ComponentSpec, ControlClass,
    DatabaseRole, DependencyBinding, LinkKind, LinkSpec, PaymentMethod, catalog_for_platform,
    default_backend_registry, resolve_lock,
};
use proofstorm_kube::{
    CellAction, ComponentForensicsAction, ProofstormCell, ProofstormCellAction,
    ProofstormCellActionSpec, ProofstormCellSpec, RenderedComponent, compile_component_plans,
    render_bitcoin_component, render_cdk_component, render_cell, render_cell_action_job,
    render_cln_component, render_keycloak_component, render_lnd_component,
    render_nutshell_mint_component, render_postgres_component, render_redis_component,
    render_security_spine, render_wallet_component, render_workspace_component,
};
use serde_json::{Value, json};

const INSTANCE_KEY: &str = "i-golden-b2";
const REVISION_DIGEST: &str = "sha256:b2-golden-revision";

// Shared snapshots have a fixed platform; the backend matrix below separately
// checks all platform-specific images without depending on the test host.
fn default_catalog() -> &'static CatalogResponse {
    static CATALOG: std::sync::LazyLock<CatalogResponse> =
        std::sync::LazyLock::new(|| catalog_for_platform(CatalogPlatform::LinuxArm64));
    &CATALOG
}

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
            "bitcoin-core" => "bitcoin-core/31/v1",
            "lnd" => "lnd/0.20/v1",
            "cln" => "cln/26.06/v1",
            "cdk" => "cdk-mintd/0.18/v1",
            "nutshell" => "nutshell-mint/0.20/v1",
            "postgresql" => "postgresql/17/v1",
            "redis" => "redis/8.10/v1",
            "keycloak" => "keycloak/25/v1",
            "nutshell-wallet" => "nutshell-wallet/0.20/v1",
            "cdk-cli-wallet" => "cdk-cli-wallet/0.18/v1",
            "cocod-wallet" => "cocod-wallet/0.0.17/v1",
            "workspace" => "workspace/0.1/v1",
            _ => panic!("unknown test implementation {implementation:?}"),
        }
        .into(),
        control,
        config: BTreeMap::new(),
    }
}

fn cell(name: &str, components: Vec<ComponentSpec>, links: Vec<LinkSpec>) -> CellSpec {
    CellSpec {
        api_version: API_VERSION.into(),
        name: name.into(),
        components,
        links,
        policy: CellPolicy::default(),
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
            database: None,
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
            database: None,
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
fn backend_cell(backend_id: &str) -> (CellSpec, &'static str) {
    match backend_id {
        "ldk-server" | "cdk-ldk-server-processor" => (
            serde_json::from_str(include_str!("../../../examples/ldk-server-cell.json")).unwrap(),
            if backend_id == "ldk-server" {
                "ldk"
            } else {
                "processor"
            },
        ),
        "bitcoin-core" => (
            cell(
                "golden-bitcoin",
                vec![component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Cell,
                )],
                vec![],
            ),
            "chain",
        ),
        "lnd" | "cln" => (
            cell(
                &format!("golden-{backend_id}"),
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Cell,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        backend_id,
                        ControlClass::Cell,
                    ),
                ],
                vec![chain_link("lightning", "chain")],
            ),
            "lightning",
        ),
        "cdk" => (
            cell(
                "golden-cdk",
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Cell,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        "lnd",
                        ControlClass::Cell,
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
        "cdk-ldk" => cdk_embedded_cell(
            "golden-cdk-ldk",
            &[("embedded_lightning", "ldk-node")],
            false,
        ),
        "cdk-bdk" => cdk_embedded_cell("golden-cdk-bdk", &[("embedded_onchain", "bdk")], false),
        "cdk-lnd-bdk" => {
            cdk_embedded_cell("golden-cdk-lnd-bdk", &[("embedded_onchain", "bdk")], true)
        }
        "nutshell" => (
            cell(
                "golden-nutshell",
                vec![
                    component(
                        "chain",
                        ComponentKind::Bitcoin,
                        "bitcoin-core",
                        ControlClass::Cell,
                    ),
                    component(
                        "lightning",
                        ComponentKind::Lightning,
                        "lnd",
                        ControlClass::Cell,
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
            cell(
                "golden-wallet",
                vec![component(
                    "wallet",
                    ComponentKind::Wallet,
                    backend_id,
                    ControlClass::Cell,
                )],
                vec![],
            ),
            "wallet",
        ),
        "postgresql" => (
            cell(
                "golden-postgresql",
                vec![component(
                    "database",
                    ComponentKind::Database,
                    "postgresql",
                    ControlClass::Cell,
                )],
                vec![],
            ),
            "database",
        ),
        "redis" => (
            cell(
                "golden-redis",
                vec![component(
                    "cache",
                    ComponentKind::Database,
                    "redis",
                    ControlClass::Cell,
                )],
                vec![],
            ),
            "cache",
        ),
        "keycloak" => (
            cell(
                "golden-keycloak",
                vec![
                    component(
                        "database",
                        ComponentKind::Database,
                        "postgresql",
                        ControlClass::Cell,
                    ),
                    component(
                        "identity",
                        ComponentKind::IdentityProvider,
                        "keycloak",
                        ControlClass::Cell,
                    ),
                ],
                vec![database_link("identity", "database")],
            ),
            "identity",
        ),
        "workspace" => (
            cell(
                "golden-workspace",
                vec![component(
                    "workspace",
                    ComponentKind::Workspace,
                    "workspace",
                    ControlClass::Workspace,
                )],
                vec![],
            ),
            "workspace",
        ),
        _ => panic!("uncharacterized backend {backend_id}"),
    }
}

/// One CDK mint with embedded backends selected by configuration, optionally
/// alongside a linked LND node.
fn cdk_embedded_cell(
    name: &str,
    config: &[(&str, &str)],
    with_lnd: bool,
) -> (CellSpec, &'static str) {
    let mut mint = component("mint", ComponentKind::Mint, "cdk", ControlClass::Target);
    for (field, value) in config {
        mint.config.insert((*field).into(), json!(value));
    }
    let mut components = vec![
        component(
            "chain",
            ComponentKind::Bitcoin,
            "bitcoin-core",
            ControlClass::Cell,
        ),
        mint,
    ];
    let mut links = vec![chain_link("mint", "chain")];
    if with_lnd {
        components.push(component(
            "lightning",
            ComponentKind::Lightning,
            "lnd",
            ControlClass::Cell,
        ));
        links.push(chain_link("lightning", "chain"));
        links.push(lightning_link("mint", "lightning"));
    }
    (cell(name, components, links), "mint")
}

const CDK_EMBEDDED_SCENARIOS: [&str; 3] = ["cdk-bdk", "cdk-ldk", "cdk-lnd-bdk"];

fn render_backend(backend_id: &str) -> Value {
    render_backend_with_catalog(backend_id, default_catalog())
}

fn render_backend_with_catalog(backend_id: &str, catalog: &CatalogResponse) -> Value {
    let (cell, component_id) = backend_cell(backend_id);
    let lock = resolve_lock(&cell, catalog).expect("backend lock");
    let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &cell, &lock)
        .expect("backend plans");
    let plan = plans
        .iter()
        .find(|plan| plan.component_id == component_id)
        .expect("target plan");
    let rendered = match backend_id {
        "bitcoin-core" => render_bitcoin_component(plan),
        "lnd" => render_lnd_component(plan),
        "cln" => render_cln_component(plan),
        "ldk-server" => proofstorm_kube::render_ldk_server_component(plan),
        "cdk-ldk-server-processor" => proofstorm_kube::render_ldk_server_processor_component(plan),
        "cdk" | "cdk-ldk" | "cdk-bdk" | "cdk-lnd-bdk" => render_cdk_component(plan),
        "nutshell" => render_nutshell_mint_component(plan),
        "nutshell-wallet" => render_wallet_component(plan),
        "cdk-cli-wallet" => proofstorm_kube::render_cdk_wallet_component(plan),
        "cocod-wallet" => proofstorm_kube::render_cocod_wallet_component(plan),
        "postgresql" => render_postgres_component(plan),
        "redis" => render_redis_component(plan),
        "keycloak" => render_keycloak_component(plan),
        "workspace" => render_workspace_component(plan),
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
        assert_eq!(pod["enableServiceLinks"], json!(false));
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

fn full_baseline_cell() -> CellSpec {
    cell(
        "golden-full-baseline",
        vec![
            component(
                "chain-a",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "chain-b",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component("lnd", ComponentKind::Lightning, "lnd", ControlClass::Cell),
            component("cln", ComponentKind::Lightning, "cln", ControlClass::Cell),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
            component(
                "wallet",
                ComponentKind::Wallet,
                "nutshell-wallet",
                ControlClass::Cell,
            ),
            component(
                "workspace",
                ComponentKind::Workspace,
                "workspace",
                ControlClass::Workspace,
            ),
        ],
        vec![
            chain_link("lnd", "chain-a"),
            chain_link("cln", "chain-a"),
            lightning_link("mint", "lnd"),
        ],
    )
}

fn cdk_cln_cell() -> CellSpec {
    cell(
        "golden-cdk-cln",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "mint-cln",
                ComponentKind::Lightning,
                "cln",
                ControlClass::Cell,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ],
        vec![
            chain_link("mint-cln", "chain"),
            lightning_link("mint", "mint-cln"),
        ],
    )
}

fn nutshell_cln_cell() -> CellSpec {
    cell(
        "golden-nutshell-cln",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "mint-cln",
                ComponentKind::Lightning,
                "cln",
                ControlClass::Cell,
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

fn assert_ensure_database_credential(container: &k8s_openapi::api::core::v1::Container) {
    let env = container.env.as_ref().expect("owner password environment");
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].name, "PROOFSTORM_POSTGRES_PASSWORD");
    let reference = env[0]
        .value_from
        .as_ref()
        .and_then(|source| source.secret_key_ref.as_ref())
        .expect("secret-backed owner password");
    assert_eq!(reference.key, "POSTGRES_PASSWORD");
    assert!(
        container
            .image
            .as_deref()
            .is_some_and(|image| image.contains("/postgres@sha256:"))
    );
}

fn assert_postgres_bootstrap_env(container: &Value) {
    let environment = container["env"]
        .as_array()
        .expect("CDK container environment");
    let position = |name: &str| {
        environment
            .iter()
            .position(|entry| entry["name"] == name)
            .expect("PostgreSQL URL environment")
    };
    // The owner password precedes the URL that expands it.
    let password = position("CDK_MINTD_POSTGRES_URL_PASSWORD");
    let url = position("CDK_MINTD_POSTGRES_URL");
    assert!(password < url);
    assert_eq!(
        environment[password]["valueFrom"]["secretKeyRef"],
        json!({"name": "database-credentials", "key": "POSTGRES_PASSWORD"})
    );
    assert_eq!(
        environment[url]["value"],
        "postgresql://proofstorm:$(CDK_MINTD_POSTGRES_URL_PASSWORD)@database:5432/mint_primary"
    );
}

#[test]
fn cdk_waits_for_external_lightning_before_reading_credentials_or_opening_rpc() {
    for (backend, port) in [("lnd", "10009"), ("cln", "9735")] {
        for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
            let (mut spec, _) = backend_cell("cdk");
            *spec
                .components
                .iter_mut()
                .find(|component| component.id == "lightning")
                .unwrap() = component(
                "lightning",
                ComponentKind::Lightning,
                backend,
                ControlClass::Cell,
            );
            let lock = resolve_lock(&spec, &catalog_for_platform(platform)).unwrap();
            let plans =
                compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
            let rendered = render_cdk_component(
                plans
                    .iter()
                    .find(|plan| plan.component_id == "mint")
                    .unwrap(),
            )
            .unwrap();
            let pod = rendered.deployments[0]
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap();
            let init = pod.init_containers.as_ref().unwrap();
            let wait = init
                .iter()
                .position(|container| container.name == "wait-for-lightning")
                .unwrap();
            let initialize = init
                .iter()
                .position(|container| container.name == "initialize-config")
                .unwrap();
            assert!(wait < initialize);
            assert_eq!(
                &init[wait].command.as_ref().unwrap()[4..],
                ["lightning", port]
            );
            assert!(init[wait].env.is_none());
            assert!(init[wait].volume_mounts.is_none());
        }
    }
}

#[test]
fn keycloak_waits_for_its_actual_database_service_without_database_credentials() {
    for platform in [CatalogPlatform::LinuxAmd64, CatalogPlatform::LinuxArm64] {
        let (mut spec, _) = backend_cell("keycloak");
        spec.components
            .iter_mut()
            .find(|c| c.id == "database")
            .unwrap()
            .id = "identity-storage".into();
        spec.links
            .iter_mut()
            .find(|link| link.kind == LinkKind::DatabaseBackend)
            .unwrap()
            .to = "identity-storage".into();
        let lock = resolve_lock(&spec, &catalog_for_platform(platform)).unwrap();
        let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
        let plan = plans.iter().find(|p| p.component_id == "identity").unwrap();
        let rendered = render_keycloak_component(plan).unwrap();
        let pod = rendered.deployments[0]
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap();
        let init = pod.init_containers.as_ref().unwrap();
        assert_eq!(init.len(), 1);
        assert_eq!(init[0].name, "ensure-database");
        assert_eq!(
            &init[0].command.as_ref().unwrap()[4..],
            ["identity-storage", "5432", "identity_primary"]
        );
        assert_ensure_database_credential(&init[0]);
        assert!(init[0].volume_mounts.is_none());
        let database = plans
            .iter()
            .find(|p| p.component_id == "identity-storage")
            .unwrap();
        let rendered = render_postgres_component(database).unwrap();
        let probe = rendered.stateful_sets[0]
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .readiness_probe
            .as_ref()
            .unwrap()
            .exec
            .as_ref()
            .unwrap()
            .command
            .as_ref()
            .unwrap();
        assert_eq!(&probe[..5], ["pg_isready", "-h", "127.0.0.1", "-p", "5432"]);
    }
}

/// The JVM augments and imports its realm at startup and does not fit the
/// namespace default, so the limit must stay above it or the pod is OOM killed
/// before it can ever pass its readiness probe.
#[test]
fn keycloak_declares_a_memory_limit_above_the_namespace_container_default() {
    let mebibytes = |quantity: &k8s_openapi::apimachinery::pkg::api::resource::Quantity| {
        let value = &quantity.0;
        let (amount, scale) = value.split_at(value.len() - 2);
        amount.parse::<u64>().unwrap()
            * match scale {
                "Mi" => 1,
                "Gi" => 1024,
                other => panic!("unexpected quantity scale {other:?} in {value:?}"),
            }
    };
    let default = render_security_spine(INSTANCE_KEY)
        .limits
        .spec
        .unwrap()
        .limits[0]
        .default
        .clone()
        .unwrap();
    let (spec, _) = backend_cell("keycloak");
    let lock = resolve_lock(&spec, default_catalog()).unwrap();
    let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
    let plan = plans.iter().find(|p| p.component_id == "identity").unwrap();
    let rendered = render_keycloak_component(plan).unwrap();
    let resources = rendered.deployments[0]
        .spec
        .as_ref()
        .unwrap()
        .template
        .spec
        .as_ref()
        .unwrap()
        .containers[0]
        .resources
        .as_ref()
        .expect("keycloak declares its own resources");
    let limits = resources.limits.as_ref().unwrap();
    let requests = resources.requests.as_ref().unwrap();
    assert!(mebibytes(&limits["memory"]) > mebibytes(&default["memory"]));
    assert!(mebibytes(&requests["memory"]) <= mebibytes(&limits["memory"]));
}

#[test]
fn every_cdk_backend_waits_for_its_linked_postgres_before_initialization() {
    for backend in ["cdk", "cdk-ldk", "cdk-bdk", "ldk-server"] {
        for postgres in [false, true] {
            let (mut spec, _) = backend_cell(backend);
            if postgres {
                spec.components.push(component(
                    "mint-storage",
                    ComponentKind::Database,
                    "postgresql",
                    ControlClass::Cell,
                ));
                spec.links.push(database_link("mint", "mint-storage"));
            }
            for platform in [CatalogPlatform::LinuxArm64, CatalogPlatform::LinuxAmd64] {
                let catalog = catalog_for_platform(platform);
                let lock = resolve_lock(&spec, &catalog).unwrap();
                let plans =
                    compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
                let plan = plans.iter().find(|p| p.component_id == "mint").unwrap();
                let rendered = render_cdk_component(plan).unwrap();
                let pod = rendered.deployments[0]
                    .spec
                    .as_ref()
                    .unwrap()
                    .template
                    .spec
                    .as_ref()
                    .unwrap();
                let init = pod.init_containers.as_ref().unwrap();
                let wait = init.iter().position(|c| c.name == "ensure-database");
                if postgres {
                    let wait = wait.expect("the mint database must exist before CDK config access");
                    let initialize = init
                        .iter()
                        .position(|c| c.name == "initialize-config")
                        .unwrap();
                    assert!(wait < initialize);
                    let command = init[wait].command.as_ref().unwrap();
                    assert_eq!(&command[4..], ["mint-storage", "5432", "mint_primary"]);
                    // Creating the database needs only the owner password.
                    assert_ensure_database_credential(&init[wait]);
                    assert!(init[wait].volume_mounts.is_none());
                    let database = plans
                        .iter()
                        .find(|p| p.component_id == "mint-storage")
                        .unwrap();
                    let rendered = render_postgres_component(database).unwrap();
                    let pod = rendered.stateful_sets[0]
                        .spec
                        .as_ref()
                        .unwrap()
                        .template
                        .spec
                        .as_ref()
                        .unwrap();
                    let probe = pod.containers[0]
                        .readiness_probe
                        .as_ref()
                        .unwrap()
                        .exec
                        .as_ref()
                        .unwrap()
                        .command
                        .as_ref()
                        .unwrap();
                    assert_eq!(&probe[..5], ["pg_isready", "-h", "127.0.0.1", "-p", "5432"]);
                } else {
                    assert!(wait.is_none(), "SQLite must not wait for PostgreSQL");
                }
            }
        }
    }
}

#[test]
fn cdk_postgres_binding_materializes_secret_backed_native_configuration() {
    let spec = cell(
        "golden-cdk-postgres",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("mint", "database"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("PostgreSQL cell lock");
    let rendered =
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("PostgreSQL cell render");
    let secret = rendered
        .secrets
        .iter()
        .find(|secret| secret.metadata.name.as_deref() == Some("database-credentials"))
        .expect("database credential template");
    // Only the maintenance database; the mint creates its own.
    assert_eq!(
        secret.string_data.as_ref().unwrap()["POSTGRES_DB"],
        "postgres"
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
    let initialize = mint["spec"]["template"]["spec"]["initContainers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|container| container["name"] == "initialize-config")
        .expect("CDK configuration initializer");
    assert_postgres_bootstrap_env(&mint["spec"]["template"]["spec"]["containers"][0]);
    assert_postgres_bootstrap_env(initialize);
    assert_golden(
        "cdk-postgres-cell",
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
fn nutshell_probes_preserve_strict_application_limits() {
    for trust_proxy in [false, true] {
        let (mut spec, mint_id) = backend_cell("nutshell");
        let mint = spec
            .components
            .iter_mut()
            .find(|c| c.id == mint_id)
            .unwrap();
        for key in [
            "global_rate_limit_per_minute",
            "transaction_rate_limit_per_minute",
            "auth_rate_limit_per_minute",
        ] {
            mint.config.insert(key.into(), json!(1));
        }
        mint.config.insert("rate_limit".into(), json!(true));
        mint.config
            .insert("rate_limit_proxy_trust".into(), json!(trust_proxy));
        let lock = resolve_lock(&spec, default_catalog()).expect("strict quota lock");
        let rendered = render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock)
            .expect("strict quota rendering");
        let config = rendered
            .config_maps
            .iter()
            .find(|c| c.metadata.name.as_deref() == Some("mint-config"))
            .unwrap()
            .data
            .as_ref()
            .unwrap();
        assert_eq!(config["MINT_RATE_LIMIT"], "TRUE");
        assert_eq!(
            config["MINT_RATE_LIMIT_PROXY_TRUST"],
            if trust_proxy { "TRUE" } else { "FALSE" }
        );
        for key in [
            "MINT_GLOBAL_RATE_LIMIT_PER_MINUTE",
            "MINT_TRANSACTION_RATE_LIMIT_PER_MINUTE",
            "MINT_AUTH_RATE_LIMIT_PER_MINUTE",
        ] {
            assert_eq!(config[key], "1");
        }
        let plan = rendered
            .plans
            .iter()
            .find(|p| p.component_id == mint_id)
            .unwrap();
        assert_eq!(
            plan.protocol_probe,
            Some(proofstorm_core::ProtocolProbePlan::Tcp { port: 3338 })
        );
        let deployment = rendered
            .deployments
            .iter()
            .find(|d| d.metadata.name.as_deref() == Some(mint_id))
            .unwrap();
        let deployment = serde_json::to_value(deployment).unwrap();
        let readiness = &deployment["spec"]["template"]["spec"]["containers"][0]["readinessProbe"];
        assert_eq!(
            readiness["exec"]["command"][3],
            "http://127.0.0.1:3338/v1/info"
        );
        assert!(readiness.get("httpGet").is_none());
    }
}

#[test]
fn nutshell_postgres_binding_keeps_database_and_mint_secrets_out_of_public_config() {
    let spec = cell(
        "golden-nutshell-postgres",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
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
    let rendered = render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock)
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
        entry["name"] == "MINT_DATABASE_PASSWORD"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "database-credentials"
            && entry["valueFrom"]["secretKeyRef"]["key"] == "POSTGRES_PASSWORD"
    }));
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_DATABASE"
            && entry["value"]
                == "postgresql://proofstorm:$(MINT_DATABASE_PASSWORD)@database:5432/mint_primary"
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
    let spec = cell(
        "golden-nutshell-oidc",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
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
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell OIDC render");
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
        "nutshell-oidc-cell",
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
    let mut spec = cell(
        "golden-nutshell-keycloak",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "identity-db",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
            ),
            component(
                "identity",
                ComponentKind::IdentityProvider,
                "keycloak",
                ControlClass::Cell,
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
    // Exercise the retained renderer with an explicit synthetic compatibility
    // declaration. Shipped Nutshell releases cannot issue blind-auth proofs.
    spec.components
        .iter_mut()
        .find(|component| component.id == "mint")
        .unwrap()
        .version = Some("0.20.3".into());
    let mut catalog = default_catalog().clone();
    assert!(resolve_lock(&spec, &catalog).is_err());
    let nutshell = catalog
        .entries
        .iter_mut()
        .find(|entry| entry.id == "nutshell" && entry.version == "0.20.3")
        .unwrap();
    nutshell
        .compatible_dependencies
        .push(proofstorm_core::CatalogDependencySupport {
            link_kind: LinkKind::AuthenticationBackend,
            implementation: "keycloak".into(),
            versions: ["25.0.6".into()].into(),
        });
    // Authentication links need a declared mode, not only a dependency.
    nutshell
        .support_matrix
        .authentication
        .insert(proofstorm_core::AuthenticationMode::Nut22Blind);
    let lock = resolve_lock(&spec, &catalog).expect("synthetic Nutshell Keycloak lock");
    let rendered =
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell Keycloak render");
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
    let initializers: Vec<_> = mint["spec"]["template"]["spec"]["initContainers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|container| container["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        initializers,
        ["proofstorm-driver", "wait-for-lightning", "wait-for-oidc"]
    );
    assert_eq!(
        mint.pointer("/spec/template/spec/containers/0/command"),
        Some(&json!(["mint"]))
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
    let mint_probe = rendered
        .plans
        .iter()
        .find(|plan| plan.component_id == "mint")
        .expect("Nutshell protocol plan");
    assert_eq!(
        mint_probe.protocol_probe,
        Some(proofstorm_core::ProtocolProbePlan::Tcp { port: 3338 }),
        "the remote probe must verify reachability without making an HTTP request"
    );
    assert_golden(
        "nutshell-keycloak-cell",
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
    let spec = cell(
        "golden-nutshell-redis",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
            ),
            component(
                "cache",
                ComponentKind::Database,
                "redis",
                ControlClass::Cell,
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
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell Redis render");
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
        entry["name"] == "MINT_DATABASE_PASSWORD"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "database-credentials"
    }));
    assert!(env.iter().any(|entry| {
        entry["name"] == "MINT_REDIS_CACHE_URL"
            && entry["valueFrom"]["secretKeyRef"]["name"] == "cache-credentials"
            && entry["valueFrom"]["secretKeyRef"]["key"] == "REDIS_URL"
    }));
    assert_golden(
        "nutshell-redis-cell",
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

#[test]
fn management_is_authenticated_loopback_with_separate_certificate_projections() {
    for backend in ["cdk", "cdk-ldk", "cdk-bdk", "nutshell"] {
        let rendered = render_backend(backend);
        let resources = &rendered["resources"];
        let pod = &resources["deployments"][0]["spec"]["template"]["spec"];
        assert_ne!(pod["hostNetwork"], true);
        for service in resources["services"].as_array().unwrap() {
            assert!(
                service["spec"]["ports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p["port"] != 8086 && p["targetPort"] != 8086)
            );
        }
        for role in ["client", "server"] {
            let name = format!("management-{role}");
            let volume = pod["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["name"] == name)
                .unwrap();
            assert_eq!(volume["secret"]["defaultMode"], 288);
            let keys: Vec<_> = volume["secret"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["key"].as_str().unwrap())
                .collect();
            assert_eq!(
                keys,
                ["ca.pem", &format!("{role}.pem"), &format!("{role}.key")]
            );
            let mount = pod["containers"][0]["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["name"] == name)
                .unwrap();
            assert_eq!(mount["readOnly"], true);
        }
        let config = serde_json::to_string(&resources["configMaps"]).unwrap();
        assert!(config.contains("127.0.0.1"));
        assert!(config.contains("/management-server/tls"));
        let probe = pod["containers"][0]["readinessProbe"].to_string();
        if backend == "nutshell" {
            assert!(probe.contains("/opt/proofstorm/driver"));
            assert!(probe.contains("nutshell"));
        } else {
            assert!(probe.contains("/management-client"));
        }
        assert!(
            !resources["secrets"]
                .to_string()
                .contains("BEGIN PRIVATE KEY")
        );
    }
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
fn grpc_mint_uses_the_selected_processor_and_only_its_client_identity() {
    let (spec, _) = backend_cell("ldk-server");
    for (id, service, port) in [("ldk", "rpc", 3536), ("processor", "grpc", 50051)] {
        let component = spec
            .components
            .iter()
            .find(|component| component.id == id)
            .unwrap();
        assert_eq!(proofstorm_kube::component_ports(component)[service], port);
    }
    let lock = resolve_lock(&spec, default_catalog()).unwrap();
    let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
    let mint = plans
        .iter()
        .find(|plan| plan.component_id == "mint")
        .unwrap();
    assert!(
        mint.execution_context
            .mounts
            .iter()
            .all(|mount| mount.name != "lnd" && mount.name != "cln")
    );
    let rendered = render_cdk_component(mint).unwrap();
    assert_component_security(&rendered);
    let snapshot = component_snapshot(mint, &rendered);
    let config = &rendered.config_maps[0].data.as_ref().unwrap()["config.toml"];
    for required in [
        "backend = \"grpcprocessor\"",
        "address = \"processor\"",
        "allow_insecure = false",
        "supported_units = [\"sat\"]",
    ] {
        assert!(config.contains(required), "missing {required}");
    }
    assert!(!config.contains("[lnd]") && !config.contains("[cln]"));
    let pod = &snapshot["resources"]["deployments"][0]["spec"]["template"]["spec"];
    let volume = pod["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|volume| volume["name"] == "payment-processor")
        .unwrap();
    assert_eq!(volume["secret"]["secretName"], "processor-payment-tls");
    let keys: Vec<_> = volume["secret"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["ca.pem", "client.pem", "client.key"]);
    let initializers = pod["initContainers"].as_array().unwrap();
    assert!(initializers[0]["name"].as_str().unwrap().contains("driver"));
    assert!(
        initializers
            .iter()
            .any(|init| init["name"] == "wait-for-payment-processor")
    );
    assert_golden("cdk-grpc-processor", &snapshot);

    // A forged compiled plan cannot silently point the two advertised methods
    // at different services, even if it bypasses authored-cell validation.
    let mut invalid = mint.clone();
    invalid
        .relevant_links
        .iter_mut()
        .find(|link| link.id == "mint-bolt12")
        .unwrap()
        .to = "payer".into();
    assert!(render_cdk_component(&invalid).is_err());
}

#[test]
fn every_registered_backend_matches_its_golden_contract() {
    // Keep shared snapshot updates sequential when UPDATE_GOLDENS is enabled.
    assert_backend_goldens(CatalogPlatform::LinuxArm64);
    assert_backend_goldens(CatalogPlatform::LinuxAmd64);
}

fn assert_backend_goldens(platform: CatalogPlatform) {
    let characterized = [
        "bitcoin-core",
        "cdk",
        "cdk-cli-wallet",
        "cdk-ldk-server-processor",
        "cln",
        "cocod-wallet",
        "keycloak",
        "ldk-server",
        "lnd",
        "nutshell",
        "nutshell-wallet",
        "postgresql",
        "redis",
        "workspace",
    ];
    assert_eq!(
        default_backend_registry().ids().collect::<Vec<_>>(),
        characterized
    );
    let catalog = catalog_for_platform(platform);
    // Embedded CDK backends are configuration of the one CDK backend.
    for backend_id in characterized.into_iter().chain(CDK_EMBEDDED_SCENARIOS) {
        // These packaged components have architecture-specific images. Every other
        // backend must match the same full contract on both platforms.
        let golden_name = match (platform, backend_id) {
            (
                CatalogPlatform::LinuxAmd64,
                "cdk-cli-wallet"
                | "cocod-wallet"
                | "nutshell"
                | "nutshell-wallet"
                | "ldk-server"
                | "cdk-ldk-server-processor",
            ) => {
                format!("linux-amd64/{backend_id}")
            }
            _ => backend_id.to_owned(),
        };
        assert_golden(
            &golden_name,
            &render_backend_with_catalog(backend_id, &catalog),
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the complete baseline is one golden rendering contract"
)]
fn full_baseline_cell_matches_its_golden_contract() {
    let spec = full_baseline_cell();
    let lock = resolve_lock(&spec, default_catalog()).expect("full baseline lock");
    let rendered = render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("full render");
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

    let cell = ProofstormCell::new(
        "golden-cell",
        ProofstormCellSpec {
            workspace_id: "workspace-golden".into(),
            instance_id: "instance-golden".into(),
            instance_key: INSTANCE_KEY.into(),
            revision_digest: REVISION_DIGEST.into(),
            lock,
            cell: spec,
        },
    );
    let action = ProofstormCellAction::new(
        "golden-native-exec",
        ProofstormCellActionSpec {
            access_scope: None,
            cell_name: "golden-cell".into(),
            workspace_id: "workspace-golden".into(),
            instance_id: "instance-golden".into(),
            instance_key: INSTANCE_KEY.into(),
            experiment_id: "experiment-golden".into(),
            session_id: "session-golden".into(),
            principal_id: "principal-golden".into(),
            sequence: 1,
            operation_id: "operation-golden".into(),
            request_digest: "sha256:golden-native-exec".into(),
            capability: Capability::ComponentForensics,
            accepted_at_unix: 1,
            action: CellAction::ComponentForensics(ComponentForensicsAction {
                component: "chain-a".into(),
                target_component: "chain-b".into(),
                script: "bitcoin-cli getblockchaininfo".into(),
                timeout_seconds: 30,
            }),
        },
    );
    let native_exec = render_cell_action_job(&action, &cell).expect("cross-target native exec");
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
        "full-baseline-cell",
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
            },
            "crossTargetNativeExec": native_exec,
        }),
    );
}

#[test]
fn cdk_cln_cell_matches_its_golden_contract() {
    let spec = cdk_cln_cell();
    let lock = resolve_lock(&spec, default_catalog()).expect("CDK+CLN lock");
    let rendered =
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("CDK+CLN full render");
    assert_golden(
        "cdk-cln-cell",
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
fn nutshell_021_contract_selects_xpay_without_changing_the_020_contract() {
    let catalog = default_catalog();
    let mut spec = nutshell_cln_cell();
    spec.components
        .iter_mut()
        .find(|component| component.id == "mint")
        .unwrap()
        .version = Some("0.21.0".into());
    let lock = resolve_lock(&spec, catalog).unwrap();
    let rendered = render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).unwrap();
    let config = rendered
        .config_maps
        .iter()
        .find(|config| config.metadata.name.as_deref() == Some("mint-config"))
        .unwrap()
        .data
        .as_ref()
        .unwrap();
    assert_eq!(config["PROOFSTORM_NUTSHELL_VERSION"], "0.21.0");
    assert_eq!(
        config["MINT_CLNREST_RUNE"],
        "/app/data/.proofstorm/cln-xpay.rune"
    );
    let mint = rendered
        .deployments
        .iter()
        .find(|deployment| deployment.metadata.name.as_deref() == Some("mint"))
        .unwrap();
    let mint = serde_json::to_value(mint).unwrap();
    assert_eq!(
        mint.pointer("/spec/template/spec/containers/0/command/2")
            .unwrap(),
        "/opt/proofstorm/driver cln-mint-rune xpay; exec mint"
    );
}

#[test]
fn nutshell_cln_cell_uses_restricted_runtime_rune_contract() {
    let mut spec = nutshell_cln_cell();
    spec.components
        .iter_mut()
        .find(|component| component.id == "mint")
        .unwrap()
        .version = Some("0.20.3".into());
    let lock = resolve_lock(&spec, default_catalog()).expect("Nutshell+CLN lock");
    let rendered =
        render_cell(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock).expect("Nutshell+CLN full render");
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
    assert_eq!(command, "/opt/proofstorm/driver cln-mint-rune; exec mint");
    // The actual Unix RPC restriction list and private rune reuse are exercised
    // against a socket fixture in proofstorm-driver/tests/cln.rs.
    assert_golden(
        "nutshell-cln-cell",
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
fn one_postgres_server_hosts_a_database_per_linked_component() {
    let mut spec = cell(
        "golden-shared-postgres",
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
            ),
            component(
                "identity",
                ComponentKind::IdentityProvider,
                "keycloak",
                ControlClass::Cell,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ],
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("mint", "database"),
            database_link("identity", "database"),
        ],
    );
    let lock = resolve_lock(&spec, default_catalog()).expect("shared server lock");
    let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, &spec, &lock)
        .expect("shared server plans");
    let ensure = |component: &str| {
        let plan = plans
            .iter()
            .find(|plan| plan.component_id == component)
            .unwrap();
        let rendered = if component == "mint" {
            render_cdk_component(plan)
        } else {
            render_keycloak_component(plan)
        }
        .unwrap();
        let pod = rendered.deployments[0]
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .clone()
            .unwrap();
        pod.init_containers
            .unwrap()
            .into_iter()
            .find(|container| container.name == "ensure-database")
            .and_then(|container| container.command)
            .unwrap()
    };
    assert_eq!(&ensure("mint")[4..], ["database", "5432", "mint_primary"]);
    assert_eq!(
        &ensure("identity")[4..],
        ["database", "5432", "identity_primary"]
    );

    // Two bindings can never own the same database on one server.
    for link in &mut spec.links {
        if link.kind == LinkKind::DatabaseBackend {
            link.binding = Some(DependencyBinding::Database {
                role: DatabaseRole::Primary,
                database: Some("shared".into()),
            });
        }
    }
    let error = resolve_lock(&spec, default_catalog()).expect_err("duplicate database name");
    assert!(error.contains("duplicate_database_name"));
}

#[test]
fn cdk_auth_keeps_upstream_endpoint_defaults_and_follows_the_primary_engine() {
    let identity_links = || {
        vec![
            chain_link("lightning", "chain"),
            lightning_link("mint", "lightning"),
            database_link("identity", "database"),
            authentication_link("mint", "identity"),
        ]
    };
    let components = || {
        vec![
            component(
                "chain",
                ComponentKind::Bitcoin,
                "bitcoin-core",
                ControlClass::Cell,
            ),
            component(
                "lightning",
                ComponentKind::Lightning,
                "lnd",
                ControlClass::Cell,
            ),
            component(
                "database",
                ComponentKind::Database,
                "postgresql",
                ControlClass::Cell,
            ),
            component(
                "identity",
                ComponentKind::IdentityProvider,
                "keycloak",
                ControlClass::Cell,
            ),
            component("mint", ComponentKind::Mint, "cdk", ControlClass::Target),
        ]
    };
    let render_mint = |spec: &CellSpec| {
        let lock = resolve_lock(spec, default_catalog()).expect("CDK auth lock");
        let plans = compile_component_plans(INSTANCE_KEY, REVISION_DIGEST, spec, &lock)
            .expect("CDK auth plans");
        let plan = plans
            .iter()
            .find(|plan| plan.component_id == "mint")
            .unwrap();
        let rendered = render_cdk_component(plan).expect("CDK auth render");
        let config = rendered.config_maps[0].data.as_ref().unwrap()["config.toml"].clone();
        let pod =
            serde_json::to_value(&rendered.deployments[0]).unwrap()["spec"]["template"]["spec"]
                .clone();
        (config, pod)
    };
    let init_names = |pod: &Value| {
        pod["initContainers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|container| container["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    // SQLite: the auth store stays beside the mint database.
    let sqlite = cell("golden-cdk-auth", components(), identity_links());
    let (config, pod) = render_mint(&sqlite);
    for fragment in [
        "[auth]\nauth_enabled = true",
        "openid_discovery = \"http://identity:8080/realms/proofstorm/.well-known/openid-configuration\"",
        "openid_client_id = \"cashu-client\"",
        "mint_max_bat = 50",
    ] {
        assert!(config.contains(fragment), "missing {fragment:?}");
    }
    // Upstream decides which endpoints are protected.
    for absent in ["[auth_database", "get_mint_quote", "swap =", "restore ="] {
        assert!(!config.contains(absent), "unexpected {absent:?}");
    }
    assert!(init_names(&pod).contains(&"wait-for-oidc".to_owned()));

    // PostgreSQL: a separate auth database on the same server is required.
    let mut postgres = cell("golden-cdk-auth-postgres", components(), identity_links());
    postgres.links.push(database_link("mint", "database"));
    assert!(
        resolve_lock(&postgres, default_catalog())
            .unwrap_err()
            .contains("cdk_authentication_database_required")
    );
    postgres.links.push(LinkSpec {
        id: "mint-database-authentication".into(),
        kind: LinkKind::DatabaseBackend,
        from: "mint".into(),
        to: "database".into(),
        binding: Some(DependencyBinding::Database {
            role: DatabaseRole::Authentication,
            database: None,
        }),
    });
    let (config, pod) = render_mint(&postgres);
    assert!(config.contains("[auth_database.postgres]\nurl = \"env:CDK_MINTD_AUTH_POSTGRES_URL\""));
    let names = init_names(&pod);
    for name in ["ensure-database", "ensure-auth-database", "wait-for-oidc"] {
        assert!(names.contains(&name.to_owned()), "missing {name}");
    }
    let env = pod["containers"][0]["env"].as_array().unwrap();
    assert!(env.iter().any(|entry| entry["name"] == "CDK_MINTD_AUTH_POSTGRES_URL"
        && entry["value"]
            == "postgresql://proofstorm:$(CDK_MINTD_AUTH_POSTGRES_URL_PASSWORD)@database:5432/mint_authentication"));
}
