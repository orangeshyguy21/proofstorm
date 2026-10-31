//! Lightning and Spark liquidity through the exact recipe agents are given in
//! `docker/payment/SPARK.md` and the catalog guidance.
use super::{
    Context, GateContext, INSTANCE, LDK, McpClient, RUN, Result, Value, available_sats,
    claim_deposit, deposit_address, ensure, expect, json, native, ssp_status,
};

/// The SSP node's channel to the peer, pushing half so both directions have liquidity.
const CHANNEL: &str = "2000000sat --push-to-counterparty 1000000sat --announce-channel";
const CHANNEL_SAT: u64 = 2_000_000;
/// The on-chain deposit that becomes the SSP's Spark liquidity, in BTC.
const DEPOSIT_BTC: &str = "0.005";

/// The Lightning identities the funded stack routes between.
pub(super) struct Nodes {
    pub(super) ssp: String,
    pub(super) peer: String,
}

pub(super) fn run(context: &GateContext, client: &mut McpClient) -> Result<Nodes> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "spark-chain-wallet",
        &format!("{} createwallet default >/dev/null", native::BITCOIN_ROOT),
    )?;
    session.mine("chain", "spark-mature-coins", 101)?;
    for node in ["ssp-node", "peer"] {
        let address = session.json(
            node,
            &format!("spark-{node}-address"),
            &format!("{LDK} onchain-receive"),
        )?;
        session.execute(
            "chain",
            &format!("spark-fund-{node}"),
            &format!(
                "{} sendtoaddress {} 1",
                native::BITCOIN,
                native::quote(expect::string(&address, "/address")?)
            ),
        )?;
    }
    session.mine("chain", "spark-confirm-funds", 6)?;
    for node in ["ssp-node", "peer"] {
        session.poll(
            node,
            &format!("spark-{node}-funded"),
            &format!("{LDK} get-balances"),
            |v| {
                Ok(
                    (expect::integer(v, "/spendable_onchain_balance_sats")? >= CHANNEL_SAT)
                        .then_some(()),
                )
            },
        )?;
    }
    let mut identity = |node: &str| -> Result<String> {
        let info = session.json(
            node,
            &format!("spark-{node}-identity"),
            &format!("{LDK} get-node-info"),
        )?;
        Ok(expect::string(&info, "/node_id")?.to_owned())
    };
    let nodes = Nodes {
        ssp: identity("ssp-node")?,
        peer: identity("peer")?,
    };
    context.record(
        "spark-nodes.json",
        &json!({"ssp_node":nodes.ssp,"peer":nodes.peer}),
    )?;
    session.execute(
        "ssp-node",
        "spark-open-channel",
        &format!(
            "{LDK} open-channel {} peer:9735 {CHANNEL}",
            native::quote(&nodes.peer)
        ),
    )?;
    // Mine only once the funding transaction has been broadcast.
    session.poll(
        "chain",
        "spark-channel-broadcast",
        &format!("{} getrawmempool", native::BITCOIN),
        |v| Ok((!expect::array(v, "")?.is_empty()).then_some(())),
    )?;
    session.mine("chain", "spark-confirm-channel", 6)?;
    liquidity(context, &mut session)?;
    // The deposit mined more blocks. Establish channel readiness and chain-tip
    // agreement last, immediately before any invoice is requested.
    lightning(context, session.client, &nodes, "funded")?;
    Ok(nodes)
}

