use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    core::v1::{ConfigMap, PersistentVolumeClaim, Service},
    networking::v1::NetworkPolicy,
};
use proofstorm_core::{
    ComponentKind, ComponentSpec, InventoryEntry, LabSpec, LinkKind, ResolvedLock,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{INSTANCE_LABEL, instance_namespace};

const COMPONENT_LABEL: &str = "proofstorm.dev/component";
const NETWORK_IDENTITY_LABEL: &str = "proofstorm.dev/network-identity";
const RPC_USER: &str = "proofstorm";
const RPC_PASSWORD: &str = "proofstorm-regtest-only";

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("component {component:?} has no resolved lock entry")]
    MissingLock { component: String },
    #[error("component {component:?} requires a {link:?} link")]
    MissingLink { component: String, link: LinkKind },
    #[error("component {component:?} references missing component {target:?}")]
    MissingTarget { component: String, target: String },
    #[error("adapter {adapter:?} is not installed")]
    UnsupportedAdapter { adapter: String },
    #[error("adapter rendered an invalid Kubernetes resource: {0}")]
    InvalidResource(#[from] serde_json::Error),
}

#[derive(Debug, Default)]
pub struct RenderedLab {
    pub config_maps: Vec<ConfigMap>,
    pub services: Vec<Service>,
    pub stateful_sets: Vec<StatefulSet>,
    pub deployments: Vec<Deployment>,
    pub persistent_volume_claims: Vec<PersistentVolumeClaim>,
    pub network_policies: Vec<NetworkPolicy>,
}

impl RenderedLab {
    #[must_use]
    pub fn inventory(&self) -> Vec<InventoryEntry> {
        let mut inventory = Vec::new();
        append_inventory(&mut inventory, "v1", "ConfigMap", &self.config_maps);
        append_inventory(&mut inventory, "v1", "Service", &self.services);
        append_inventory(
            &mut inventory,
            "apps/v1",
            "StatefulSet",
            &self.stateful_sets,
        );
        append_inventory(&mut inventory, "apps/v1", "Deployment", &self.deployments);
        append_inventory(
            &mut inventory,
            "v1",
            "PersistentVolumeClaim",
            &self.persistent_volume_claims,
        );
        append_inventory(
            &mut inventory,
            "networking.k8s.io/v1",
            "NetworkPolicy",
            &self.network_policies,
        );
        inventory.sort_by(|left, right| {
            (&left.api_version, &left.kind, &left.namespace, &left.name).cmp(&(
                &right.api_version,
                &right.kind,
                &right.namespace,
                &right.name,
            ))
        });
        inventory
    }
}

/// Render a resolved lab into bounded Kubernetes protocol workloads.
///
/// # Errors
///
/// Returns an error when a component is unresolved, an adapter is unsupported,
/// a required topology link is absent, or an internal resource contract is
/// invalid.
pub fn render_lab(
    instance_key: &str,
    lab: &LabSpec,
    lock: &ResolvedLock,
) -> Result<RenderedLab, AdapterError> {
    let namespace = instance_namespace(instance_key);
    let mut rendered = RenderedLab::default();
    rendered
        .network_policies
        .push(resource(action_network_policy(instance_key, &namespace))?);

    for component in &lab.components {
        rendered
            .network_policies
            .push(render_component_network_policy(
                instance_key,
                &component.id,
                &[],
            )?);
        let entry = lock
            .entries
            .iter()
            .find(|entry| entry.component_id == component.id)
            .ok_or_else(|| AdapterError::MissingLock {
                component: component.id.clone(),
            })?;
        match entry.catalog_id.as_str() {
            "bitcoin-core" => render_bitcoin(
                &mut rendered,
                instance_key,
                &namespace,
                component,
                &entry.image,
            )?,
            "lnd" => render_lnd(
                &mut rendered,
                instance_key,
                &namespace,
                lab,
                component,
                &entry.image,
            )?,
            "cln" => render_cln(
                &mut rendered,
                instance_key,
                &namespace,
                lab,
                component,
                &entry.image,
            )?,
            "cdk" => render_cdk(
                &mut rendered,
                instance_key,
                &namespace,
                lab,
                component,
                &entry.image,
            )?,
            "nutshell-wallet" => render_wallet(
                &mut rendered,
                instance_key,
                &namespace,
                component,
                &entry.image,
            )?,
            "attacker-workspace" => render_attacker(
                &mut rendered,
                instance_key,
                &namespace,
                component,
                &entry.image,
            )?,
            adapter => {
                return Err(AdapterError::UnsupportedAdapter {
                    adapter: adapter.into(),
                });
            }
        }
    }
    Ok(rendered)
}

