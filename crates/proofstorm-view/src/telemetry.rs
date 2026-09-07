//! Credential-free, sampled runtime measurements for the local dashboard.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SystemView {
    pub sampled_at_unix: i64,
    pub error: Option<String>,
    pub labs: Vec<LabUsage>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct UsageTotals {
    pub running: usize,
    pub sampled: usize,
    pub restarts: i32,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<f64>,
}

impl UsageTotals {
    pub fn from_processes<'a>(processes: impl Iterator<Item = &'a ProcessUsage>) -> Self {
        let mut total = Self::default();
        for process in processes {
            total.restarts += process.restarts;
            if process.running {
                total.running += 1;
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
pub struct LabUsage {
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
    pub restarts: i32,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub metrics_timestamp: Option<String>,
    pub cpu_request_millicores: Option<f64>,
    pub memory_request_bytes: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComponentBalance {
    pub component: String,
    pub observed_at_unix: i64,
    pub error: Option<String>,
    pub amounts: Vec<BalanceAmount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BalanceAmount {
    pub label: String,
    pub sat: u64,
}
