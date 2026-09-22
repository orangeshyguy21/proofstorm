//! Channel lifecycle coverage through component-native commands and receipts.
use super::common::{EXPERIMENT, INSTANCE};
use crate::{
    McpClient, json as expect,
    native::{self, LND, Session, quote},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

const CLN: &str = "lightning-cli --lightning-dir=/home/cln/.lightning --network=regtest --json --notifications=none";

pub(super) fn run(client: &mut McpClient, bootstrap_point: &str) -> Result<Vec<String>> {
    let mut native = Session::new(client, INSTANCE, EXPERIMENT);
    let mint = native.json("mint-lnd", "mint-identity", &format!("{LND} getinfo"))?;
    let payer = native.json("payer-lnd", "payer-identity", &format!("{LND} getinfo"))?;
    let cln = native.json("workspace-cln", "cln-identity", &format!("{CLN} getinfo"))?;
    let mint_key = expect::string(&mint, "/identity_pubkey")?;
    let payer_key = expect::string(&payer, "/identity_pubkey")?;
    let cln_key = expect::string(&cln, "/id")?;

    connect(
        &mut native,
        "mint-lnd",
        LND,
        "payer-lnd",
        payer_key,
        "peer-connect",
    )?;
    let point = open(&mut native, "mint-lnd", payer_key, "channel", 2_000_000, 0)?;
    connect(
        &mut native,
        "workspace-cln",
        CLN,
        "mint-lnd",
        mint_key,
        "cln-peer-connect",
    )?;
    let cln_point = open(
        &mut native,
        "mint-lnd",
        cln_key,
        "cln-channel",
        1_000_000,
        300_000,
    )?;
    connect(
        &mut native,
        "payer-lnd",
        LND,
        "workspace-cln",
        cln_key,
        "bridge-peer-connect",
    )?;
    let bridge = open(
        &mut native,
        "payer-lnd",
        cln_key,
        "bridge-channel",
        1_000_000,
        0,
    )?;

    rebalance(&mut native, &point, &cln_point)?;
    close_lnd(&mut native, "payer-lnd", "bridge-close", &bridge, false)?;
    close_lnd(&mut native, "mint-lnd", "channel-close", &point, false)?;
    close_lnd(
        &mut native,
        "payer-lnd",
        "bootstrap-close",
        bootstrap_point,
        false,
    )?;
    disconnect(
        &mut native,
        "mint-lnd",
        LND,
        "payer-lnd",
        LND,
        mint_key,
        payer_key,
        "peer-disconnect",
    )?;
    connect(
        &mut native,
        "mint-lnd",
        LND,
        "payer-lnd",
        payer_key,
        "peer-reconnect",
    )?;
    let force = open(
        &mut native,
        "mint-lnd",
        payer_key,
        "force-channel",
        1_000_000,
        0,
    )?;
    close_lnd(&mut native, "mint-lnd", "channel-force-close", &force, true)?;

    close_cln(&mut native, "cln-close", &cln_point, false)?;
    disconnect(
        &mut native,
        "workspace-cln",
        CLN,
        "mint-lnd",
        LND,
        cln_key,
        mint_key,
        "cln-peer-disconnect",
    )?;
    connect(
        &mut native,
        "workspace-cln",
        CLN,
        "mint-lnd",
        mint_key,
        "cln-peer-reconnect",
    )?;
    let force = open(
        &mut native,
        "mint-lnd",
        cln_key,
        "cln-force-channel",
        1_000_000,
        300_000,
    )?;
    close_cln(&mut native, "cln-force-close", &force, true)?;
    Ok(native.operations)
}

fn connect(
    native: &mut Session<'_>,
    from: &str,
    cli: &str,
    to: &str,
    pubkey: &str,
    id: &str,
) -> Result<()> {
    let command = if cli == LND {
        format!("{cli} connect {}", quote(&format!("{pubkey}@{to}:9735")))
    } else {
        format!("{cli} connect {} {} 9735", quote(pubkey), quote(to))
    };
    // Bootstrap may already have connected these endpoints. Observe the actual
    // peer state after the command instead of trusting an 'already connected' error.
    native.execute(from, id, &format!("{command} || true"))?;
    native.poll(
        from,
        &format!("{id}-observed"),
        &format!("{cli} listpeers"),
        |value| Ok(peer_connected(value, pubkey, cli == CLN)?.then_some(())),
    )
}

fn peer_connected(value: &Value, pubkey: &str, cln: bool) -> Result<bool> {
    Ok(expect::array(value, "/peers")?.iter().any(|peer| {
        peer[if cln { "id" } else { "pub_key" }] == pubkey && (!cln || peer["connected"] == true)
    }))
}

#[allow(clippy::too_many_arguments)]
fn disconnect(
    native: &mut Session<'_>,
    from: &str,
    from_cli: &str,
    to: &str,
    to_cli: &str,
    from_key: &str,
    to_key: &str,
    id: &str,
) -> Result<()> {
    for (component, cli, peer, side) in [
        (from, from_cli, to_key, "from"),
        (to, to_cli, from_key, "to"),
    ] {
        native.execute(
            component,
            &format!("{id}-{side}"),
            &format!("{cli} disconnect {} || true", quote(peer)),
        )?;
    }
    for (component, cli, peer, side) in [
        (from, from_cli, to_key, "from"),
        (to, to_cli, from_key, "to"),
    ] {
        native.poll(
            component,
            &format!("{id}-{side}-observed"),
            &format!("{cli} listpeers"),
            |value| Ok((!peer_connected(value, peer, cli == CLN)?).then_some(())),
        )?;
    }
    Ok(())
}

fn open(
    native: &mut Session<'_>,
    from: &str,
    pubkey: &str,
    id: &str,
    amount: u64,
    push: u64,
) -> Result<String> {
    let opened = native.json(
        from,
        &format!("{id}-open"),
        &format!(
            "{LND} openchannel --node_key={} --local_amt={amount} --push_amt={push}",
            quote(pubkey)
        ),
    )?;
    native.mine("chain", &format!("{id}-confirm"), 6)?;
    native.poll(
        from,
        &format!("{id}-active"),
        &format!("{LND} listchannels"),
        |channels| native::active_channel_point(&opened, channels),
    )
}

fn channel<'a>(snapshot: &'a Value, point: &str) -> Result<&'a Value> {
    let matches: Vec<_> = expect::array(snapshot, "/channels")?
        .iter()
        .filter(|channel| channel["channel_point"] == point)
        .collect();
    ensure!(
        matches.len() == 1,
        "expected one channel for {point}: {snapshot}"
    );
    Ok(matches[0])
}

