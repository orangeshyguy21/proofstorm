use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPhase {
    #[default]
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub owner_principal_id: String,
    pub phase: ExperimentPhase,
    pub created_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    #[default]
    Active,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub experiment_id: String,
    pub instance_id: String,
    pub principal_id: String,
    pub phase: SessionPhase,
    pub started_at_unix: i64,
    pub last_activity_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_unix: Option<i64>,
}

/// A recipient may inspect and consume one private transfer in one wallet.
/// This scope grants no global capability and is independent of session lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateTransferScope {
    pub receive_command_digest: String,
    pub issuer_principal_id: String,
    pub component: String,
    pub mint: String,
    pub reference: String,
}

/// Explicit permission for one private transfer. Sessions never confer or revoke access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccessGrant {
    pub id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub principal_id: String,
    pub scope: PrivateTransferScope,
    pub created_at_unix: i64,
    pub revoked_at_unix: Option<i64>,
}

impl PrivateTransferScope {
    /// Evaluate the normalized public operation request before admission.
    pub fn permits(&self, kind: crate::OperationKind, request: &serde_json::Value) -> bool {
        use crate::OperationKind;
        let same = |pointer, expected: &str| {
            request.pointer(pointer).and_then(serde_json::Value::as_str) == Some(expected)
        };
        match kind {
            OperationKind::WalletBalance => {
                same("/wallet", &self.component) && same("/mint", &self.mint)
            }
            OperationKind::PrivateTransfer => {
                same("/transfer/component", &self.component)
                    && same("/transfer/reference", &self.reference)
                    && matches!(
                        request
                            .pointer("/transfer/transferMethod")
                            .and_then(serde_json::Value::as_str),
                        Some("status" | "deliver")
                    )
            }
            OperationKind::ComponentExecLive => {
                same("/component", &self.component)
                    && same("/private_payload/kind", "consume")
                    && same("/private_payload/reference", &self.reference)
                    && same("/output/mode", "private")
                    && request.get("target_component").is_none()
                    && request.get("private_io").is_none()
                    && request
                        .pointer("/output/fields")
                        .is_none_or(|fields| fields.as_array().is_some_and(Vec::is_empty))
                    && PrivateReceiveCommand::from_request(request).is_some_and(|command| {
                        command.validate().is_ok()
                            && command.digest() == self.receive_command_digest
                    })
            }
            _ => false,
        }
    }
}

/// Exact native command approved by the source for this recipient session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivateReceiveCommand {
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[schemars(range(min = 1, max = 120))]
    pub timeout_seconds: u32,
    pub input: crate::private_io::InputBinding,
}

impl PrivateReceiveCommand {
    /// # Errors
    /// Rejects invalid commands, input placeholders, deadlines and excessive approval size.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.timeout_seconds > 120
            || serde_json::to_vec(self)
                .map_err(|_| "receive command invalid")?
                .len()
                > 8192
        {
            return Err("approved receive command exceeds 120 seconds or 8192 bytes");
        }
        let command = crate::native::NativeCommand {
            private_io: None,
            script: self.script.clone(),
            argv: self.argv.clone(),
            timeout_seconds: self.timeout_seconds,
            output: crate::native::NativeOutput::default(),
        };
        command.validate()?;
        // No payload is accessed during approval; validate only the selected input binding.
        crate::private_io::PrivateIo::Consume {
            bytes: 1,
            sha256: "0".repeat(64),
            input: self.input.clone(),
        }
        .validate(&command)
    }
    #[must_use]
    pub fn digest(&self) -> String {
        crate::digest_json(self)
    }

    fn from_request(request: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(serde_json::json!({
            "script":request.get("script").cloned().unwrap_or_else(|| serde_json::json!("")),
            "argv":request.get("argv").cloned().unwrap_or_else(|| serde_json::json!([])),
            "timeout_seconds":request.get("timeout_seconds")?,
            "input":request.pointer("/private_payload/input")?,
        }))
        .ok()
    }
}
