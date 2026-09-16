//! Native CLI orchestration through the same public execution tool agents use.
use crate::{McpClient, cell, json as expect};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub const BITCOIN_ROOT: &str = "bitcoin-cli -regtest -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only";
pub const BITCOIN: &str = "bitcoin-cli -regtest -rpcconnect=127.0.0.1 -rpcport=18443 -rpcuser=proofstorm -rpcpassword=proofstorm-regtest-only -rpcwallet=default";
pub const LND: &str = "lncli --lnddir=/home/lnd/.lnd --network=regtest --rpcserver=127.0.0.1:10009";

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn execute(
    client: &mut McpClient,
    name: &str,
    run: &str,
    component: &str,
    id: &str,
    script: &str,
) -> Result<Value> {
    client.call("cell_exec",json!({"name":name,"run_id":run,"component":component,"request_id":id,"script":script,"timeout_seconds":120,"output":{"mode":"public"}}))?;
    let operation = cell::wait_succeeded(client, id)?;
    let content = cell::artifact_content(&operation)?;
    ensure!(
        content["exit_code"] == 0
            && content["cleanup_verified"] == true
            && content["streams_complete"] == true
            && content["output_truncated"] == false,
        "native command {id} failed or has incomplete evidence: {content}"
    );
    Ok(content.clone())
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
) -> Result<String> {
    ensure!(
        push_sat <= channel_sat && channel_sat < funding_sat,
        "invalid bootstrap amounts"
    );
    stdout(
        client,
        name,
        run,
        chain,
        &format!("{id}-initialize"),
        &format!(
            "set -eu; {BITCOIN_ROOT} createwallet default >/dev/null 2>&1 || {BITCOIN} getwalletinfo >/dev/null; address=$({BITCOIN} getnewaddress); {BITCOIN} generatetoaddress 101 \"$address\" >/dev/null"
        ),
    )?;
    for node in [mint, payer] {
        let address = json_output(
            client,
            name,
            run,
            node,
            &format!("{id}-address-{node}"),
            &format!("{LND} newaddress p2wkh"),
        )?;
        let address = expect::string(&address, "/address")?;
        stdout(
            client,
            name,
            run,
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
    mine(
        client,
        name,
        run,
        chain,
        &format!("{id}-confirm-funding"),
        6,
    )?;
    let identity = json_output(
        client,
        name,
        run,
        mint,
        &format!("{id}-peer-identity"),
        &format!("{LND} getinfo"),
    )?;
    let pubkey = expect::string(&identity, "/identity_pubkey")?;
    stdout(
        client,
        name,
        run,
        payer,
        &format!("{id}-connect"),
        &format!(
            "{LND} connect {} || {LND} listpeers | grep -F {}",
            quote(&format!("{pubkey}@{mint}:9735")),
            quote(pubkey)
        ),
    )?;
    let opened = json_output(
        client,
        name,
        run,
        payer,
        &format!("{id}-open-channel"),
        &format!(
            "{LND} openchannel --node_key={} --local_amt={channel_sat} --push_amt={push_sat}",
            quote(pubkey)
        ),
    )?;
    // lncli can acknowledge the funding transaction without an output index.
    // Discover the actual outpoint from the active channel, never guess index 0.
    expect::string(&opened, "/funding_txid")?;
    mine(
        client,
        name,
        run,
        chain,
        &format!("{id}-confirm-channel"),
        6,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let mut attempt = 0;
    loop {
        let channels = json_output(
            client,
            name,
            run,
            payer,
            &format!("{id}-active-{attempt}"),
            &format!("{LND} listchannels"),
        )?;
        if let Some(point) = active_channel_point(&opened, &channels)? {
            return Ok(point);
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "accepted funding transaction {opened} did not become active: {channels}"
        );
        attempt += 1;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn active_channel_point(opened: &Value, channels: &Value) -> Result<Option<String>> {
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

pub fn mine(
    client: &mut McpClient,
    name: &str,
    run: &str,
    chain: &str,
    id: &str,
    count: u64,
) -> Result<()> {
    stdout(
        client,
        name,
        run,
        chain,
        id,
        &format!(
            "set -eu; address=$({BITCOIN} getnewaddress); {BITCOIN} generatetoaddress {count} \"$address\" >/dev/null"
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