fn render_bitcoin(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let txindex = component
        .config
        .get("txindex")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let fallback_fee = component
        .config
        .get("fallback_fee")
        .and_then(Value::as_f64)
        .unwrap_or(0.0002);
    let args = vec![
        "-regtest".to_owned(),
        "-datadir=/home/bitcoin/.bitcoin".to_owned(),
        "-server=1".to_owned(),
        "-rpcbind=0.0.0.0".to_owned(),
        "-rpcallowip=0.0.0.0/0".to_owned(),
        format!("-rpcuser={RPC_USER}"),
        format!("-rpcpassword={RPC_PASSWORD}"),
        "-rpcport=18443".to_owned(),
        format!("-txindex={}", u8::from(txindex)),
        "-zmqpubrawblock=tcp://0.0.0.0:28334".to_owned(),
        "-zmqpubrawtx=tcp://0.0.0.0:28335".to_owned(),
        format!("-fallbackfee={fallback_fee}"),
        "-debug=0".to_owned(),
    ];
    rendered.services.push(resource(service(
        instance_key,
        namespace,
        component,
        &[
            ("rpc", 18_443),
            ("p2p", 18_444),
            ("zmq-block", 28_334),
            ("zmq-tx", 28_335),
        ],
    ))?);
    rendered.stateful_sets.push(resource(stateful_set(
        instance_key,
        namespace,
        component,
        image,
        Some(vec!["bitcoind".to_owned()]),
        &args,
        "/home/bitcoin/.bitcoin",
        &json!({
            "exec": {"command": ["bitcoin-cli", "-regtest", format!("-rpcuser={RPC_USER}"), format!("-rpcpassword={RPC_PASSWORD}"), "getblockchaininfo"]}
        }),
    ))?);
    Ok(())
}

fn render_lnd(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    lab: &LabSpec,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let chain = linked_target(lab, component, LinkKind::ChainBackend)?;
    let alias = component
        .config
        .get("alias")
        .and_then(Value::as_str)
        .unwrap_or(&component.id);
    let args = vec![
        "--lnddir=/home/lnd/.lnd".to_owned(),
        "--noseedbackup".to_owned(),
        format!("--alias={alias}"),
        format!("--externalip={}", component.id),
        "--listen=0.0.0.0:9735".to_owned(),
        "--rpclisten=0.0.0.0:10009".to_owned(),
        "--restlisten=0.0.0.0:8080".to_owned(),
        "--bitcoin.active".to_owned(),
        "--bitcoin.regtest".to_owned(),
        "--bitcoin.node=bitcoind".to_owned(),
        format!("--bitcoind.rpchost={}:18443", chain.id),
        format!("--bitcoind.rpcuser={RPC_USER}"),
        format!("--bitcoind.rpcpass={RPC_PASSWORD}"),
        format!("--bitcoind.zmqpubrawblock=tcp://{}:28334", chain.id),
        format!("--bitcoind.zmqpubrawtx=tcp://{}:28335", chain.id),
        format!("--tlsextradomain={}", component.id),
        "--accept-keysend".to_owned(),
        "--debuglevel=info".to_owned(),
    ];
    rendered.services.push(resource(service(
        instance_key,
        namespace,
        component,
        &[("p2p", 9_735), ("rpc", 10_009), ("rest", 8_080)],
    ))?);
    rendered.stateful_sets.push(resource(stateful_set(
        instance_key,
        namespace,
        component,
        image,
        Some(vec!["lnd".to_owned()]),
        &args,
        "/home/lnd/.lnd",
        &json!({
            "exec": {"command": ["lncli", "--lnddir=/home/lnd/.lnd", "--network=regtest", "getinfo"]}
        }),
    ))?);
    Ok(())
}

