use super::{
    ARK, CLN, Context, GateContext, INSTANCE, McpClient, PEER, RUN, Result, expect, native,
};

pub(super) fn run(context: &GateContext, client: &mut McpClient) -> Result<()> {
    let mut session = native::Session::new(client, INSTANCE, RUN);
    session.execute(
        "chain",
        "bark-chain-wallet",
        &format!("{} createwallet default >/dev/null", native::BITCOIN_ROOT),
    )?;
    session.mine("chain", "bark-mature-coins", 101)?;
    let server = session.json("ark", "bark-server-wallet", &format!("{ARK} wallet"))?;
    let address = session.json("cln", "bark-cln-address", &format!("{CLN} newaddr"))?;
    for (id, address, amount) in [
        ("server", expect::string(&server, "/rounds/address")?, 20),
        ("cln", expect::string(&address, "/bech32")?, 1),
    ] {
        session.execute(
            "chain",
            &format!("bark-fund-{id}"),
            &format!(
                "{} sendtoaddress {} {amount}",
                native::BITCOIN,
                native::quote(address)
            ),
        )?;
    }
    session.mine("chain", "bark-confirm-funds", 6)?;
    session.poll("cln", "bark-cln-funded", &format!("{CLN} listfunds"), |v| {
        Ok(expect::array(v, "/outputs")?
            .iter()
            .any(|o| o["status"] == "confirmed")
            .then_some(()))
    })?;
    let peer = session.json("peer", "bark-peer-identity", &format!("{PEER} getinfo"))?;
    let peer_id = native::quote(expect::string(&peer, "/id")?);
    session.execute(
        "cln",
        "bark-connect-peer",
        &format!("{CLN} connect {peer_id} peer 9735"),
    )?;
    session.execute(
        "cln",
        "bark-open-channel",
        &format!("{CLN} -k fundchannel id={peer_id} amount=2000000sat push_msat=1000000000msat"),
    )?;
    session.mine("chain", "bark-confirm-channel", 6)?;
    for (id, cli) in [("cln", CLN), ("peer", PEER)] {
        let channels = session.poll(
            id,
            &format!("bark-{id}-channels"),
            &format!("{cli} listpeerchannels"),
            |v| {
                Ok(expect::array(v, "/channels")?
                    .iter()
                    .any(|c| c["state"] == "CHANNELD_NORMAL")
                    .then(|| v.clone()))
            },
        )?;
        context.record(&format!("bark-{id}-channels.json"), &channels)?;
    }
    // A normal channel can still lag freshly mined regtest blocks. Bark checks
    // absolute HTLC expiries, so a payer with an old tip can invalidate even a
    // newly created invoice. Establish agreement before requesting invoices.
    let height = session
        .json(
            "chain",
            "bark-funded-chain-tip",
            &format!("{} getblockcount", native::BITCOIN_ROOT),
        )?
        .as_u64()
        .context("Bitcoin chain height is missing")?;
    for (id, cli) in [("cln", CLN), ("peer", PEER)] {
        let synced = session.poll(
            id,
            &format!("bark-{id}-synced"),
            &format!("{cli} getinfo"),
            |v| {
                Ok((v["blockheight"].as_u64() == Some(height)
                    && v["warning_bitcoind_sync"].is_null()
                    && v["warning_lightningd_sync"].is_null())
                .then(|| v.clone()))
            },
        )?;
        context.record(&format!("bark-{id}-synced.json"), &synced)?;
    }
    let funded = session.poll("ark", "bark-server-funded", &format!("{ARK} wallet"), |v| {
        Ok((expect::integer(v, "/rounds/trusted_balance")? > 1_000_000).then(|| v.clone()))
    })?;
    context.record("bark-funded-server.json", &funded)
}
