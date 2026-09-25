//! Wait for the controller's protocol observation after an auth mint restart.
use std::{
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::McpClient;

pub(super) fn now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

pub(super) fn wait_protocol_ready(
    client: &mut McpClient,
    instance: &str,
    mint: &str,
    after: i64,
) -> Result<()> {
    for _ in 0..60 {
        let status = client.call(
            "cell_component_status_list",
            json!({"name":instance,"component":mint,"limit":1}),
        )?;
        if status["components"][0]["id"] == mint && ready(&status["components"][0], after, now()?) {
            return Ok(());
        }
        sleep(Duration::from_secs(2));
    }
    bail!("{mint} did not regain fresh protocol readiness after restart")
}

fn ready(component: &Value, after: i64, now: i64) -> bool {
    component["ready"] == true
        && component["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions.iter().any(|condition| {
                    condition["condition_type"] == "protocol_ready" && condition["state"] == "true"
                })
            })
        && component["protocol_observation"]["observed_at_unix"]
            .as_i64()
            .is_some_and(|observed| observed >= after && observed <= now)
        && component["protocol_observation"]["expires_at_unix"]
            .as_i64()
            .is_some_and(|expires| now < expires)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_restart_requires_a_new_successful_unexpired_protocol_observation() {
        let mut component = json!({"ready":true,"conditions":[{"condition_type":"protocol_ready","state":"true"}],"protocol_observation":{"observed_at_unix":10,"expires_at_unix":20}});
        assert!(ready(&component, 10, 11));
        assert!(!ready(&component, 11, 11));
        assert!(!ready(&component, 10, 20));
        assert!(!ready(&component, 10, 9));
        component["conditions"][0]["state"] = json!("false");
        assert!(!ready(&component, 10, 11));
        assert!(!ready(&Value::Null, 10, 11));
    }
}
