//! Native CLI orchestration through the same public execution tool agents use.
mod nutshell;
pub use nutshell::assert_payment_accounting;
mod observations;
use crate::{McpClient, cell, json as expect};
use anyhow::{Context, Result, ensure};
pub use observations::{nutshell_balance, observe_wallet, wallet_request};
use serde_json::{Value, json};

pub const BITCOIN_ROOT: &str = "bitcoin-cli -regtest -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only";
pub const BITCOIN: &str = "bitcoin-cli -regtest -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only -rpcwallet=default";
pub const LND: &str = "lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=127.0.0.1:10009";

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// A caller can explicitly supply a projected BOLT11 in a native command.
// Exclude only that request source from this fixture's disclosure assertion;
// generated receipts and every typed request remain subject to it.
pub fn omit_native_request_source(actions: &mut [Value]) {
    for action in actions {
        if matches!(
            action["kind"].as_str(),
            Some("component_exec_live" | "component_forensics")
        ) {
            action["request"] = Value::Null;
        }
    }
}

pub fn execute(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
) -> Result<Value> {
    submit(client, name, run, component, id, script)?;
    wait(client, id)
}

pub fn submit(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
) -> Result<Value> {
    submit_with_output(
        client,
        name,
        run,
        component,
        id,
        script,
        &json!({"mode":"public"}),
    )
}

fn submit_with_output(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
    output: &Value,
) -> Result<Value> {
    let request = json!({"name":name,"run_id":run,"component":component,"request_id":id,"script":script,"timeout_seconds":120,"output":output});
    let accepted = client.call("cell_exec", request.clone())?;
    let retried = client.call("cell_exec", request)?;
    validate_retry(&accepted, &retried)?;
    Ok(accepted)
}

pub(crate) fn validate_retry(accepted: &Value, retried: &Value) -> Result<()> {
    // Public native responses use compact operation identity. Runtime resource
    // names are deliberately absent; phase and document digest may advance.
    for field in ["operation_id", "run_id", "kind", "sequence"] {
        ensure!(
            accepted.get(field).is_some() && accepted[field] == retried[field],
            "operation retry changed {field}: {accepted} {retried}"
        );
    }
    Ok(())
}

pub fn wait(client: &mut McpClient, id: &str) -> Result<Value> {
    let operation = cell::wait_succeeded(client, id)?;
    let content = cell::artifact_content(&operation)?;
    validate_output(content).with_context(|| format!("native command {id}"))?;
    Ok(content.clone())
}

pub fn validate_output(content: &Value) -> Result<()> {
    ensure!(
        content["exit_code"] == 0
            && content["cleanup_verified"] == true
            && content["streams_complete"] == true
            && content["output_truncated"] == false,
        "native command failed or has incomplete evidence: {content}"
    );
    Ok(())
}

pub fn json_content(content: &Value) -> Result<Value> {
    validate_output(content)?;
    serde_json::from_str(expect::string(content, "/stdout")?)
        .context("native command did not produce JSON")
}

pub fn stdout(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
) -> Result<String> {
    Ok(expect::string(
        &execute(client, name, run, component, id, script)?,
        "/stdout",
    )?
    .trim()
    .to_owned())
}

pub fn json_output(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
) -> Result<Value> {
    Ok(serde_json::from_str(&stdout(
        client, name, run, component, id, script,
    )?)?)
}

/// Captures the exact native request IDs a live scenario must find in its journal.
/// Scenario helpers compose ordinary native executions without aggregate operations.
pub struct Session<'a> {
    pub client: &'a mut McpClient,
    name: &'a str,
    run: &'a str,
    pub operations: Vec<String>,
}

impl<'a> Session<'a> {
    pub fn new(client: &'a mut McpClient, name: &'a str, run: &'a str) -> Self {
        Self {
            client,
            name,
            run,
            operations: Vec::new(),
        }
    }

    pub fn execute(&mut self, component: &str, id: &str, script: &str) -> Result<Value> {
        self.start(component, id, script)?;
        wait(self.client, id)
    }

    pub fn start(&mut self, component: &str, id: &str, script: &str) -> Result<Value> {
        self.start_with_output(component, id, script, &json!({"mode":"public"}))
    }

