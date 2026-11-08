//! Runtime settings the user owns: `runtime.toml` in the installation home.
//!
//! `storm config` edits the file and `storm setup` applies it. Every key is
//! optional; an absent key keeps Proofstorm's default.
use serde::{Deserialize, Serialize};

use crate::{NO_LIMIT, ResourceAmount, quantity_value};

/// The controller's CPU request. The chart's values must match these defaults.
pub const CONTROLLER_CPU_REQUEST: &str = "250m";
pub const CONTROLLER_MEMORY_REQUEST: &str = "64Mi";
pub const CONTROLLER_MEMORY_LIMIT: &str = "1Gi";

/// How much work the controller runs at once, and how long a one-off action
/// may wait to start before it fails without running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerLimits {
    pub action_slots: u16,
    pub native_starts: u16,
    pub native_starts_per_component: u16,
    pub native_polls: u16,
    /// A one-off action that hasn't started this long after acceptance fails
    /// instead of starting. By then its caller has usually given up, and a late
    /// start would run, say, a payment hours after anyone expected it (PS-44).
    pub start_deadline_seconds: u32,
}

impl Default for ControllerLimits {
    fn default() -> Self {
        Self {
            action_slots: 16,
            native_starts: 4,
            native_starts_per_component: 2,
            native_polls: 6,
            start_deadline_seconds: 120,
        }
    }
}

const ENV: [&str; 5] = [
    "PROOFSTORM_ACTION_SLOTS",
    "PROOFSTORM_NATIVE_STARTS",
    "PROOFSTORM_NATIVE_STARTS_PER_COMPONENT",
    "PROOFSTORM_NATIVE_POLLS",
    "PROOFSTORM_START_DEADLINE_SECONDS",
];

impl ControllerLimits {
    /// The rules that keep one busy component or one kind of work from taking
    /// every slot (PS-44).
    ///
    /// # Errors
    ///
    /// Describes the first rule the values break.
    pub fn validate(&self) -> Result<(), String> {
        let values = [
            ("action_slots", u32::from(self.action_slots)),
            ("native_starts", u32::from(self.native_starts)),
            (
                "native_starts_per_component",
                u32::from(self.native_starts_per_component),
            ),
            ("native_polls", u32::from(self.native_polls)),
            ("start_deadline_seconds", self.start_deadline_seconds),
        ];
        if let Some((name, _)) = values.iter().find(|(_, value)| *value == 0) {
            return Err(format!("controller.{name} must be at least 1"));
        }
        if self.native_starts_per_component >= self.native_starts {
            return Err("controller.native_starts_per_component must be less than controller.native_starts, so one component can't hold every start".into());
        }
        if u32::from(self.native_starts) + u32::from(self.native_polls)
            >= u32::from(self.action_slots)
        {
            return Err("controller.native_starts plus controller.native_polls must be less than controller.action_slots, so other actions keep a slot".into());
        }
        Ok(())
    }

    /// Environment variables the controller reads at startup.
    #[must_use]
    pub fn env(&self) -> [(&'static str, String); 5] {
        [
            (ENV[0], self.action_slots.to_string()),
            (ENV[1], self.native_starts.to_string()),
            (ENV[2], self.native_starts_per_component.to_string()),
            (ENV[3], self.native_polls.to_string()),
            (ENV[4], self.start_deadline_seconds.to_string()),
        ]
    }

    /// Read the controller's environment; unset variables keep the defaults.
    ///
    /// # Errors
    ///
    /// Rejects an unparsable value or a combination [`Self::validate`] refuses.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut limits = Self::default();
        for (index, name) in ENV.iter().enumerate() {
            let Some(value) = get(name) else { continue };
            let invalid = || format!("{name} must be a positive integer, not {value:?}");
            match index {
                0 => limits.action_slots = value.parse().map_err(|_| invalid())?,
                1 => limits.native_starts = value.parse().map_err(|_| invalid())?,
                2 => limits.native_starts_per_component = value.parse().map_err(|_| invalid())?,
                3 => limits.native_polls = value.parse().map_err(|_| invalid())?,
                _ => limits.start_deadline_seconds = value.parse().map_err(|_| invalid())?,
            }
        }
        limits.validate()?;
        Ok(limits)
    }
}

/// The parsed `runtime.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    #[serde(default)]
    pub controller: ControllerSettings,
    #[serde(default)]
    pub cluster: ClusterSettings,
}

/// The local cluster's shape. Kubernetes fixes both when a node is created.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pods_per_node: Option<u32>,
}

/// k3s's own pod limit per node.
pub const DEFAULT_MAX_PODS_PER_NODE: u32 = 110;
/// The most pods a node may run; its pod address range grows to fit.
pub const MAX_PODS_PER_NODE: u32 = 4000;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<ResourceAmount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<ResourceAmount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_slots: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_starts: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_starts_per_component: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_polls: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_deadline_seconds: Option<u32>,
}