fn render_cln(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    lab: &LabSpec,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let chain = linked_target(lab, component, LinkKind::ChainBackend)?;
    let alias = component
        .config
        .get("alias")
        .and_then(Value::as_str)
        .unwrap_or(&component.id);
    let args = vec![
        "--lightning-dir=/home/cln/.lightning".to_owned(),
        "--network=regtest".to_owned(),
        format!("--alias={alias}"),
        "--developer".to_owned(),
        "--dev-no-reconnect".to_owned(),
        "--autoconnect-seeker-peers=0".to_owned(),
        "--bind-addr=0.0.0.0:9735".to_owned(),
        format!("--announce-addr={}:9735", component.id),
        format!("--bitcoin-rpcconnect={}", chain.id),
        "--bitcoin-rpcport=18443".to_owned(),
        format!("--bitcoin-rpcuser={RPC_USER}"),
        format!("--bitcoin-rpcpassword={RPC_PASSWORD}"),
        "--bitcoin-retry-timeout=60".to_owned(),
        "--log-level=info".to_owned(),
    ];
    rendered.services.push(resource(service(
        instance_key,
        namespace,
        component,
        &[("p2p", 9_735)],
    ))?);
    rendered.stateful_sets.push(resource(stateful_set(
        instance_key,
        namespace,
        component,
        image,
        Some(vec!["lightningd".to_owned()]),
        &args,
        "/home/cln/.lightning",
        &json!({
            "exec": {"command": ["lightning-cli", "--lightning-dir=/home/cln/.lightning", "--network=regtest", "getinfo"]}
        }),
    ))?);
    Ok(())
}

fn render_cdk(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    lab: &LabSpec,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let lightning = linked_target(lab, component, LinkKind::LightningBackend)?;
    let name = component
        .config
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Proofstorm CDK mint");
    let description = component
        .config
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Proofstorm regtest CDK mint");
    let config_name = format!("{}-config", component.id);
    let data_name = format!("{}-data", component.id);
    let config = mint_config(&component.id, &lightning.id, name, description);
    rendered.config_maps.push(resource(json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": metadata(&config_name, instance_key, namespace, Some(&component.id)),
        "data": {"config.toml": config}
    }))?);
    rendered.persistent_volume_claims.push(resource(json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": metadata(&data_name, instance_key, namespace, Some(&component.id)),
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}
    }))?);
    rendered.services.push(resource(service(
        instance_key,
        namespace,
        component,
        &[("http", 3_338)],
    ))?);
    let labels = labels(instance_key, Some(&component.id));
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": metadata(&component.id, instance_key, namespace, Some(&component.id)),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": {"labels": labels}, "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(instance_key), "containers": [{
                    "name": "component", "image": image, "imagePullPolicy": "IfNotPresent",
                    "command": ["cdk-mintd"], "args": ["--config", "/config/config.toml"],
                    "env": [{"name": "CDK_MINTD_WORK_DIR", "value": "/app/data"}],
                    "ports": [{"name": "http", "containerPort": 3338}],
                    "securityContext": container_security(),
                    "readinessProbe": {"httpGet": {"path": "/v1/info", "port": 3338}, "periodSeconds": 3, "failureThreshold": 40},
                    "volumeMounts": [
                        {"name": "config", "mountPath": "/config", "readOnly": true},
                        {"name": "data", "mountPath": "/app/data"},
                        {"name": "lnd", "mountPath": "/lnd", "readOnly": true}
                    ]
                }], "volumes": [
                    {"name": "config", "configMap": {"name": config_name}},
                    {"name": "data", "persistentVolumeClaim": {"claimName": data_name}},
                    {"name": "lnd", "persistentVolumeClaim": {"claimName": format!("data-{}-0", lightning.id)}}
                ]
            }
        }}
    }))?);
    Ok(())
}

fn render_attacker(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let labels = labels(instance_key, Some(&component.id));
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": metadata(&component.id, instance_key, namespace, Some(&component.id)),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": {"labels": labels}, "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(instance_key), "containers": [{
                    "name": "component", "image": image, "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "trap : TERM INT; sleep infinity & wait"],
                    "securityContext": container_security()
                }]
            }
        }}
    }))?);
    Ok(())
}