    fn start_with_output(
        &mut self,
        component: &str,
        id: &str,
        script: &str,
        output: &Value,
    ) -> Result<Value> {
        ensure!(
            !self.operations.iter().any(|seen| seen == id),
            "duplicate scenario request: {id}"
        );
        let result = submit_with_output(
            self.client,
            self.name,
            self.run,
            component,
            id,
            script,
            output,
        )?;
        self.operations.push(id.to_owned());
        Ok(result)
    }

    pub fn projected(
        &mut self,
        component: &str,
        id: &str,
        script: &str,
        output: &Value,
    ) -> Result<Value> {
        self.start_with_output(component, id, script, output)?;
        let receipt = wait(self.client, id)?;
        ensure!(
            receipt["projection_succeeded"] == true,
            "native projection {id} failed: {receipt}"
        );
        Ok(receipt["selected_output"].clone())
    }

    pub fn json(&mut self, component: &str, id: &str, script: &str) -> Result<Value> {
        serde_json::from_str(expect::string(
            &self.execute(component, id, script)?,
            "/stdout",
        )?)
        .with_context(|| format!("native command {id} did not return JSON"))
    }

    pub fn poll<T>(
        &mut self,
        component: &str,
        id: &str,
        script: &str,
        mut inspect: impl FnMut(&Value) -> Result<Option<T>>,
    ) -> Result<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        for attempt in 0.. {
            let observed = self.json(component, &format!("{id}-{attempt}"), script)?;
            if let Some(result) = inspect(&observed)? {
                return Ok(result);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "native observation {id} timed out: {observed}"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        unreachable!()
    }

    pub fn mine(&mut self, chain: &str, id: &str, count: u64) -> Result<()> {
        self.execute(chain, id, &format!(
            "set -eu; address=$({BITCOIN} getnewaddress); {BITCOIN} generatetoaddress {count} \"$address\" >/dev/null"
        ))?;
        Ok(())
    }
}

/// Fund both LND nodes, open one exact channel, confirm it, and verify its active point.
/// Each step has its own retry identity; no aggregate platform operation is synthesized.
#[allow(clippy::too_many_arguments)]
pub fn bootstrap(
    client: &mut McpClient,
    name: &str,
    run: &str,
    id: &str,
    chain: &str,
    mint: &str,
    payer: &str,
    funding_sat: u64,
    channel_sat: u64,
    push_sat: u64,
) -> Result<(String, Vec<String>)> {
    ensure!(
        push_sat <= channel_sat && channel_sat < funding_sat,
        "invalid bootstrap amounts"
    );
    let mut session = Session::new(client, name, run);
    session.execute(chain, &format!("{id}-initialize"), &format!(
        "set -eu; {BITCOIN_ROOT} createwallet default >/dev/null 2>&1 || {BITCOIN} getwalletinfo >/dev/null; address=$({BITCOIN} getnewaddress); {BITCOIN} generatetoaddress 101 \"$address\" >/dev/null"
    ))?;
    for node in [mint, payer] {
        let address = session.json(
            node,
            &format!("{id}-address-{node}"),
            &format!("{LND} newaddress p2wkh"),
        )?;
        let address = expect::string(&address, "/address")?;
        session.execute(
            chain,
            &format!("{id}-fund-{node}"),
            &format!(
                "{BITCOIN} sendtoaddress {} {}.{:08}",
                quote(address),
                funding_sat / 100_000_000,
                funding_sat % 100_000_000
            ),
        )?;
    }
    session.mine(chain, &format!("{id}-confirm-funding"), 6)?;
    let identity = session.json(
        mint,
        &format!("{id}-peer-identity"),
        &format!("{LND} getinfo"),
    )?;
    let pubkey = expect::string(&identity, "/identity_pubkey")?;
    session.execute(
        payer,
        &format!("{id}-connect"),
        &format!(
            "{LND} connect {} || {LND} listpeers | grep -F {}",
            quote(&format!("{pubkey}@{mint}:9735")),
            quote(pubkey)
        ),
    )?;
    let opened = session.json(
        payer,
        &format!("{id}-open-channel"),
        &format!(
            "{LND} openchannel --node_key={} --local_amt={channel_sat} --push_amt={push_sat}",
            quote(pubkey)
        ),
    )?;
    expect::string(&opened, "/funding_txid")?;
    session.mine(chain, &format!("{id}-confirm-channel"), 6)?;
    let point = session.poll(
        payer,
        &format!("{id}-active"),
        &format!("{LND} listchannels"),
        |channels| active_channel_point(&opened, channels),
    )?;
    Ok((point, session.operations))
}

pub(crate) fn active_channel_point(opened: &Value, channels: &Value) -> Result<Option<String>> {
    let txid = expect::string(opened, "/funding_txid")?;
    ensure!(
        txid.len() == 64 && txid.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid accepted funding transaction"
    );
    let expected_index = opened
        .get("output_index")
        .map(|value| {
            value
                .as_u64()
                .filter(|index| u32::try_from(*index).is_ok())
                .ok_or_else(|| anyhow::anyhow!("invalid accepted output index"))
        })
        .transpose()?;
    let prefix = format!("{txid}:");
    let mut points = Vec::new();
    for channel in expect::array(channels, "/channels")? {
        let point = expect::string(channel, "/channel_point")?;
        if channel["active"] == true
            && let Some(index) = point.strip_prefix(&prefix)
        {
            let index: u32 = index.parse()?;
            ensure!(
                expected_index.is_none_or(|expected| expected == u64::from(index)),
                "active outpoint differs from accepted output index"
            );
            points.push(point.to_owned());
        }
    }
    ensure!(
        points.len() <= 1,
        "funding transaction matches multiple active channels"
    );
    Ok(points.pop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_observations_require_a_complete_successful_native_receipt() {
        let receipt = json!({"exit_code":0,"cleanup_verified":true,"streams_complete":true,"output_truncated":false,"stdout":r#"{"balance_sat":100}"#});
        assert_eq!(json_content(&receipt).unwrap(), json!({"balance_sat":100}));
        for (field, value) in [
            ("exit_code", json!(1)),
            ("cleanup_verified", json!(false)),
            ("streams_complete", json!(false)),
            ("output_truncated", json!(true)),
            ("stdout", json!("incomplete {")),
        ] {
            let mut invalid = receipt.clone();
            invalid[field] = value;
            assert!(json_content(&invalid).is_err(), "accepted invalid {field}");
        }
    }

    #[test]
    fn retry_uses_public_identity_while_allowing_the_receipt_to_advance() {
        let accepted = json!({"operation_id":"native", "run_id":"run", "kind":"component_exec_live", "sequence":1, "phase":"running", "operation_digest":"first"});
        let mut completed = accepted.clone();
        completed["phase"] = json!("succeeded");
        completed["operation_digest"] = json!("second");
        assert!(validate_retry(&accepted, &completed).is_ok());
        completed["sequence"] = json!(2);
        assert!(validate_retry(&accepted, &completed).is_err());
        assert!(validate_retry(&json!({}), &json!({})).is_err());
    }

    #[test]
    fn channel_identity_comes_from_observation_when_acknowledgement_omits_index() {
        let txid = "a".repeat(64);
        let opened = json!({"funding_txid":txid});
        let point = format!("{txid}:3");
        let channels = json!({"channels":[{"channel_point":point,"active":true}]});
        assert_eq!(
            active_channel_point(&opened, &channels).unwrap(),
            Some(point)
        );
        assert!(
            active_channel_point(&json!({"funding_txid":txid,"output_index":0}), &channels)
                .is_err()
        );
        assert_eq!(
            active_channel_point(&opened, &json!({"channels":[]})).unwrap(),
            None
        );
        assert_eq!(
            active_channel_point(
                &opened,
                &json!({"channels":[{"channel_point":format!("{txid}:3"),"active":false}]})
            )
            .unwrap(),
            None
        );
        assert!(active_channel_point(&opened, &json!({"channels":[{"channel_point":format!("{txid}:0"),"active":true},{"channel_point":format!("{txid}:1"),"active":true}]})).is_err());
    }
}