/// One settable key: its dotted name, whether it holds a quantity or a count,
/// its default, and what applying a change does.
pub struct SettingKey {
    pub name: &'static str,
    pub quantity: bool,
    pub default: &'static str,
    pub effect: &'static str,
}

const RESTART: &str = "restarts the controller once";
const RECREATE: &str =
    "applies when the cluster is created: storm setup --recreate, which deletes all cells";

pub const SETTING_KEYS: [SettingKey; 11] = [
    SettingKey {
        name: "controller.cpu.request",
        quantity: true,
        default: CONTROLLER_CPU_REQUEST,
        effect: RESTART,
    },
    SettingKey {
        name: "controller.cpu.limit",
        quantity: true,
        default: NO_LIMIT,
        effect: RESTART,
    },
    SettingKey {
        name: "controller.memory.request",
        quantity: true,
        default: CONTROLLER_MEMORY_REQUEST,
        effect: RESTART,
    },
    SettingKey {
        name: "controller.memory.limit",
        quantity: true,
        default: CONTROLLER_MEMORY_LIMIT,
        effect: RESTART,
    },
    SettingKey {
        name: "controller.action_slots",
        quantity: false,
        default: "16",
        effect: RESTART,
    },
    SettingKey {
        name: "controller.native_starts",
        quantity: false,
        default: "4",
        effect: RESTART,
    },
    SettingKey {
        name: "controller.native_starts_per_component",
        quantity: false,
        default: "2",
        effect: RESTART,
    },
    SettingKey {
        name: "controller.native_polls",
        quantity: false,
        default: "6",
        effect: RESTART,
    },
    SettingKey {
        name: "controller.start_deadline_seconds",
        quantity: false,
        default: "120",
        effect: RESTART,
    },
    SettingKey {
        name: "cluster.agents",
        quantity: false,
        default: "1",
        effect: RECREATE,
    },
    SettingKey {
        name: "cluster.max_pods_per_node",
        quantity: false,
        default: "110",
        effect: RECREATE,
    },
];

impl RuntimeSettings {
    /// The controller limits with every unset key at its default.
    #[must_use]
    pub fn controller_limits(&self) -> ControllerLimits {
        let defaults = ControllerLimits::default();
        let c = &self.controller;
        ControllerLimits {
            action_slots: c.action_slots.unwrap_or(defaults.action_slots),
            native_starts: c.native_starts.unwrap_or(defaults.native_starts),
            native_starts_per_component: c
                .native_starts_per_component
                .unwrap_or(defaults.native_starts_per_component),
            native_polls: c.native_polls.unwrap_or(defaults.native_polls),
            start_deadline_seconds: c
                .start_deadline_seconds
                .unwrap_or(defaults.start_deadline_seconds),
        }
    }

