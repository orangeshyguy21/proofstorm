use super::common::{assert_handle, scoped, submit_idempotent};
use crate::{McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::json;

pub(super) fn run(client: &mut McpClient, bootstrap_channel_id: &str) -> Result<()> {
    // --- peer, channel, wallet ---------------------------------------------
    submit_idempotent(
        client,
        "peer_connect",
        scoped(
            "peer-connect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-connect-slice5"}),
        ),
        "peer",
    )?;
    let peer = cell::wait_operation(client, "peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&peer)?, "/connected")? {
        bail!("peer-connect artifact is invalid: {peer}");
    }

    submit_idempotent(
        client,
        "channel_open",
        scoped(
            "channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 2_000_000, "push_sat": 0, "idempotency_key": "channel-open-slice5"}),
        ),
        "channel",
    )?;
    let channel = cell::wait_operation(client, "channel-open", 120)?;
    let channel_content = cell::artifact_content(&channel)?;
    if !expect::boolean(channel_content, "/active")? {
        bail!("channel-open artifact is invalid: {channel}");
    }
    let channel_id = assert_handle(channel_content, "channel open")?;

    // --- CLN interoperability and rebalance --------------------------------
    client.call(
        "peer_connect",
        scoped(
            "cln-peer-connect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-connect-slice5"}),
        ),
    )?;
    let cln_peer = cell::wait_operation(client, "cln-peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_peer)?, "/connected")? {
        bail!("CLN to LND peer connection artifact is invalid: {cln_peer}");
    }

    client.call(
        "channel_open",
        scoped(
            "cln-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000, "idempotency_key": "cln-channel-open-slice5"}),
        ),
    )?;
    let cln_channel = cell::wait_operation(client, "cln-channel-open", 120)?;
    let cln_channel_id =
        assert_handle(cell::artifact_content(&cln_channel)?, "LND to CLN channel")?;

    client.call(
        "peer_connect",
        scoped(
            "rebalance-bridge-peer-connect",
            json!({"from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "idempotency_key": "rebalance-bridge-peer-connect-slice5"}),
        ),
    )?;
    let bridge_peer = cell::wait_operation(client, "rebalance-bridge-peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&bridge_peer)?, "/connected")? {
        bail!("rebalance bridge peer artifact is invalid: {bridge_peer}");
    }

    client.call(
        "channel_open",
        scoped(
            "rebalance-bridge-channel-open",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 0, "idempotency_key": "rebalance-bridge-channel-open-slice5"}),
        ),
    )?;
    let bridge_channel = cell::wait_operation(client, "rebalance-bridge-channel-open", 120)?;
    let bridge_channel_id =
        assert_handle(cell::artifact_content(&bridge_channel)?, "rebalance bridge")?;

    submit_idempotent(
        client,
        "channel_rebalance",
        scoped(
            "channel-rebalance",
            json!({"lightning": "mint-lnd", "outgoing_channel_id": channel_id, "incoming_channel_id": cln_channel_id, "amount_sat": 100_000, "max_fee_sat": 100, "idempotency_key": "channel-rebalance-slice5"}),
        ),
        "channel rebalance",
    )?;
    let rebalanced = cell::wait_operation(client, "channel-rebalance", 120)?;
    let rebalance_content = cell::artifact_content(&rebalanced)?;
    if !expect::boolean(rebalance_content, "/rebalanced")?
        || expect::integer(rebalance_content, "/amount_sat")? != 100_000
        || expect::integer(rebalance_content, "/fee_sat")? > 100
        || expect::string(rebalance_content, "/outgoing_channel_id")? != channel_id
        || expect::string(rebalance_content, "/incoming_channel_id")? != cln_channel_id
        || expect::integer(rebalance_content, "/outgoing_local_before_sat")?
            <= expect::integer(rebalance_content, "/outgoing_local_after_sat")?
        || expect::integer(rebalance_content, "/incoming_local_before_sat")?
            >= expect::integer(rebalance_content, "/incoming_local_after_sat")?
    {
        bail!("channel rebalance artifact is invalid: {rebalanced}");
    }

    // --- topology teardown --------------------------------------------------
    client.call(
        "channel_close",
        scoped(
            "rebalance-bridge-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_id": bridge_channel_id, "idempotency_key": "rebalance-bridge-channel-close-slice5"}),
        ),
    )?;
    let bridge_closed = cell::wait_operation(client, "rebalance-bridge-channel-close", 120)?;
    if expect::string(cell::artifact_content(&bridge_closed)?, "/channel_id")? != bridge_channel_id
    {
        bail!("rebalance bridge close artifact is invalid: {bridge_closed}");
    }

    submit_idempotent(
        client,
        "channel_close",
        scoped(
            "channel-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": channel_id, "idempotency_key": "channel-close-slice5"}),
        ),
        "channel close",
    )?;
    let closed = cell::wait_operation(client, "channel-close", 120)?;
    let closed_content = cell::artifact_content(&closed)?;
    if !expect::boolean(closed_content, "/closed")?
        || !expect::boolean(closed_content, "/confirmed")?
        || expect::boolean(closed_content, "/force")?
        || expect::boolean(closed_content, "/pending_resolution")?
        || expect::string(closed_content, "/channel_id")? != channel_id
    {
        bail!("cooperative channel close artifact is invalid: {closed}");
    }

    client.call(
        "channel_close",
        scoped(
            "bootstrap-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "mint-lnd", "channel_id": bootstrap_channel_id, "idempotency_key": "bootstrap-channel-close-slice5"}),
        ),
    )?;
    let bootstrap_closed = cell::wait_operation(client, "bootstrap-channel-close", 120)?;
    if expect::string(cell::artifact_content(&bootstrap_closed)?, "/channel_id")?
        != bootstrap_channel_id
    {
        bail!("bootstrap channel close artifact is invalid: {bootstrap_closed}");
    }

    submit_idempotent(
        client,
        "peer_disconnect",
        scoped(
            "peer-disconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-disconnect-slice5"}),
        ),
        "peer disconnect",
    )?;
    let disconnected = cell::wait_operation(client, "peer-disconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&disconnected)?, "/disconnected")? {
        bail!("peer disconnect artifact is invalid: {disconnected}");
    }

    client.call(
        "peer_connect",
        scoped(
            "peer-reconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "idempotency_key": "peer-reconnect-slice5"}),
        ),
    )?;
    let reconnected = cell::wait_operation(client, "peer-reconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&reconnected)?, "/connected")? {
        bail!("peer reconnect artifact is invalid: {reconnected}");
    }

    client.call(
        "channel_open",
        scoped(
            "force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 1_000_000, "push_sat": 0, "idempotency_key": "force-channel-open-slice5"}),
        ),
    )?;
    let force_channel = cell::wait_operation(client, "force-channel-open", 120)?;
    let force_channel_id = assert_handle(
        cell::artifact_content(&force_channel)?,
        "force-close target",
    )?;

    client.call(
        "channel_force_close",
        scoped(
            "channel-force-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": force_channel_id, "idempotency_key": "channel-force-close-slice5"}),
        ),
    )?;
    let force_closed = cell::wait_operation(client, "channel-force-close", 120)?;
    let force_content = cell::artifact_content(&force_closed)?;
    if !expect::boolean(force_content, "/closed")?
        || !expect::boolean(force_content, "/confirmed")?
        || !expect::boolean(force_content, "/force")?
        || !expect::boolean(force_content, "/pending_resolution")?
        || expect::string(force_content, "/channel_id")? != force_channel_id
    {
        bail!("force channel close artifact is invalid: {force_closed}");
    }

    client.call(
        "channel_close",
        scoped(
            "cln-channel-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_channel_id, "idempotency_key": "cln-channel-close-slice5"}),
        ),
    )?;
    let cln_closed = cell::wait_operation(client, "cln-channel-close", 120)?;
    let cln_closed_content = cell::artifact_content(&cln_closed)?;
    if !expect::boolean(cln_closed_content, "/closed")?
        || !expect::boolean(cln_closed_content, "/confirmed")?
        || expect::boolean(cln_closed_content, "/force")?
        || expect::boolean(cln_closed_content, "/pending_resolution")?
        || expect::string(cln_closed_content, "/channel_id")? != cln_channel_id
    {
        bail!("CLN cooperative close artifact is invalid: {cln_closed}");
    }

    client.call(
        "peer_disconnect",
        scoped(
            "cln-peer-disconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-disconnect-slice5"}),
        ),
    )?;
    let cln_disconnected = cell::wait_operation(client, "cln-peer-disconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_disconnected)?, "/disconnected")? {
        bail!("CLN to LND disconnect artifact is invalid: {cln_disconnected}");
    }

    client.call(
        "peer_connect",
        scoped(
            "cln-peer-reconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "idempotency_key": "cln-peer-reconnect-slice5"}),
        ),
    )?;
    let cln_reconnected = cell::wait_operation(client, "cln-peer-reconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_reconnected)?, "/connected")? {
        bail!("CLN to LND reconnect artifact is invalid: {cln_reconnected}");
    }

    client.call(
        "channel_open",
        scoped(
            "cln-force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000, "idempotency_key": "cln-force-channel-open-slice5"}),
        ),
    )?;
    let cln_force_channel = cell::wait_operation(client, "cln-force-channel-open", 120)?;
    let cln_force_channel_id = assert_handle(
        cell::artifact_content(&cln_force_channel)?,
        "CLN force-close target",
    )?;

    client.call(
        "channel_force_close",
        scoped(
            "cln-channel-force-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_force_channel_id, "idempotency_key": "cln-channel-force-close-slice5"}),
        ),
    )?;
    let cln_force_closed = cell::wait_operation(client, "cln-channel-force-close", 120)?;
    let cln_force_content = cell::artifact_content(&cln_force_closed)?;
    if !expect::boolean(cln_force_content, "/closed")?
        || !expect::boolean(cln_force_content, "/confirmed")?
        || !expect::boolean(cln_force_content, "/force")?
        || !expect::boolean(cln_force_content, "/pending_resolution")?
        || expect::string(cln_force_content, "/channel_id")? != cln_force_channel_id
    {
        bail!("CLN force-close artifact is invalid: {cln_force_closed}");
    }

    Ok(())
}
