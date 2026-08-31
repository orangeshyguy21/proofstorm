use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: &str = "proofstorm/v1alpha1";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Capability {
    #[serde(rename = "catalog.read")]
    CatalogRead,
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
    #[serde(default)]
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
    LightningBackend,
    NetworkPath,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    pub kind: LinkKind,
    pub from: String,
    pub to: String,
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
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
    #[serde(default)]
    pub links: Vec<LinkSpec>,
    #[serde(default)]
    pub policy: LabPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateLabRequest {
    pub lab: LabSpec,
}
