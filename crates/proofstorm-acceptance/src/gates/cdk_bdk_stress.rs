//! CDK 0.18.1 embedded BDK: full agent-authored metadata rendering, 24
//! concurrent NUT-30 on-chain quotes with unique addresses, NUT-20 pubkey
//! enforcement, dust rejection, and settled-state survival across a restart.
//!
//! Ported from `tests/kubernetes/cdk_bdk_stress_mcp_client.py`. Serves both the
//! `cdk-bdk-stress` and `cdk-bdk-postgres` targets.

use std::{thread::sleep, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, http, json as expect, postgres};

const INSTANCE: &str = "cdk-bdk-instance";
const DATABASE: &str = "proofstorm_bdk";
const MARKER: &str = "bdk-persistent";
const IMAGE: &str = proofstorm_core::CDK_MINT_IMAGE;
const PUBKEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const QUOTES: usize = 24;

fn cell_document(postgres_enabled: bool) -> Value {
    let mut cell = json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-bdk-stress-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {"txindex": true, "fallback_fee": 0.0002}},
            {
                "id": "mint", "kind": "mint", "implementation": "cdk-bdk", "version": "0.18.1",
                "config_version": "cdk-mintd-bdk/0.18/v1", "control": "target",
                "config": {
                    "name": "Proofstorm CDK BDK",
                    "description": "Native CDK embedded-BDK NUT-30 stress cell",
                    "description_long": "Agent-authored long-form CDK metadata",
                    "motd": "Proofstorm agents welcome",
                    "icon_url": "https://proofstorm.invalid/cdk-bdk.png",
                    "contact_email": "operator@proofstorm.invalid",
                    "contact_nostr_public_key": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
                    "tos_url": "https://proofstorm.invalid/terms",
                    "enable_info_page": true,
                    "input_fee_ppk": 321,
                    "use_keyset_v2": false,
                    "mint_quote_ttl_seconds": 900,
                    "melt_quote_ttl_seconds": 180,
                    "http_cache_ttl_seconds": 75,
                    "http_cache_tti_seconds": 45,
                    "max_inputs": 64,
                    "max_outputs": 96,
                    "min_mint_sat": 1200,
                    "max_mint_sat": 5000,
                    "min_melt_sat": 1300,
                    "max_melt_sat": 6000
                }
            }
        ],
        "links": [
            {"id": "mint-chain", "kind": "chain_backend", "from": "mint", "to": "chain", "binding": {"type": "chain", "network": "regtest"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    });
    postgres::augment_cell(postgres_enabled, &mut cell, DATABASE);
    cell
}

const CONFIG_FRAGMENTS: &[&str] = &[
    "enable_info_page = true",
    "input_fee_ppk = 321",
    "use_keyset_v2 = false",
    "mint_ttl = 900",
    "melt_ttl = 180",
    "backend = \"memory\"",
    "ttl = 75",
    "tti = 45",
    "description_long = \"Agent-authored long-form CDK metadata\"",
    "motd = \"Proofstorm agents welcome\"",
    "icon_url = \"https://proofstorm.invalid/cdk-bdk.png\"",
    "contact_email = \"operator@proofstorm.invalid\"",
    "contact_nostr_public_key = \"79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798\"",
    "tos_url = \"https://proofstorm.invalid/terms\"",
    "max_inputs = 64",
    "max_outputs = 96",
    "[payment_backend]\nbackend = \"none\"",
    "min_mint = 1200",
    "max_mint = 5000",
    "min_melt = 1300",
    "max_melt = 6000",
    "onchain_backend = \"bdk\"",
    "min_receive_amount_sat = 1200",
    "chain_source_type = \"bitcoinrpc\"",
    "bitcoind_rpc_host = \"chain\"",
    "num_confs = 1",
];

const INFO_FRAGMENTS: &[&str] = &[
    "Agent-authored long-form CDK metadata",
    "Proofstorm agents welcome",
    "https://proofstorm.invalid/cdk-bdk.png",
    "operator@proofstorm.invalid",
    "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
    "https://proofstorm.invalid/terms",
];