    /// The value a key holds, or `None` when it is unset.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        let c = &self.controller;
        let amount = |amount: &Option<ResourceAmount>, request: bool| {
            amount.as_ref().and_then(|amount| {
                if request {
                    amount.request.clone()
                } else {
                    amount.limit.clone()
                }
            })
        };
        match key {
            "controller.cpu.request" => amount(&c.cpu, true),
            "controller.cpu.limit" => amount(&c.cpu, false),
            "controller.memory.request" => amount(&c.memory, true),
            "controller.memory.limit" => amount(&c.memory, false),
            "controller.action_slots" => c.action_slots.map(|v| v.to_string()),
            "controller.native_starts" => c.native_starts.map(|v| v.to_string()),
            "controller.native_starts_per_component" => {
                c.native_starts_per_component.map(|v| v.to_string())
            }
            "controller.native_polls" => c.native_polls.map(|v| v.to_string()),
            "controller.start_deadline_seconds" => c.start_deadline_seconds.map(|v| v.to_string()),
            "cluster.agents" => self.cluster.agents.map(|v| v.to_string()),
            "cluster.max_pods_per_node" => self.cluster.max_pods_per_node.map(|v| v.to_string()),
            _ => None,
        }
    }

    /// Check every value and the combinations the controller enforces.
    ///
    /// # Errors
    ///
    /// Describes the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        for (name, amount, default_request, default_limit) in [
            ("cpu", &self.controller.cpu, CONTROLLER_CPU_REQUEST, None),
            (
                "memory",
                &self.controller.memory,
                CONTROLLER_MEMORY_REQUEST,
                Some(CONTROLLER_MEMORY_LIMIT),
            ),
        ] {
            let request = amount.as_ref().and_then(|amount| amount.request.as_deref());
            let limit = amount.as_ref().and_then(|amount| amount.limit.as_deref());
            for (field, value) in [
                ("request", request),
                ("limit", limit.filter(|limit| *limit != NO_LIMIT)),
            ] {
                if let Some(value) = value
                    && !quantity_value(value).is_some_and(|value| value > 0.0)
                {
                    return Err(format!(
                        "controller.{name}.{field} must be a positive Kubernetes quantity such as 250m, 2 or 1Gi, not {value:?}"
                    ));
                }
            }
            // Compare what the controller gets: an unset value is the default.
            let shown = |value: Option<&str>, default: &str| match value {
                Some(value) => value.to_owned(),
                None => format!("{default} by default"),
            };
            let effective_limit = match limit {
                Some(NO_LIMIT) => None,
                Some(limit) => Some(limit),
                None => default_limit,
            };
            if let Some(effective_limit) = effective_limit
                && quantity_value(request.unwrap_or(default_request))
                    > quantity_value(effective_limit)
            {
                return Err(format!(
                    "controller.{name}.request ({}) must not exceed controller.{name}.limit ({}); set both",
                    shown(request, default_request),
                    shown(limit, effective_limit),
                ));
            }
        }
        if self
            .cluster
            .agents
            .is_some_and(|agents| !(1..=32).contains(&agents))
        {
            return Err("cluster.agents must be 1..=32".into());
        }
        if self
            .cluster
            .max_pods_per_node
            .is_some_and(|pods| !(10..=MAX_PODS_PER_NODE).contains(&pods))
        {
            return Err(format!(
                "cluster.max_pods_per_node must be 10..={MAX_PODS_PER_NODE}"
            ));
        }
        self.controller_limits().validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_reach_the_controller_unchanged() {
        let limits = RuntimeSettings::default().controller_limits();
        assert_eq!(limits, ControllerLimits::default());
        limits.validate().unwrap();
        let env = limits.env();
        let read = ControllerLimits::from_env(|name| {
            env.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone())
        });
        assert_eq!(read, Ok(limits));
        assert_eq!(ControllerLimits::from_env(|_| None), Ok(limits));
    }

    #[test]
    fn the_controller_refuses_combinations_that_reopen_ps_44() {
        let bad = |change: fn(&mut ControllerLimits)| {
            let mut limits = ControllerLimits::default();
            change(&mut limits);
            limits.validate().unwrap_err()
        };
        assert!(bad(|l| l.native_starts_per_component = 4).contains("one component"));
        assert!(bad(|l| l.native_polls = 12).contains("other actions"));
        assert!(bad(|l| l.start_deadline_seconds = 0).contains("at least 1"));
        assert!(
            ControllerLimits::from_env(
                |name| (name == "PROOFSTORM_ACTION_SLOTS").then(|| "lots".into())
            )
            .unwrap_err()
            .contains("positive integer")
        );
    }

    #[test]
    fn settings_scale_the_controller_and_validate_quantities() {
        let settings: RuntimeSettings = serde_json::from_value(serde_json::json!({"controller": {
            "action_slots": 64, "native_starts": 24, "native_polls": 24, "cpu": {"limit": "none"}
        }}))
        .unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.controller_limits().native_starts, 24);
        assert_eq!(
            settings.get("controller.cpu.limit").as_deref(),
            Some(NO_LIMIT)
        );
        assert_eq!(settings.get("controller.memory.limit"), None);

        let bad: RuntimeSettings = serde_json::from_value(serde_json::json!({"controller": {
            "memory": {"request": "2Gi", "limit": "1Gi"}
        }}))
        .unwrap();
        assert!(bad.validate().unwrap_err().contains("must not exceed"));
        // An unset value is its default: 250m CPU requested, a 1Gi memory limit.
        for (setting, message) in [
            (
                serde_json::json!({"cpu": {"limit": "100m"}}),
                "controller.cpu.request (250m by default) must not exceed controller.cpu.limit (100m)",
            ),
            (
                serde_json::json!({"memory": {"request": "2Gi"}}),
                "controller.memory.request (2Gi) must not exceed controller.memory.limit (1Gi by default)",
            ),
            (
                serde_json::json!({"memory": {"limit": "32Mi"}}),
                "controller.memory.request (64Mi by default)",
            ),
        ] {
            let below: RuntimeSettings =
                serde_json::from_value(serde_json::json!({"controller": setting})).unwrap();
            let error = below.validate().unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        for fine in [
            serde_json::json!({"cpu": {"limit": "250m"}}),
            serde_json::json!({"memory": {"request": "512Mi", "limit": "none"}}),
        ] {
            let settings: RuntimeSettings =
                serde_json::from_value(serde_json::json!({"controller": fine})).unwrap();
            settings.validate().unwrap();
        }
        assert!(
            serde_json::from_value::<RuntimeSettings>(
                serde_json::json!({"controller": {"slots": 1}})
            )
            .is_err()
        );
    }

    #[test]
    fn every_setting_key_reads_back_through_get() {
        let settings: RuntimeSettings = serde_json::from_value(serde_json::json!({"controller": {
            "cpu": {"request": "1", "limit": "2"}, "memory": {"request": "1Gi", "limit": "2Gi"},
            "action_slots": 20, "native_starts": 5, "native_starts_per_component": 3,
            "native_polls": 7, "start_deadline_seconds": 300
        }, "cluster": {"agents": 3, "max_pods_per_node": 1000}}))
        .unwrap();
        for key in &SETTING_KEYS {
            assert!(settings.get(key.name).is_some(), "{}", key.name);
        }
    }
}