// lncli encodes satoshi balances and short channel IDs as decimal strings.
fn decimal(value: &Value, pointer: &str) -> Result<u64> {
    Ok(expect::string(value, pointer)?.parse()?)
}

fn rebalance(native: &mut Session<'_>, outgoing: &str, incoming: &str) -> Result<()> {
    let before = native.json(
        "mint-lnd",
        "rebalance-before",
        &format!("{LND} listchannels"),
    )?;
    let out = channel(&before, outgoing)?;
    let incoming_channel = channel(&before, incoming)?;
    let scid = expect::string(out, "/scid")?;
    let last_hop = expect::string(incoming_channel, "/remote_pubkey")?;
    let invoice = native.projected(
        "mint-lnd",
        "rebalance-invoice",
        &format!("set -eu; umask 077; {LND} addinvoice --amt=100000 --private --expiry=120 > /tmp/proofstorm-rebalance-invoice.json; cat /tmp/proofstorm-rebalance-invoice.json"),
        &serde_json::json!({"mode":"lnd_invoice"}),
    )?;
    ensure!(
        invoice["amount_msat"] == 100_000_000 && invoice["currency"] == "bcrt",
        "unexpected rebalance invoice: {invoice}"
    );
    expect::string(&invoice, "/payment_request")?;
    // Keep the validated invoice inside the component. The recorded command
    // contains its local filename and field selector, never the invoice itself.
    let read_invoice = r#"set -eu; invoice=$(sed -n 's/.*"payment_request"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' /tmp/proofstorm-rebalance-invoice.json); test -n "$invoice""#;
    // Gossip can lag confirmation. Project the final document of lncli's JSON
    // stream and retry the same invoice only after a terminal routing failure.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    for attempt in 0.. {
        let payment = native.projected("mint-lnd", &format!("rebalance-pay-{attempt}"), &format!(
            "{read_invoice}; {LND} sendpayment --pay_req=\"$invoice\" --outgoing_chan_id={} --last_hop={} --fee_limit=100 --timeout=5s --max_parts=1 --allow_self_payment --force --json || true",
            quote(scid), quote(last_hop)), &serde_json::json!({"mode":"json_fields","fields":["status","failure_reason"]}))?;
        if payment["status"] == "SUCCEEDED" {
            break;
        }
        ensure!(
            payment["status"] == "FAILED"
                && matches!(
                    payment["failure_reason"].as_str(),
                    Some("FAILURE_REASON_NO_ROUTE" | "FAILURE_REASON_TIMEOUT")
                ),
            "unexpected rebalance outcome: {payment}"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "rebalance route did not settle: {payment}"
        );
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let after = native.json(
        "mint-lnd",
        "rebalance-after",
        &format!("{LND} listchannels"),
    )?;
    let spent = decimal(out, "/local_balance")?
        .checked_sub(decimal(channel(&after, outgoing)?, "/local_balance")?)
        .context("outgoing balance did not decrease")?;
    let received = decimal(channel(&after, incoming)?, "/local_balance")?
        .checked_sub(decimal(incoming_channel, "/local_balance")?)
        .context("incoming balance did not increase")?;
    ensure!(
        (100_000..=100_100).contains(&spent) && received == 100_000,
        "rebalance amounts differ: spent={spent}, received={received}"
    );
    Ok(())
}

