//! Bounded, task-owned calls. The controller remains the authority boundary.
use super::{TaskStart, validate_task_id};
use crate::native::NativeCommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_BRIDGE_BYTES: u64 = 65536;
pub const GRANT_ANNOTATION: &str = "proofstorm.dev/workspace-control-grant";

/// Recognize the only command form which can establish a controller grant.
#[must_use]
pub fn start_request(script: &str, argv: &[String]) -> Option<TaskStart> {
    if !script.is_empty()
        || argv.len() != 4
        || argv[0] != super::WORKSPACE_RUNNER
        || argv[1] != "workspace"
        || argv[2] != "request"
    {
        return None;
    }
    let super::WorkspaceRequest::Task(super::TaskRequest::Start(start)) =
        serde_json::from_str(&argv[3]).ok()?
    else {
        return None;
    };
    start.validate().ok()?;
    start.control.as_ref()?;
    Some(start)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlScope {
    /// Only these components in the task's original cell revision may be called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Components on which start, stop and restart are explicitly permitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle: Vec<String>,
    /// Exact bidirectional pairs on which temporary partitions may be created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<NetworkPair>,
    #[serde(
        default = "default_fault_seconds",
        skip_serializing_if = "is_default_fault_seconds"
    )]
    pub max_fault_seconds: u32,
    #[serde(default = "default_calls")]
    pub max_calls: u32,
    #[serde(default = "default_timeout")]
    pub max_timeout_seconds: u32,
}

