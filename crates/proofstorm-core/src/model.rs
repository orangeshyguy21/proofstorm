use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: &str = "proofstorm/v1alpha1";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Capability {
    #[serde(rename = "catalog.read")]
    CatalogRead,
    #[serde(rename = "candidate.build")]
    CandidateBuild,
    #[serde(rename = "candidate.read")]
    CandidateRead,
    #[serde(rename = "candidate.cancel")]
    CandidateCancel,
    #[serde(rename = "lab.read")]
    LabRead,
    #[serde(rename = "lab.create")]
    LabCreate,
    #[serde(rename = "lab.edit")]
    LabEdit,
    #[serde(rename = "lab.clone")]
    LabClone,
    #[serde(rename = "lab.validate")]
    LabValidate,
    #[serde(rename = "lab.publish")]
    LabPublish,
    #[serde(rename = "lab.materialize")]
    LabMaterialize,
    #[serde(rename = "lab.status")]
    LabStatus,
    #[serde(rename = "lab.close")]
    LabClose,
    #[serde(rename = "experiment.create")]
    ExperimentCreate,
    #[serde(rename = "experiment.read")]
    ExperimentRead,
    #[serde(rename = "experiment.close")]
    ExperimentClose,
    #[serde(rename = "lease.acquire")]
    LeaseAcquire,
    #[serde(rename = "lease.release")]
    LeaseRelease,
    #[serde(rename = "action.cancel")]
    ActionCancel,
    #[serde(rename = "topology.inspect")]
    TopologyInspect,
    #[serde(rename = "topology.mutate")]
    TopologyMutate,
    #[serde(rename = "node.add")]
    NodeAdd,
    #[serde(rename = "node.remove")]
    NodeRemove,
    #[serde(rename = "node.control")]
    NodeControl,
    #[serde(rename = "component.control")]
    ComponentControl,
    #[serde(rename = "peer.connect")]
    PeerConnect,
    #[serde(rename = "peer.disconnect")]
    PeerDisconnect,
    #[serde(rename = "wallet.create")]
    WalletCreate,
    #[serde(rename = "wallet.control")]
    WalletControl,
    #[serde(rename = "wallet.fund")]
    WalletFund,
    #[serde(rename = "channel.open")]
    ChannelOpen,
    #[serde(rename = "channel.close")]
    ChannelClose,
    #[serde(rename = "channel.force_close")]
    ChannelForceClose,
    #[serde(rename = "channel.rebalance")]
    ChannelRebalance,
    #[serde(rename = "chain.mine")]
    ChainMine,
    #[serde(rename = "chain.fee")]
    ChainFee,
    #[serde(rename = "chain.reorg")]
    ChainReorg,
    #[serde(rename = "network.delay")]
    NetworkDelay,
    #[serde(rename = "network.drop")]
    NetworkDrop,
    #[serde(rename = "network.partition")]
    NetworkPartition,
    #[serde(rename = "network.heal")]
    NetworkHeal,
    #[serde(rename = "component.logs")]
    ComponentLogs,
    #[serde(rename = "component.exec_live")]
    ComponentExecLive,
    #[serde(rename = "component.forensics")]
    ComponentForensics,
    #[serde(rename = "authentication.test")]
    AuthenticationTest,
    #[serde(rename = "traffic.capture")]
    TrafficCapture,
    #[serde(rename = "oracle.list")]
    OracleList,
    #[serde(rename = "oracle.run")]
    OracleRun,
    #[serde(rename = "artifact.read")]
    ArtifactRead,
    #[serde(rename = "policy.read")]
    PolicyRead,
    #[serde(rename = "policy.edit")]
    PolicyEdit,
    #[serde(rename = "snapshot.create")]
    SnapshotCreate,
    #[serde(rename = "snapshot.restore")]
    SnapshotRestore,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Bitcoin,
    Lightning,
    Mint,
    Database,
    IdentityProvider,
    Wallet,
    Attacker,
    Proxy,
    Oracle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlClass {
    Laboratory,
    Target,
    Attacker,
    Oracle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    pub id: String,
    pub kind: ComponentKind,
    pub implementation: String,
    /// Version of the service binary, image, or implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Version of the implementation adapter's configuration contract.
    pub config_version: String,
    pub control: ControlClass,
    pub config: BTreeMap<String, Value>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    BitcoinPeer,
    LightningPeer,
    ChainBackend,
    PaymentBackend,
    DatabaseBackend,
    AuthenticationBackend,
    NetworkPath,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseRole {
    Primary,
    Cache,
    Authentication,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    Bolt11,
    Bolt12,
    Onchain,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationProtocol {
    Oidc,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BitcoinNetwork {
    Regtest,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DependencyBinding {
    Chain { network: BitcoinNetwork },
    Payment { method: PaymentMethod, unit: String },
    Database { role: DatabaseRole },
    Authentication { protocol: AuthenticationProtocol },
}

// Kubernetes structural schemas cannot merge internally tagged enum branches
// that assign different constants to the same discriminator. Keep the strict
// serde representation above and expose its union as one structural object;
// validate_lab enforces the legal field combinations before publication.
impl JsonSchema for DependencyBinding {
    fn schema_name() -> Cow<'static, str> {
        "DependencyBinding".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::DependencyBinding").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "description": "Typed dependency qualifier. Chain bindings require network; payment bindings require method and unit; database bindings require a role; authentication bindings require a protocol. Proofstorm validates the discriminator-specific fields before publication.",
            "required": ["type"],
            "properties": {
                "type": {
                    "type": "string",
                    "enum": ["chain", "payment", "database", "authentication"]
                },
                "protocol": AuthenticationProtocol::json_schema(generator),
                "network": BitcoinNetwork::json_schema(generator),
                "method": PaymentMethod::json_schema(generator),
                "role": DatabaseRole::json_schema(generator),
                "unit": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 16,
                    "pattern": "^[a-z0-9-]+$"
                }
            },
            "additionalProperties": false,
            "x-kubernetes-validations": [
                {
                    "rule": "self.type == 'chain' ? has(self.network) && !has(self.method) && !has(self.unit) && !has(self.role) && !has(self.protocol) : (self.type == 'payment' ? has(self.method) && has(self.unit) && !has(self.network) && !has(self.role) && !has(self.protocol) : (self.type == 'database' ? has(self.role) && !has(self.network) && !has(self.method) && !has(self.unit) && !has(self.protocol) : has(self.protocol) && !has(self.network) && !has(self.method) && !has(self.unit) && !has(self.role)))",
                    "message": "chain bindings require only network; payment bindings require only method and unit; database bindings require only role; authentication bindings require only protocol"
                }
            ]
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "x-kubernetes-validations" = [{
        "rule": "(self.kind == 'chain_backend' || self.kind == 'payment_backend' || self.kind == 'database_backend' || self.kind == 'authentication_backend') ? has(self.binding) : !has(self.binding)",
        "message": "backend links require a binding; peer and network-path links forbid one"
    }]
))]
pub struct LinkSpec {
    /// Stable binding identity within one lab revision.
    pub id: String,
    pub kind: LinkKind,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<DependencyBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabLimits {
    #[serde(default = "default_max_components")]
    pub max_components: u16,
    #[serde(default = "default_max_links")]
    pub max_links: u16,
    #[serde(default = "default_max_config_bytes")]
    pub max_config_bytes: u32,
}

impl Default for LabLimits {
    fn default() -> Self {
        Self {
            max_components: default_max_components(),
            max_links: default_max_links(),
            max_config_bytes: default_max_config_bytes(),
        }
    }
}

const fn default_max_components() -> u16 {
    64
}

const fn default_max_links() -> u16 {
    256
}

const fn default_max_config_bytes() -> u32 {
    65_536
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabPolicy {
    #[serde(default)]
    pub allow: BTreeSet<Capability>,
    #[serde(default)]
    pub limits: LabLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabSpec {
    pub api_version: String,
    pub name: String,
    pub components: Vec<ComponentSpec>,
    pub links: Vec<LinkSpec>,
    #[serde(default)]
    pub policy: LabPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateLabRequest {
    pub lab: LabSpec,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ComponentSpec, LabSpec};

    #[test]
    fn component_config_must_be_explicit() {
        let missing = json!({
            "id": "chain",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "config_version": "bitcoin-core/30/v1",
            "control": "laboratory"
        });
        let error = serde_json::from_value::<ComponentSpec>(missing)
            .expect_err("an omitted component config must not silently become empty");
        assert!(error.to_string().contains("missing field `config`"));

        let explicit = json!({
            "id": "chain",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "config_version": "bitcoin-core/30/v1",
            "control": "laboratory",
            "config": {}
        });
        serde_json::from_value::<ComponentSpec>(explicit)
            .expect("an explicitly empty component config is valid input");
    }

    #[test]
    fn lab_topology_collections_must_be_explicit() {
        for (field, document) in [
            (
                "components",
                json!({
                    "api_version": "proofstorm/v1alpha1",
                    "name": "missing-components",
                    "links": []
                }),
            ),
            (
                "links",
                json!({
                    "api_version": "proofstorm/v1alpha1",
                    "name": "missing-links",
                    "components": []
                }),
            ),
        ] {
            let error = serde_json::from_value::<LabSpec>(document)
                .expect_err("an omitted topology collection must not silently become empty");
            assert!(
                error
                    .to_string()
                    .contains(&format!("missing field `{field}`")),
                "unexpected error for {field}: {error}"
            );
        }

        serde_json::from_value::<LabSpec>(json!({
            "api_version": "proofstorm/v1alpha1",
            "name": "explicitly-empty",
            "components": [],
            "links": []
        }))
        .expect("explicitly empty topology collections are valid input");
    }
}