fn close_lnd(
    native: &mut Session<'_>,
    from: &str,
    id: &str,
    point: &str,
    force: bool,
) -> Result<()> {
    let (txid, index) = point.split_once(':').context("invalid funding outpoint")?;
    native.execute(
        from,
        id,
        &format!(
            "{LND} closechannel --funding_txid={} --output_index={}{}",
            quote(txid),
            quote(index),
            if force { " --force" } else { "" }
        ),
    )?;
    native.mine("chain", &format!("{id}-confirm"), 6)?;
    native.poll(
        from,
        &format!("{id}-inactive"),
        &format!("{LND} listchannels"),
        |snapshot| {
            Ok((!expect::array(snapshot, "/channels")?
                .iter()
                .any(|channel| channel["channel_point"] == point))
            .then_some(()))
        },
    )?;
    native.poll(
        from,
        &format!("{id}-observed"),
        &format!(
            "{LND} {}",
            if force {
                "pendingchannels"
            } else {
                "closedchannels"
            }
        ),
        |snapshot| {
            let entries = expect::array(
                snapshot,
                if force {
                    "/pending_force_closing_channels"
                } else {
                    "/channels"
                },
            )?;
            Ok(entries
                .iter()
                .any(|entry| {
                    if force {
                        entry["channel"]["channel_point"] == point
                            && entry["closing_txid"]
                                .as_str()
                                .is_some_and(|id| !id.is_empty())
                    } else {
                        entry["channel_point"] == point
                            && entry["close_type"] == "COOPERATIVE_CLOSE"
                            && entry["close_height"]
                                .as_u64()
                                .is_some_and(|height| height > 0)
                    }
                })
                .then_some(()))
        },
    )
}