const fn default_calls() -> u32 {
    256
}
const fn default_timeout() -> u32 {
    30
}
const fn default_fault_seconds() -> u32 {
    60
}
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip predicate takes a reference"
)]
fn is_default_fault_seconds(value: &u32) -> bool {
    *value == default_fault_seconds()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkPair {
    pub from_component: String,
    pub to_component: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlOperation {
    ComponentStart {
        component: String,
    },
    ComponentStop {
        component: String,
    },
    ComponentRestart {
        component: String,
    },
    NetworkPartition {
        from_component: String,
        to_component: String,
        duration_seconds: u32,
    },
    /// Only a partition created by this same task can be released.
    NetworkHeal {
        partition_call_id: String,
    },
}

impl ControlScope {
    /// # Errors
    /// Rejects empty, duplicate or oversized scopes.
    pub fn validate(&self) -> Result<(), &'static str> {
        if (self.components.is_empty() && self.lifecycle.is_empty() && self.network.is_empty())
            || self.components.len() > 16
            || self.lifecycle.len() > 16
            || self.network.len() > 16
            || !(1..=3600).contains(&self.max_fault_seconds)
            || !(1..=4096).contains(&self.max_calls)
            || !(1..=300).contains(&self.max_timeout_seconds)
        {
            return Err(
                "control requires nonempty scopes (at most 16 each), 1..4096 calls, a 1..300 second command limit and a 1..3600 second fault limit",
            );
        }
        let mut unique = std::collections::BTreeSet::new();
        for component in &self.components {
            validate_task_id(component)?;
            if !unique.insert(component) {
                return Err("duplicate control component");
            }
        }
        unique.clear();
        for component in &self.lifecycle {
            validate_task_id(component)?;
            if !unique.insert(component) {
                return Err("duplicate lifecycle component");
            }
        }
        let mut pairs = std::collections::BTreeSet::new();
        for pair in &self.network {
            validate_task_id(&pair.from_component)?;
            validate_task_id(&pair.to_component)?;
            if pair.from_component == pair.to_component {
                return Err("partition endpoints must differ");
            }
            let ordered = if pair.from_component < pair.to_component {
                (&pair.from_component, &pair.to_component)
            } else {
                (&pair.to_component, &pair.from_component)
            };
            if !pairs.insert(ordered) {
                return Err("duplicate network pair");
            }
        }
        Ok(())
    }

    pub fn targets(&self) -> impl Iterator<Item = &str> {
        self.components
            .iter()
            .chain(&self.lifecycle)
            .map(String::as_str)
            .chain(
                self.network
                    .iter()
                    .flat_map(|pair| [pair.from_component.as_str(), pair.to_component.as_str()]),
            )
    }

    #[must_use]
    pub fn capabilities(&self) -> Vec<crate::Capability> {
        let mut caps = vec![];
        if !self.lifecycle.is_empty() {
            caps.push(crate::Capability::ComponentControl);
        }
        if !self.network.is_empty() {
            caps.extend([
                crate::Capability::NetworkPartition,
                crate::Capability::NetworkHeal,
            ]);
        }
        caps
    }

    /// # Errors
    /// Rejects commands outside this grant, private custody bindings, or oversized requests.
    pub fn permits(&self, call: &ControlCall) -> Result<(), &'static str> {
        self.validate()?;
        validate_task_id(&call.call_id)?;
        match (&call.command, &call.operation) {
            (Some(command), None) => {
                command.validate()?;
                if !self.components.contains(&call.component)
                    || command.timeout_seconds > self.max_timeout_seconds
                    || command.private_io.is_some()
                {
                    return Err("call is outside the task's control scope");
                }
            }
            (None, Some(operation)) if call.component.is_empty() => match operation {
                ControlOperation::ComponentStart { component }
                | ControlOperation::ComponentStop { component }
                | ControlOperation::ComponentRestart { component } => {
                    if !self.lifecycle.contains(component) {
                        return Err("lifecycle component is outside task scope");
                    }
                }
                ControlOperation::NetworkPartition {
                    from_component,
                    to_component,
                    duration_seconds,
                } => {
                    if *duration_seconds == 0
                        || *duration_seconds > self.max_fault_seconds
                        || !self.network.iter().any(|pair| {
                            (&pair.from_component == from_component
                                && &pair.to_component == to_component)
                                || (&pair.to_component == from_component
                                    && &pair.from_component == to_component)
                        })
                    {
                        return Err("partition is outside task scope or duration bound");
                    }
                }
                ControlOperation::NetworkHeal { partition_call_id } => {
                    validate_task_id(partition_call_id)?;
                    if self.network.is_empty() {
                        return Err("network control is not granted");
                    }
                }
            },
            _ => return Err("provide either component and command, or operation"),
        }
        if serde_json::to_vec(call)
            .map_err(|_| "invalid control call")?
            .len()
            > 8192
        {
            return Err("control call exceeds 8192 bytes");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlCall {
    /// Reuse this ID only for an exact retry; it never starts a second command.
    pub call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<NativeCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ControlOperation>,
}

/// Local transport, not an authentication boundary between scripts sharing a workspace.
/// Only a controller-recorded start grants authority outside the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BridgeRequest {
    Start {
        start: TaskStart,
        owner: String,
    },
    Submit {
        task_id: String,
        owner: String,
        call: ControlCall,
    },
    Poll {
        task_id: String,
        owner: String,
    },
    Close {
        task_id: String,
        owner: String,
    },
    Claim {
        task_id: String,
        owner: String,
        call_id: String,
    },
    Complete {
        task_id: String,
        owner: String,
        call_id: String,
        receipt: Value,
    },
    Result {
        task_id: String,
        owner: String,
        call_id: String,
    },
    Cleanup {
        task_id: String,
        owner: String,
        pending_faults: u32,
    },
}

impl BridgeRequest {
    /// # Errors
    /// Checks identifiers before they become file paths.
    pub fn validate(&self) -> Result<(), &'static str> {
        let (task_id, owner, call_id) = match self {
            Self::Start { start, owner } => {
                start.validate()?;
                (&start.task_id, owner, None)
            }
            Self::Submit {
                task_id,
                owner,
                call,
            } => (task_id, owner, Some(&call.call_id)),
            Self::Poll { task_id, owner }
            | Self::Close { task_id, owner }
            | Self::Cleanup { task_id, owner, .. } => (task_id, owner, None),
            Self::Claim {
                task_id,
                owner,
                call_id,
            }
            | Self::Complete {
                task_id,
                owner,
                call_id,
                ..
            }
            | Self::Result {
                task_id,
                owner,
                call_id,
            } => (task_id, owner, Some(call_id)),
        };
        validate_task_id(task_id)?;
        if owner.is_empty() || owner.len() > 128 || owner.contains('\0') {
            return Err("invalid control owner");
        }
        if let Some(id) = call_id {
            validate_task_id(id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scope_limits_and_path_identifiers_are_checked_before_dispatch() {
        let scope: ControlScope = serde_json::from_value(json!({"components":["chain"]})).unwrap();
        let mut call: ControlCall = serde_json::from_value(json!({"call_id":"height","component":"chain","command":{"script":"true","timeout_seconds":10}})).unwrap();
        scope.permits(&call).unwrap();
        call.component = "wallet".into();
        assert!(scope.permits(&call).is_err());
        call.component = "chain".into();
        call.command.as_mut().unwrap().timeout_seconds = 31;
        assert!(scope.permits(&call).is_err());
        call.command.as_mut().unwrap().timeout_seconds = 10;
        call.call_id = "../other".into();
        assert!(scope.permits(&call).is_err());
        call.call_id = "height".into();
        call.command.as_mut().unwrap().script = "a".repeat(8192);
        assert!(scope.permits(&call).is_err());
        for bad in [
            json!({"components":[]}),
            json!({"components":["chain","chain"]}),
            json!({"components":["chain"],"max_calls":0}),
            json!({"components":["chain"],"max_calls":4097}),
            json!({"components":["chain"],"max_timeout_seconds":301}),
        ] {
            assert!(
                serde_json::from_value::<ControlScope>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<BridgeRequest>(
                json!({"action":"result","task_id":"flow","owner":"start","call_id":"../other"})
            )
            .unwrap()
            .validate()
            .is_err()
        );
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_and_faults_require_explicit_scopes_and_bounded_exact_pairs() {
        let native: ControlScope = serde_json::from_value(json!({"components":["chain"]})).unwrap();
        let scope: ControlScope = serde_json::from_value(json!({"lifecycle":["chain"],"network":[{"from_component":"chain","to_component":"wallet"}],"max_fault_seconds":30})).unwrap();
        scope.validate().unwrap();
        for operation in [
            json!({"kind":"component_restart","component":"chain"}),
            json!({"kind":"network_partition","from_component":"wallet","to_component":"chain","duration_seconds":30}),
            json!({"kind":"network_heal","partition_call_id":"outage"}),
        ] {
            let call: ControlCall =
                serde_json::from_value(json!({"call_id":"step","operation":operation})).unwrap();
            scope.permits(&call).unwrap();
            assert!(native.permits(&call).is_err());
        }
        for operation in [
            json!({"kind":"component_stop","component":"wallet"}),
            json!({"kind":"network_partition","from_component":"chain","to_component":"wallet","duration_seconds":31}),
            json!({"kind":"network_partition","from_component":"chain","to_component":"other","duration_seconds":10}),
            json!({"kind":"network_partition","from_component":"chain","to_component":"wallet","duration_seconds":0}),
        ] {
            let call: ControlCall =
                serde_json::from_value(json!({"call_id":"step","operation":operation})).unwrap();
            assert!(scope.permits(&call).is_err());
        }
        let ambiguous: ControlCall = serde_json::from_value(json!({"call_id":"step","component":"chain","command":{"script":"true","timeout_seconds":10},"operation":{"kind":"component_restart","component":"chain"}})).unwrap();
        assert!(scope.permits(&ambiguous).is_err());
        assert_eq!(
            scope.capabilities(),
            vec![
                crate::Capability::ComponentControl,
                crate::Capability::NetworkPartition,
                crate::Capability::NetworkHeal
            ]
        );
        assert_eq!(
            serde_json::to_value(native).unwrap(),
            json!({"components":["chain"],"max_calls":256,"max_timeout_seconds":30})
        );
    }
}