/// Deposit on-chain funds to the SSP's Spark wallet and claim them.
fn liquidity(context: &GateContext, session: &mut native::Session<'_>) -> Result<()> {
    // Operators finish key generation after they report ready.
    let address = session.poll("ssp", "spark-deposit-address", &deposit_address(), |v| {
        Ok(v["address"].as_str().map(str::to_owned))
    })?;
    ensure!(
        address.starts_with("bcrt1"),
        "SSP did not return a regtest deposit address"
    );
    context.record("spark-deposit-address.json", &json!({"address":address}))?;
    let sent = session.execute(
        "chain",
        "spark-deposit",
        &format!(
            "{} sendtoaddress {} {DEPOSIT_BTC}",
            native::BITCOIN,
            native::quote(&address)
        ),
    )?;
    let txid = expect::string(&sent, "/stdout")?.trim().to_owned();
    session.mine("chain", "spark-confirm-deposit", 3)?;
    let transaction = session.json(
        "chain",
        "spark-deposit-transaction",
        &format!(
            "{} getrawtransaction {} true",
            native::BITCOIN,
            native::quote(&txid)
        ),
    )?;
    let vout = deposit_output(&transaction, &address)?;
    let raw = session.execute(
        "chain",
        "spark-deposit-hex",
        &format!(
            "{} getrawtransaction {} false",
            native::BITCOIN,
            native::quote(&txid)
        ),
    )?;
    let hex = expect::string(&raw, "/stdout")?.trim().to_owned();
    ensure!(
        !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "deposit transaction is not hex"
    );
    let claimed = session.poll(
        "ssp",
        "spark-claim-deposit",
        &claim_deposit(&hex, vout),
        |v| Ok((v["ok"] == true).then(|| v.clone())),
    )?;
    context.record(
        "spark-deposit-claim.json",
        &json!({"txid":txid,"vout":vout,"claim":claimed}),
    )?;
    let funded = session.poll("ssp", "spark-ssp-funded", &ssp_status(), |v| {
        Ok((available_sats(v)? > 0).then(|| v.clone()))
    })?;
    context.record("spark-funded-ssp.json", &funded)
}

/// The single output of a decoded transaction that pays `address`.
pub(super) fn deposit_output(transaction: &Value, address: &str) -> Result<u64> {
    let outputs = expect::array(transaction, "/vout")?
        .iter()
        .filter(|output| output["scriptPubKey"]["address"] == address)
        .collect::<Vec<_>>();
    ensure!(
        outputs.len() == 1,
        "deposit transaction must pay the SSP exactly once"
    );
    expect::integer(outputs[0], "/n")
}

/// Both channel ends ready and usable, and both nodes at the chain tip. As
/// Bark found, a payer behind the tip can refuse a fresh invoice's expiry.
pub(super) fn lightning(
    context: &GateContext,
    client: &mut McpClient,
    nodes: &Nodes,
    phase: &str,
) -> Result<()> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    for (node, counterparty) in [("ssp-node", &nodes.peer), ("peer", &nodes.ssp)] {
        let channels = session.poll(
            node,
            &format!("spark-{phase}-{node}-channels"),
            &format!("{LDK} list-channels"),
            |v| Ok(usable(v, counterparty)?.then(|| v.clone())),
        )?;
        context.record(&format!("spark-{phase}-{node}-channels.json"), &channels)?;
    }
    let height = session
        .json(
            "chain",
            &format!("spark-{phase}-chain-tip"),
            &format!("{} getblockcount", native::BITCOIN_ROOT),
        )?
        .as_u64()
        .context("Bitcoin chain height is missing")?;
    for node in ["ssp-node", "peer"] {
        let synced = session.poll(
            node,
            &format!("spark-{phase}-{node}-synced"),
            &format!("{LDK} get-node-info"),
            |v| Ok((v["current_best_block"]["height"].as_u64() == Some(height)).then(|| v.clone())),
        )?;
        context.record(&format!("spark-{phase}-{node}-synced.json"), &synced)?;
    }
    Ok(())
}

/// Whether `list-channels` shows a ready, usable channel with `counterparty`.
pub(super) fn usable(channels: &Value, counterparty: &str) -> Result<bool> {
    Ok(expect::array(channels, "/channels")?.iter().any(|channel| {
        channel["counterparty_node_id"] == counterparty
            && channel["is_channel_ready"] == true
            && channel["is_usable"] == true
    }))
}
