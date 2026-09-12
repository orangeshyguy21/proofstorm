//! Credential-free, sampled runtime measurements for the local dashboard.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SystemView {
    pub sampled_at_unix: i64,
    pub error: Option<String>,
    pub cells: Vec<CellUsage>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct UsageTotals {
    pub running: usize,
    #[serde(default)]
    pub ready: usize,
    pub sampled: usize,
    pub restarts: i32,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<f64>,
}

impl UsageTotals {
    #[must_use]
    pub fn from_processes<'a>(processes: impl Iterator<Item = &'a ProcessUsage>) -> Self {
        let mut total = Self::default();
        for process in processes {
            total.restarts = total.restarts.saturating_add(process.restarts);
            if process.running {
                total.running += 1;
                total.ready += usize::from(process.ready);
                if process.cpu_millicores.is_some() && process.memory_bytes.is_some() {
                    total.sampled += 1;
                }
                if let Some(cpu) = process.cpu_millicores {
                    *total.cpu_millicores.get_or_insert(0.0) += cpu;
                }
                if let Some(memory) = process.memory_bytes {
                    *total.memory_bytes.get_or_insert(0.0) += memory;
                }
            }
        }
        if total.running == 0 {
            total.cpu_millicores = Some(0.0);
            total.memory_bytes = Some(0.0);
        }
        total
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CellUsage {
    #[serde(default)]
    pub incarnation: String,
    pub id: String,
    pub name: String,
    pub error: Option<String>,
    pub metrics_error: Option<String>,
    pub totals: UsageTotals,
    pub processes: Vec<ProcessUsage>,
    pub balances: Vec<ComponentBalance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessUsage {
    pub pod: String,
    pub container: String,
    pub component: Option<String>,
    pub state: String,
    pub running: bool,
    pub ready: bool,
    #[serde(default)]
    pub terminated: bool,
    pub restarts: i32,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub metrics_timestamp: Option<String>,
    pub cpu_request_millicores: Option<f64>,
    pub memory_request_bytes: Option<f64>,
    #[serde(default)]
    pub cpu_limit_millicores: Option<f64>,
    #[serde(default)]
    pub memory_limit_bytes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ComponentBalance {
    #[serde(default)]
    pub rollout_digest: Option<String>,
    #[serde(default)]
    pub lightning: Option<LightningObservation>,
    #[serde(default)]
    pub holdings: Option<HoldingsObservation>,
    pub component: String,
    pub observed_at_unix: i64,
    pub error: Option<String>,
    pub amounts: Vec<BalanceAmount>,
    #[serde(default)]
    pub block_height: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BalanceAmount {
    pub label: String,
    pub sat: u64,
}

/// Open channels observed from one node, including disconnected channels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LightningObservation {
    pub observed_at_unix: i64,
    pub error: Option<String>,
    pub node_pubkey: Option<String>,
    pub channels: Vec<ObservedChannel>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedChannel {
    pub funding_outpoint: String,
    pub peer_pubkey: String,
    pub active: bool,
    pub capacity_msat: u64,
    pub local_msat: u64,
    pub remote_msat: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HoldingsObservation {
    pub observed_at_unix: i64,
    pub error: Option<String>,
    pub mints: Vec<MintHolding>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MintHolding {
    /// Opaque identity; raw wallet URLs and credentials are not exposed.
    pub id: String,
    pub mint: Option<String>,
    pub amounts: Vec<BalanceAmount>,
}
impl MintHolding {
    #[must_use]
    pub fn held_sat(&self) -> u64 {
        self.amounts
            .iter()
            .filter(|a| matches!(a.label.as_str(), "Spendable" | "Reserved"))
            .fold(0_u64, |sum, a| sum.saturating_add(a.sat))
    }
}
