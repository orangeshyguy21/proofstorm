use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_NETWORK_DELAY_MS: u32 = 60_000;
pub const MAX_NETWORK_JITTER_MS: u32 = 10_000;
pub const MAX_NETWORK_LOSS_BASIS_POINTS: u16 = 10_000;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkFaultFeature {
    Partition,
    Heal,
    Delay,
    Loss,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkFaultDirection {
    FromTo,
    Bidirectional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkFaultBounds {
    pub max_delay_ms: Option<u32>,
    pub max_jitter_ms: Option<u32>,
    pub max_loss_basis_points: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkFaultBackend {
    pub api_version: String,
    pub id: String,
    pub version: String,
    pub features: BTreeSet<NetworkFaultFeature>,
    pub directions: BTreeSet<NetworkFaultDirection>,
    pub bounds: NetworkFaultBounds,
}

impl NetworkFaultBackend {
    #[must_use]
    pub fn supports(&self, feature: NetworkFaultFeature) -> bool {
        self.features.contains(&feature)
    }
}

#[must_use]
pub fn network_policy_fault_backend() -> NetworkFaultBackend {
    NetworkFaultBackend {
        api_version: crate::API_VERSION.into(),
        id: "kubernetes-network-policy".into(),
        version: "networking.k8s.io/v1".into(),
        features: [NetworkFaultFeature::Partition, NetworkFaultFeature::Heal]
            .into_iter()
            .collect(),
        directions: [NetworkFaultDirection::Bidirectional].into_iter().collect(),
        bounds: NetworkFaultBounds {
            max_delay_ms: None,
            max_jitter_ms: None,
            max_loss_basis_points: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_policy_backend_does_not_claim_traffic_shaping() {
        let backend = network_policy_fault_backend();
        assert!(backend.supports(NetworkFaultFeature::Partition));
        assert!(backend.supports(NetworkFaultFeature::Heal));
        assert!(!backend.supports(NetworkFaultFeature::Delay));
        assert!(!backend.supports(NetworkFaultFeature::Loss));
        assert_eq!(
            backend.directions,
            [NetworkFaultDirection::Bidirectional].into_iter().collect()
        );
        assert_eq!(backend.bounds.max_delay_ms, None);
        assert_eq!(backend.bounds.max_loss_basis_points, None);
    }
}
