//! Credential-free, bounded Service-DNS checks. Batches bound work, never cell size.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod scheduler;
#[cfg(feature = "transport")]
pub mod transport;
#[cfg(feature = "runtime")]
pub mod worker;

pub const PROTOCOL_VERSION: u32 = 2;
pub const PORT: u16 = 19097;
pub const MAX_BATCH_TARGETS: usize = 32;
pub const MAX_BATCHES_PER_WORKER: usize = 4;
pub const MAX_WORKER_CHECKS: usize = MAX_BATCHES_PER_WORKER * MAX_BATCH_TARGETS;
// Holds a maximum-sized valid batch, including 32 paths of 2 KiB each.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
pub const CHECK_TIMEOUT_MILLIS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub component: String,
    pub rollout_digest: String,
    pub port: u16,
    /// `None` selects TCP; `Some` selects an HTTP GET without redirects or credentials.
    pub http_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub instance_key: String,
    pub revision_digest: String,
    pub batch_id: String,
    /// Keep the connection for another batch. Ordinary controller batches close
    /// after their reply, independently of Kubernetes tunnel teardown latency.
    #[serde(default)]
    pub keep_alive: bool,
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Reachable,
    ConnectionRefused,
    DnsFailed,
    TimedOut,
    HttpError,
    TransportError,
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub component: String,
    pub rollout_digest: String,
    pub outcome: Outcome,
    pub elapsed_micros: u64,
    /// Age when this batch was serialized, measured with a monotonic clock.
    pub age_millis: u64,
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Complete {
        protocol_version: u32,
        instance_key: String,
        revision_digest: String,
        batch_id: String,
        observations: Vec<Observation>,
    },
    Rejected {
        code: String,
    },
}

impl Request {
    /// Validate the complete batch before performing any network operation.
    ///
    /// # Errors
    /// Returns a public error code with no copied request values.
    pub fn validate(&self, instance_key: &str) -> Result<(), &'static str> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("protocol_version_mismatch");
        }
        if self.instance_key != instance_key {
            return Err("instance_mismatch");
        }
        if self.batch_id.is_empty()
            || self.batch_id.len() > 128
            || self.revision_digest.is_empty()
            || self.revision_digest.len() > 128
        {
            return Err("invalid_batch_identity");
        }
        if self.targets.is_empty() || self.targets.len() > MAX_BATCH_TARGETS {
            return Err("invalid_batch_size");
        }
        let mut ids = std::collections::BTreeSet::new();
        for target in &self.targets {
            if !dns_label(&target.component)
                || target.port == 0
                || target.rollout_digest.is_empty()
                || target.rollout_digest.len() > 128
                || !ids.insert(&target.component)
                || target.http_path.as_ref().is_some_and(|path| {
                    !path.starts_with('/')
                        || path.len() > 2048
                        || path.bytes().any(|c| c.is_ascii_control())
                })
            {
                return Err("invalid_probe_target");
            }
        }
        Ok(())
    }
}

#[must_use]
pub fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn request() -> Request {
        Request {
            protocol_version: PROTOCOL_VERSION,
            instance_key: "itest".into(),
            revision_digest: "revision".into(),
            batch_id: "batch".into(),
            keep_alive: false,
            targets: vec![Target {
                component: "chain".into(),
                rollout_digest: "rollout".into(),
                port: 18443,
                http_path: None,
            }],
        }
    }

    #[test]
    fn validates_identity_and_every_target_before_execution() {
        let mut request = request();
        assert!(request.validate("itest").is_ok());
        request.protocol_version = PROTOCOL_VERSION - 1;
        assert_eq!(request.validate("itest"), Err("protocol_version_mismatch"));
        request.protocol_version = PROTOCOL_VERSION;
        assert_eq!(request.validate("other"), Err("instance_mismatch"));
        for component in [
            "127.0.0.1",
            "chain.other.svc",
            "https://chain",
            "CHAIN",
            "-chain",
        ] {
            request.targets[0].component = component.into();
            assert_eq!(request.validate("itest"), Err("invalid_probe_target"));
        }
        request.targets[0].component = "chain".into();
        request.targets[0].http_path = Some("/info\r\nAuthorization: secret".into());
        assert_eq!(request.validate("itest"), Err("invalid_probe_target"));
        request.targets[0].http_path = None;
        request.targets.push(request.targets[0].clone());
        assert_eq!(request.validate("itest"), Err("invalid_probe_target"));
    }

    #[test]
    fn maximum_valid_batch_fits_the_frame_budget() {
        let mut batch = request();
        batch.instance_key = "i".repeat(63);
        batch.revision_digest = "r".repeat(128);
        batch.batch_id = "b".repeat(128);
        batch.targets = (0..MAX_BATCH_TARGETS)
            .map(|index| Target {
                component: format!("n{index:02}{}", "x".repeat(60)),
                rollout_digest: "r".repeat(128),
                port: u16::MAX,
                http_path: Some(format!("/{}", "x".repeat(2047))),
            })
            .collect();
        assert!(batch.validate(&batch.instance_key).is_ok());
        assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_FRAME_BYTES);
    }
}