fn render_wallet(
    rendered: &mut RenderedLab,
    instance_key: &str,
    namespace: &str,
    component: &ComponentSpec,
    image: &str,
) -> Result<(), AdapterError> {
    let data_name = format!("{}-data", component.id);
    rendered.persistent_volume_claims.push(resource(json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": metadata(&data_name, instance_key, namespace, Some(&component.id)),
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}
    }))?);
    let labels = labels(instance_key, Some(&component.id));
    rendered.deployments.push(resource(json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": metadata(&component.id, instance_key, namespace, Some(&component.id)),
        "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
            "metadata": {"labels": labels}, "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(instance_key), "containers": [{
                    "name": "component", "image": image, "imagePullPolicy": "IfNotPresent",
                    "command": ["/bin/sh", "-c", "trap 'exit 0' TERM INT; while :; do sleep 3600; done"],
                    "env": [{"name": "HOME", "value": "/wallet"}, {"name": "PROOFSTORM_WALLET", "value": component.id}],
                    "securityContext": container_security(),
                    "volumeMounts": [{"name": "wallet", "mountPath": "/wallet"}]
                }], "volumes": [{"name": "wallet", "persistentVolumeClaim": {"claimName": data_name}}]
            }
        }}
    }))?);
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "uniform adapter resource contract"
)]
fn stateful_set(
    instance_key: &str,
    namespace: &str,
    component: &ComponentSpec,
    image: &str,
    command: Option<Vec<String>>,
    args: &[String],
    data_mount: &str,
    readiness_probe: &Value,
) -> Value {
    let labels = labels(instance_key, Some(&component.id));
    let mut container = json!({
        "name": "component", "image": image, "imagePullPolicy": "IfNotPresent", "args": args,
        "securityContext": container_security(),
        "readinessProbe": {"periodSeconds": 3, "failureThreshold": 40},
        "volumeMounts": [{"name": "data", "mountPath": data_mount}]
    });
    container["readinessProbe"]
        .as_object_mut()
        .expect("probe object")
        .extend(readiness_probe.as_object().expect("probe object").clone());
    if let Some(command) = command {
        container["command"] = json!(command);
    }
    json!({
        "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": metadata(&component.id, instance_key, namespace, Some(&component.id)),
        "spec": {"serviceName": component.id, "replicas": 1, "selector": {"matchLabels": labels},
            "template": {"metadata": {"labels": labels}, "spec": {
                "serviceAccountName": "proofstorm-workload", "automountServiceAccountToken": false, "enableServiceLinks": false,
                "securityContext": pod_security(), "affinity": instance_affinity(instance_key), "containers": [container]
            }},
            "volumeClaimTemplates": [{"metadata": {"name": "data", "labels": labels},
                "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}}}]
        }
    })
}

fn service(
    instance_key: &str,
    namespace: &str,
    component: &ComponentSpec,
    ports: &[(&str, u16)],
) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": metadata(&component.id, instance_key, namespace, Some(&component.id)),
        "spec": {"selector": labels(instance_key, Some(&component.id)),
            "ports": ports.iter().map(|(name, port)| json!({"name": name, "port": port, "targetPort": port})).collect::<Vec<_>>()}
    })
}

fn action_network_policy(instance_key: &str, namespace: &str) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": metadata("allow-controller-actions", instance_key, namespace, None),
        "spec": {"podSelector": {"matchExpressions": [
                {"key": "proofstorm.dev/operation", "operator": "Exists"}
            ]},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}}}]}],
            "egress": [
                {"to": [{"podSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}}}]},
                {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}
            ]}
    })
}

/// Render the complete allow-list for one protocol component.
///
/// Components in `excluded_components` are removed from both ingress and
/// egress peers. Controller action Pods remain reachable because they carry no
/// component label and are governed by their own policy.
///
/// # Errors
///
/// Returns an error only if the fixed Kubernetes resource contract is invalid.
pub fn render_component_network_policy(
    instance_key: &str,
    component: &str,
    excluded_components: &[String],
) -> Result<NetworkPolicy, AdapterError> {
    let namespace = instance_namespace(instance_key);
    let mut peer_selector = json!({"matchLabels": {INSTANCE_LABEL: instance_key}});
    if !excluded_components.is_empty() {
        peer_selector["matchExpressions"] = json!([{
            "key": NETWORK_IDENTITY_LABEL,
            "operator": "NotIn",
            "values": excluded_components
        }]);
    }
    resource(json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": metadata(component, instance_key, &namespace, Some(component)),
        "spec": {"podSelector": {"matchLabels": {NETWORK_IDENTITY_LABEL: component}},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{"from": [{"podSelector": peer_selector.clone()}]}],
            "egress": [
                {"to": [{"podSelector": peer_selector}]},
                {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}
            ]}
    }))
}

