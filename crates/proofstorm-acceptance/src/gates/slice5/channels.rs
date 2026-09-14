use super::common::{assert_handle, scoped, submit_idempotent};
use crate::{McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::json;

pub(super) fn run(
    context: &crate::GateContext,
    client: &mut McpClient,
    bootstrap_channel_id: &str,
) -> Result<()> {
    // --- peer, channel, wallet ---------------------------------------------
    submit_idempotent(
        context,
        client,
        crate::driver::peer_connect,
        scoped(
            "peer-connect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd"}),
        ),
        "peer",
    )?;
    let peer = cell::wait_operation(client, "peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&peer)?, "/connected")? {
        bail!("peer-connect artifact is invalid: {peer}");
    }

    submit_idempotent(
        context,
        client,
        crate::driver::channel_open,
        scoped(
            "channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 2_000_000, "push_sat": 0}),
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
    crate::driver::peer_connect(
        context,
        client,
        scoped(
            "cln-peer-connect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd"}),
        ),
    )?;
    let cln_peer = cell::wait_operation(client, "cln-peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_peer)?, "/connected")? {
        bail!("CLN to LND peer connection artifact is invalid: {cln_peer}");
    }

    crate::driver::channel_open(
        context,
        client,
        scoped(
            "cln-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000}),
        ),
    )?;
    let cln_channel = cell::wait_operation(client, "cln-channel-open", 120)?;
    let cln_channel_id =
        assert_handle(cell::artifact_content(&cln_channel)?, "LND to CLN channel")?;

    crate::driver::peer_connect(
        context,
        client,
        scoped(
            "rebalance-bridge-peer-connect",
            json!({"from_lightning": "payer-lnd", "to_lightning": "attacker-cln"}),
        ),
    )?;
    let bridge_peer = cell::wait_operation(client, "rebalance-bridge-peer-connect", 120)?;
    if !expect::boolean(cell::artifact_content(&bridge_peer)?, "/connected")? {
        bail!("rebalance bridge peer artifact is invalid: {bridge_peer}");
    }

    crate::driver::channel_open(
        context,
        client,
        scoped(
            "rebalance-bridge-channel-open",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 0}),
        ),
    )?;
    let bridge_channel = cell::wait_operation(client, "rebalance-bridge-channel-open", 120)?;
    let bridge_channel_id =
        assert_handle(cell::artifact_content(&bridge_channel)?, "rebalance bridge")?;

    submit_idempotent(
        context,
        client,
        crate::driver::channel_rebalance,
        scoped(
            "channel-rebalance",
            json!({"lightning": "mint-lnd", "outgoing_channel_id": channel_id, "incoming_channel_id": cln_channel_id, "amount_sat": 100_000, "max_fee_sat": 100}),
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
    crate::driver::channel_close(
        context,
        client,
        scoped(
            "rebalance-bridge-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "attacker-cln", "channel_id": bridge_channel_id}),
        ),
    )?;
    let bridge_closed = cell::wait_operation(client, "rebalance-bridge-channel-close", 120)?;
    if expect::string(cell::artifact_content(&bridge_closed)?, "/channel_id")? != bridge_channel_id
    {
        bail!("rebalance bridge close artifact is invalid: {bridge_closed}");
    }

    submit_idempotent(
        context,
        client,
        crate::driver::channel_close,
        scoped(
            "channel-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": channel_id}),
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

    crate::driver::channel_close(
        context,
        client,
        scoped(
            "bootstrap-channel-close",
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "mint-lnd", "channel_id": bootstrap_channel_id}),
        ),
    )?;
    let bootstrap_closed = cell::wait_operation(client, "bootstrap-channel-close", 120)?;
    if expect::string(cell::artifact_content(&bootstrap_closed)?, "/channel_id")?
        != bootstrap_channel_id
    {
        bail!("bootstrap channel close artifact is invalid: {bootstrap_closed}");
    }

    submit_idempotent(
        context,
        client,
        crate::driver::peer_disconnect,
        scoped(
            "peer-disconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd"}),
        ),
        "peer disconnect",
    )?;
    let disconnected = cell::wait_operation(client, "peer-disconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&disconnected)?, "/disconnected")? {
        bail!("peer disconnect artifact is invalid: {disconnected}");
    }

    crate::driver::peer_connect(
        context,
        client,
        scoped(
            "peer-reconnect",
            json!({"from_lightning": "mint-lnd", "to_lightning": "payer-lnd"}),
        ),
    )?;
    let reconnected = cell::wait_operation(client, "peer-reconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&reconnected)?, "/connected")? {
        bail!("peer reconnect artifact is invalid: {reconnected}");
    }

    crate::driver::channel_open(
        context,
        client,
        scoped(
            "force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_sat": 1_000_000, "push_sat": 0}),
        ),
    )?;
    let force_channel = cell::wait_operation(client, "force-channel-open", 120)?;
    let force_channel_id = assert_handle(
        cell::artifact_content(&force_channel)?,
        "force-close target",
    )?;

    crate::driver::channel_force_close(
        context,
        client,
        scoped(
            "channel-force-close",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "payer-lnd", "channel_id": force_channel_id}),
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

    crate::driver::channel_close(
        context,
        client,
        scoped(
            "cln-channel-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_channel_id}),
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

    crate::driver::peer_disconnect(
        context,
        client,
        scoped(
            "cln-peer-disconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd"}),
        ),
    )?;
    let cln_disconnected = cell::wait_operation(client, "cln-peer-disconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_disconnected)?, "/disconnected")? {
        bail!("CLN to LND disconnect artifact is invalid: {cln_disconnected}");
    }

    crate::driver::peer_connect(
        context,
        client,
        scoped(
            "cln-peer-reconnect",
            json!({"from_lightning": "attacker-cln", "to_lightning": "mint-lnd"}),
        ),
    )?;
    let cln_reconnected = cell::wait_operation(client, "cln-peer-reconnect", 120)?;
    if !expect::boolean(cell::artifact_content(&cln_reconnected)?, "/connected")? {
        bail!("CLN to LND reconnect artifact is invalid: {cln_reconnected}");
    }

    crate::driver::channel_open(
        context,
        client,
        scoped(
            "cln-force-channel-open",
            json!({"chain": "chain", "from_lightning": "mint-lnd", "to_lightning": "attacker-cln", "channel_sat": 1_000_000, "push_sat": 300_000}),
        ),
    )?;
    let cln_force_channel = cell::wait_operation(client, "cln-force-channel-open", 120)?;
    let cln_force_channel_id = assert_handle(
        cell::artifact_content(&cln_force_channel)?,
        "CLN force-close target",
    )?;

    crate::driver::channel_force_close(
        context,
        client,
        scoped(
            "cln-channel-force-close",
            json!({"chain": "chain", "from_lightning": "attacker-cln", "to_lightning": "mint-lnd", "channel_id": cln_force_channel_id}),
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