fn close_cln(native: &mut Session<'_>, id: &str, point: &str, force: bool) -> Result<()> {
    // Closed channels retain extensive history. Ask CLN for the identity and
    // state fields this assertion needs so native output remains bounded.
    let list_channels = format!(
        "{CLN} --filter={} listpeerchannels",
        quote(
            r#"{"channels":[{"channel_id":true,"funding_txid":true,"funding_outnum":true,"peer_id":true,"state":true}]}"#
        )
    );
    let channels = native.json("workspace-cln", &format!("{id}-before"), &list_channels)?;
    let channel = expect::array(&channels, "/channels")?
        .iter()
        .find(|channel| {
            channel["funding_txid"]
                .as_str()
                .zip(channel["funding_outnum"].as_u64())
                .is_some_and(|(txid, index)| format!("{txid}:{index}") == point)
        })
        .context("CLN funding outpoint missing")?;
    let channel_id = expect::string(channel, "/channel_id")?;
    // Disconnect and close within one supervised command so polling latency
    // does not give the peer time to reconnect before unilateral negotiation.
    let disconnect = if force {
        format!(
            "set -eu; {CLN} disconnect {} true >/dev/null; ",
            quote(expect::string(channel, "/peer_id")?)
        )
    } else {
        String::new()
    };
    native.start(
        "workspace-cln",
        id,
        &format!(
            "{disconnect}{CLN} close {} {}",
            quote(channel_id),
            u8::from(force)
        ),
    )?;
    // The CLN close call may wait for confirmation; it must run concurrently
    // with mining rather than detach a process outside native supervision.
    for round in 0..60 {
        native.mine("chain", &format!("{id}-mine-{round}"), 1)?;
        let status = native
            .client
            .call("operation_status", serde_json::json!({"operation_id":id}))?;
        match status["phase"].as_str() {
            Some("succeeded" | "failed" | "cancelled") => break,
            _ => std::thread::sleep(std::time::Duration::from_secs(1)),
        }
    }
    let closed = native::wait(native.client, id)?;
    let closed: Value = serde_json::from_str(expect::string(&closed, "/stdout")?)
        .with_context(|| format!("native close {id} did not return JSON"))?;
    let txid = closing_txid(&closed, force)?;
    native.mine("chain", &format!("{id}-confirm"), 6)?;
    let transaction = native.json(
        "chain",
        &format!("{id}-transaction"),
        &format!(
            "{} getrawtransaction {} true",
            native::BITCOIN_ROOT,
            quote(txid)
        ),
    )?;
    ensure!(
        expect::integer(&transaction, "/confirmations")? >= 6,
        "CLN closing transaction is unconfirmed"
    );
    ensure!(
        expect::array(&transaction, "/vin")?.iter().any(|input| {
            input["txid"]
                .as_str()
                .zip(input["vout"].as_u64())
                .is_some_and(|(txid, index)| format!("{txid}:{index}") == point)
        }),
        "CLN close did not spend the selected funding outpoint"
    );
    native.poll(
        "workspace-cln",
        &format!("{id}-observed"),
        &list_channels,
        |snapshot| {
            let channel = expect::array(snapshot, "/channels")?
                .iter()
                .find(|entry| entry["channel_id"] == channel_id);
            Ok(channel
                .is_none_or(|entry| {
                    matches!(
                        entry["state"].as_str(),
                        Some("FUNDING_SPEND_SEEN" | "ONCHAIN" | "CLOSINGD_COMPLETE")
                    )
                })
                .then_some(()))
        },
    )
}

fn closing_txid(closed: &Value, force: bool) -> Result<&str> {
    ensure!(
        closed["type"] == if force { "unilateral" } else { "mutual" },
        "unexpected CLN close: {closed}"
    );
    // CLN >=24.11 returns txids, including multiple candidates for spliced
    // channels. This fixture opens one funding transaction, without splices.
    // https://docs.corelightning.org/reference/close
    let txids = expect::array(closed, "/txids")?;
    ensure!(
        txids.len() == 1,
        "expected one unspliced closing transaction: {closed}"
    );
    let txid = txids[0]
        .as_str()
        .context("CLN closing transaction ID is missing")?;
    ensure!(
        txid.len() == 64 && txid.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid CLN closing transaction ID"
    );
    Ok(txid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cln_close_uses_native_txids_and_refuses_wrong_or_ambiguous_outcomes() {
        let txid = "a".repeat(64);
        assert_eq!(
            closing_txid(&json!({"type":"mutual","txids":[txid]}), false).unwrap(),
            txid
        );
        assert!(closing_txid(&json!({"type":"mutual","txids":[txid]}), true).is_err());
        assert!(closing_txid(&json!({"type":"unilateral","txid":txid}), true).is_err());
        assert!(closing_txid(&json!({"type":"unilateral","txids":[txid,txid]}), true).is_err());
    }

    #[test]
    fn native_channel_selection_does_not_confuse_parallel_channels() {
        let snapshot = json!({"channels":[{"channel_point":"a:1"},{"channel_point":"a:3"}]});
        assert_eq!(channel(&snapshot, "a:3").unwrap()["channel_point"], "a:3");
        assert!(channel(&snapshot, "a:0").is_err());
        assert!(
            channel(
                &json!({"channels":[{"channel_point":"a:1"},{"channel_point":"a:1"}]}),
                "a:1"
            )
            .is_err()
        );
    }

    #[test]
    fn disconnected_cln_peer_records_do_not_count_as_connections() {
        assert!(
            !peer_connected(
                &json!({"peers":[{"id":"peer","connected":false}]}),
                "peer",
                true
            )
            .unwrap()
        );
        assert!(peer_connected(&json!({"peers":[{"pub_key":"peer"}]}), "peer", false).unwrap());
        assert!(peer_connected(&json!({}), "peer", false).is_err());
    }
}