fn linked_target<'a>(
    lab: &'a LabSpec,
    component: &ComponentSpec,
    kind: LinkKind,
) -> Result<&'a ComponentSpec, AdapterError> {
    let link = lab
        .links
        .iter()
        .find(|link| link.from == component.id && link.kind == kind)
        .ok_or_else(|| AdapterError::MissingLink {
            component: component.id.clone(),
            link: kind,
        })?;
    lab.components
        .iter()
        .find(|target| target.id == link.to)
        .ok_or_else(|| AdapterError::MissingTarget {
            component: component.id.clone(),
            target: link.to.clone(),
        })
}

fn mint_config(component: &str, lightning: &str, name: &str, description: &str) -> String {
    format!(
        "[info]\nurl = \"http://{component}:3338\"\nlisten_host = \"0.0.0.0\"\nlisten_port = 3338\nmnemonic = \"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about\"\ninput_fee_ppk = 100\n\n[mint_info]\nname = {name:?}\ndescription = {description:?}\nurls = [\"http://{component}:3338\"]\n\n[ln]\nln_backend = \"lnd\"\nunit = \"sat\"\nmin_mint = 1\nmax_mint = 500000\nmin_melt = 1\nmax_melt = 500000\n\n[lnd]\naddress = \"https://{lightning}:10009\"\ncert_file = \"/lnd/tls.cert\"\nmacaroon_file = \"/lnd/data/chain/bitcoin/regtest/admin.macaroon\"\n\n[database]\nengine = \"sqlite\"\n"
    )
}

fn metadata(name: &str, instance_key: &str, namespace: &str, component: Option<&str>) -> Value {
    json!({"name": name, "namespace": namespace, "labels": labels(instance_key, component)})
}

fn labels(instance_key: &str, component: Option<&str>) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (INSTANCE_LABEL.to_owned(), instance_key.to_owned()),
        (
            "app.kubernetes.io/managed-by".to_owned(),
            "proofstormd".to_owned(),
        ),
    ]);
    if let Some(component) = component {
        labels.insert(COMPONENT_LABEL.to_owned(), component.to_owned());
        labels.insert(NETWORK_IDENTITY_LABEL.to_owned(), component.to_owned());
    }
    labels
}

fn pod_security() -> Value {
    json!({"runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000, "fsGroup": 1000,
        "seccompProfile": {"type": "RuntimeDefault"}})
}

fn instance_affinity(instance_key: &str) -> Value {
    json!({"podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
        "labelSelector": {"matchLabels": {INSTANCE_LABEL: instance_key}},
        "topologyKey": "kubernetes.io/hostname"
    }]}})
}

fn container_security() -> Value {
    json!({"allowPrivilegeEscalation": false, "capabilities": {"drop": ["ALL"]}})
}

fn resource<T: DeserializeOwned>(value: Value) -> Result<T, AdapterError> {
    Ok(serde_json::from_value(value)?)
}

fn append_inventory<T>(
    output: &mut Vec<InventoryEntry>,
    api_version: &str,
    kind: &str,
    resources: &[T],
) where
    T: k8s_openapi::Metadata<Ty = k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta>,
{
    output.extend(resources.iter().map(|resource| InventoryEntry {
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        namespace: resource.metadata().namespace.clone().unwrap_or_default(),
        name: resource.metadata().name.clone().unwrap_or_default(),
    }));
}