fn bitcoin(context: &GateContext, namespace: &str, arguments: &[&str]) -> Result<String> {
    let mut argv = vec![
        "bitcoin-cli",
        "-regtest",
        "-rpcuser=proofstorm",
        "-rpcpassword=proofstorm-regtest-only",
    ];
    argv.extend_from_slice(arguments);
    context.kubectl.exec(namespace, "statefulset/chain", &argv)
}

pub fn run(context: &GateContext, postgres_enabled: bool) -> Result<()> {
    let client = context.default_session("cdk-bdk-stress-live", "designer")?;
    run_selected(
        context,
        client,
        postgres_enabled,
        context.selected_version("cdk-bdk", "0.18.1"),
        context.selected_image("cdk-bdk", IMAGE),
    )
}

pub(super) fn run_candidate(
    context: &GateContext,
    client: crate::McpClient,
    receipt: &Value,
) -> Result<()> {
    run_selected(
        context,
        client,
        false,
        expect::string(receipt, "/catalog_entry/version")?,
        expect::string(receipt, "/image")?,
    )
}

fn run_selected(
    context: &GateContext,
    mut client: crate::McpClient,
    postgres_enabled: bool,
    selected_version: &str,
    selected_image: &str,
) -> Result<()> {
    let mut document = context.document(cell_document(postgres_enabled))?;
    document["components"][1]["version"] = json!(selected_version);

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":document,"request_id":"create-cdk-bdk"}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;
    let entry = cell::lock_entry(&published, "cdk-bdk")?;
    if expect::string(entry, "/version")? != selected_version
        || expect::string(entry, "/image")? != selected_image
    {
        bail!("unexpected CDK-BDK lock: {entry}");
    }
    context.record("cdk-bdk-selected-plan.json", &published)?;

    crate::cell::apply(&mut client, &preview)?;
    let ready = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;
    if selected_version.starts_with("candidate-") {
        super::candidates::pod_image(context, namespace, selected_image)?;
    }
    context.record("cdk-bdk-selected-ready.json", &ready)?;

    let config = context.kubectl.exec(
        namespace,
        "deployment/mint",
        &["cat", "/config/config.toml"],
    )?;
    for fragment in CONFIG_FRAGMENTS {
        if !config.contains(fragment) {
            bail!("mint configuration is missing {fragment:?}: {config}");
        }
    }
    if config.contains("[lnd]") || config.contains("[cln]") || config.contains("[ldk_node]") {
        bail!("on-chain-only mint rendered a Lightning backend");
    }
    postgres::assert_materialized(
        postgres_enabled,
        &context.kubectl,
        namespace,
        &config,
        DATABASE,
    )?;

    let version =
        context
            .kubectl
            .exec(namespace, "deployment/mint", &["cdk-mintd", "--version"])?;
    if !version.contains("0.18.1") {
        bail!("live mint reports the wrong version: {version:?}");
    }

    bitcoin(context, namespace, &["createwallet", "default"])?;
    let miner = bitcoin(context, namespace, &["-rpcwallet=default", "getnewaddress"])?;
    bitcoin(
        context,
        namespace,
        &["-rpcwallet=default", "generatetoaddress", "101", &miner],
    )?;

    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let info = http::get_json_retrying(&mut forward, "/v1/info", 30)?;
    let info_text = serde_json::to_string(&info)?;
    if !info_text.to_lowercase().contains("onchain") {
        bail!("live mint does not advertise on-chain support: {info}");
    }
    for fragment in INFO_FRAGMENTS {
        if !info_text.contains(fragment) {
            bail!("live NUT-06 mint info is missing {fragment:?}: {info}");
        }
    }

    let quote_url = forward.url("/v1/mint/quote/onchain");
    let quotes: Vec<Value> = std::thread::scope(|scope| -> Result<Vec<Value>> {
        let handles: Vec<_> = (0..QUOTES)
            .map(|_| {
                let url = quote_url.clone();
                scope
                    .spawn(move || http::post_json(&url, &json!({"unit": "sat", "pubkey": PUBKEY})))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("quote thread panicked"))?
            })
            .collect()
    })?;

    context.record("cdk-bdk-selected-quotes.json", &json!(quotes))?;

    let mut addresses: Vec<&str> = quotes
        .iter()
        .map(|quote| expect::string(quote, "/request"))
        .collect::<Result<_>>()?;
    let unique: std::collections::BTreeSet<&&str> = addresses.iter().collect();
    if unique.len() != QUOTES || !addresses.iter().all(|address| address.starts_with("bcrt1")) {
        bail!("concurrent NUT-30 quotes did not return unique regtest addresses: {addresses:?}");
    }
    addresses.clear();

    let refused = http::post_status(&quote_url, &json!({"unit": "sat"}))?;
    if refused != 400 {
        bail!("on-chain quote without a NUT-20 pubkey was accepted with status {refused}");
    }

    let funded = &quotes[..3];
    let dust = &quotes[3];
    for quote in funded {
        bitcoin(
            context,
            namespace,
            &[
                "-rpcwallet=default",
                "sendtoaddress",
                expect::string(quote, "/request")?,
                "0.00001200",
            ],
        )?;
    }
    bitcoin(
        context,
        namespace,
        &[
            "-rpcwallet=default",
            "sendtoaddress",
            expect::string(dust, "/request")?,
            "0.00001199",
        ],
    )?;
    bitcoin(
        context,
        namespace,
        &["-rpcwallet=default", "generatetoaddress", "1", &miner],
    )?;

    let status_of = |quote: &Value, forward: &http::PortForward| -> Result<Value> {
        http::get_json(&forward.url(&format!(
            "/v1/mint/quote/onchain/{}",
            expect::string(quote, "/quote")?
        )))
    };

    let mut settled = Vec::new();
    let mut ok = false;
    for _ in 0..60 {
        settled = funded
            .iter()
            .map(|quote| status_of(quote, &forward))
            .collect::<Result<_>>()?;
        if settled
            .iter()
            .all(|item| item.get("amount_paid").and_then(Value::as_u64).unwrap_or(0) >= 1200)
        {
            ok = true;
            break;
        }
        sleep(Duration::from_secs(1));
    }
    if !ok {
        bail!("funded on-chain quotes did not settle after one confirmation: {settled:?}");
    }

    let dust_status = status_of(dust, &forward)?;
    if dust_status
        .get("amount_paid")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        != 0
    {
        bail!("sub-minimum on-chain deposit was credited: {dust_status}");
    }
    context.record(
        "cdk-bdk-selected-settlement.json",
        &json!({"funded":settled,"dust":dust_status,"missing_pubkey_status":refused}),
    )?;

    if postgres_enabled {
        postgres::seed_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;
        postgres::restart_database(postgres_enabled, &context.kubectl, namespace)?;
    }
    context
        .kubectl
        .rollout_restart(namespace, "deployment/mint")?;
    // kubectl binds a service forward to one pod; the old tunnel cannot follow
    // the replacement pod after a rollout.
    drop(forward);
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    http::get_json_retrying(&mut forward, "/v1/info", 30)?;

    let mut survived = false;
    for _ in 0..30 {
        let persisted: Result<Vec<Value>> = funded
            .iter()
            .map(|quote| status_of(quote, &forward))
            .collect();
        if let Ok(items) = persisted {
            if items
                .iter()
                .all(|item| item.get("amount_paid").and_then(Value::as_u64).unwrap_or(0) >= 1200)
            {
                context.record("cdk-bdk-selected-after-restart.json", &json!(items))?;
                survived = true;
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
    if !survived {
        bail!("settled quote state did not survive mint restart");
    }
    postgres::verify_sentinel(postgres_enabled, &context.kubectl, namespace, MARKER)?;

    drop(forward);

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;

    if postgres_enabled {
        println!("CDK embedded BDK + PostgreSQL MCP NUT-30 persistence and teardown passed");
    } else {
        println!(
            "CDK 0.18.1 embedded-BDK database-backed configuration, NUT-30 stress, restart persistence, and teardown passed"
        );
    }
    Ok(())
}