#[must_use]
pub fn component_ports(component: &ComponentSpec) -> BTreeMap<String, u16> {
    match component.kind {
        ComponentKind::Bitcoin => BTreeMap::from([
            ("p2p".into(), 18_444),
            ("rpc".into(), 18_443),
            ("zmq_block".into(), 28_334),
            ("zmq_tx".into(), 28_335),
        ]),
        ComponentKind::Lightning if component.implementation == "cln" => {
            BTreeMap::from([("p2p".into(), 9_735)])
        }
        ComponentKind::Lightning => BTreeMap::from([
            ("p2p".into(), 9_735),
            ("rest".into(), 8_080),
            ("rpc".into(), 10_009),
        ]),
        ComponentKind::Mint => BTreeMap::from([("http".into(), 3_338)]),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use proofstorm_core::{
        API_VERSION, ComponentSpec, ControlClass, LabPolicy, LinkSpec, default_catalog,
        resolve_lock,
    };

    use super::*;

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
            version: None,
            config_version: "v1alpha1".into(),
            control,
            config: BTreeMap::new(),
        }
    }

    #[test]
    fn renders_pinned_three_component_lab_and_stable_inventory() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "static-lab".into(),
            components: vec![
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
                component(
                    "wallet",
                    ComponentKind::Wallet,
                    "nutshell-wallet",
                    ControlClass::Laboratory,
                ),
            ],
            links: vec![
                LinkSpec {
                    kind: LinkKind::ChainBackend,
                    from: "lightning".into(),
                    to: "chain".into(),
                },
                LinkSpec {
                    kind: LinkKind::LightningBackend,
                    from: "mint".into(),
                    to: "lightning".into(),
                },
            ],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("lock");
        let rendered = render_lab("i0123456789012345678", &lab, &lock).expect("render");
        assert_eq!(rendered.services.len(), 3);
        assert_eq!(rendered.stateful_sets.len(), 2);
        assert_eq!(rendered.deployments.len(), 2);
        assert_eq!(rendered.persistent_volume_claims.len(), 2);
        assert_eq!(rendered.network_policies.len(), lab.components.len() + 1);
        assert!(
            rendered.network_policies.iter().any(|policy| {
                policy.metadata.name.as_deref() == Some("allow-controller-actions")
            })
        );
        assert_eq!(rendered.inventory(), rendered.inventory());
        assert!(
            lock.entries
                .iter()
                .all(|entry| entry.image.contains("@sha256:"))
        );
    }

    #[test]
    fn component_network_policy_excludes_only_selected_peers_and_keeps_dns() {
        let policy = render_component_network_policy(
            "i0123456789012345678",
            "mint-lnd",
            &["payer-lnd".into(), "attacker-cln".into()],
        )
        .expect("component policy");
        let value = serde_json::to_value(policy).expect("policy JSON");
        assert_eq!(value["metadata"]["name"], "mint-lnd");
        assert_eq!(
            value["spec"]["podSelector"]["matchLabels"][NETWORK_IDENTITY_LABEL],
            "mint-lnd"
        );
        let ingress = &value["spec"]["ingress"][0]["from"][0]["podSelector"]["matchExpressions"][0];
        assert_eq!(ingress["key"], NETWORK_IDENTITY_LABEL);
        assert_eq!(ingress["operator"], "NotIn");
        assert_eq!(
            ingress["values"],
            serde_json::json!(["payer-lnd", "attacker-cln"])
        );
        assert_eq!(
            value["spec"]["egress"][1]["ports"][0]["port"],
            serde_json::json!(53)
        );
    }

    #[test]
    fn renders_cln_with_private_rpc_and_versioned_pinned_adapter() {
        let lab = LabSpec {
            api_version: API_VERSION.into(),
            name: "cln-lab".into(),
            components: vec![
                component(
                    "chain",
                    ComponentKind::Bitcoin,
                    "bitcoin-core",
                    ControlClass::Laboratory,
                ),
                component(
                    "attacker-cln",
                    ComponentKind::Lightning,
                    "cln",
                    ControlClass::Attacker,
                ),
            ],
            links: vec![LinkSpec {
                kind: LinkKind::ChainBackend,
                from: "attacker-cln".into(),
                to: "chain".into(),
            }],
            policy: LabPolicy::default(),
        };
        let lock = resolve_lock(&lab, &default_catalog()).expect("CLN lock");
        let rendered = render_lab("i0123456789012345678", &lab, &lock).expect("CLN render");
        let cln = rendered
            .stateful_sets
            .iter()
            .find(|stateful_set| stateful_set.metadata.name.as_deref() == Some("attacker-cln"))
            .expect("CLN StatefulSet");
        let pod = cln
            .spec
            .as_ref()
            .expect("CLN spec")
            .template
            .spec
            .as_ref()
            .expect("CLN pod");
        let args = pod.containers[0].args.as_ref().expect("CLN args");
        assert!(args.contains(&"--dev-no-reconnect".to_owned()));
        assert!(args.contains(&"--bitcoin-rpcconnect=chain".to_owned()));
        assert_eq!(
            component_ports(
                lab.components
                    .iter()
                    .find(|component| component.id == "attacker-cln")
                    .expect("CLN component")
            ),
            BTreeMap::from([("p2p".to_owned(), 9_735)])
        );
        assert!(lock.entries.iter().any(|entry| {
            entry.catalog_id == "cln"
                && entry.version == "26.06.7"
                && entry.image.contains("@sha256:")
        }));
    }
}
