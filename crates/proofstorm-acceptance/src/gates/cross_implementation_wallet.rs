//! CDK and Redis-backed Nutshell run the identical wallet workflow: NUT-20
//! interoperability, Redis cache use, secret stability across a controller
//! restart, ephemeral cache loss across a cache restart, recovery, and
//! verified teardown.
//!
//! Ported from `tests/kubernetes/cross_implementation_wallet_mcp_client.py`.

use std::{fs, thread::sleep, time::Duration};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "cross-mint-wallet-instance";
const EXPERIMENT: &str = "cross-mint-experiment";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cross-mint-wallet-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cross-mint"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cross-payer"}},
            {"id": "cache", "kind": "database", "implementation": "redis", "version": "8.10.1", "config_version": "redis/8.10/v1", "control": "cell", "config": {"maxmemory_mb": 64}},
            {"id": "cdk-mint", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK Cross-Parity", "description": "Cross-implementation wallet acceptance"}},
            {"id": "nutshell-mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell Cross-Parity", "description": "Cross-implementation wallet acceptance", "redis_cache_ttl_seconds": 900}},
            {"id": "cdk-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "cdk-recipient", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "nutshell-recipient", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}},
            {"id": "nutshell-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
        ],
        "links": [
            {"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "cdk-bolt11", "kind": "payment_backend", "from": "cdk-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "nutshell-bolt11", "kind": "payment_backend", "from": "nutshell-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "nutshell-cache", "kind": "database_backend", "from": "nutshell-mint", "to": "cache", "binding": {"type": "database", "role": "cache"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

fn redis(context: &GateContext, namespace: &str, command: &str) -> Result<String> {
    let script = format!("redis-cli --no-auth-warning -a \"$REDIS_PASSWORD\" {command}");
    context
        .kubectl
        .exec(namespace, "deployment/cache", &["sh", "-c", &script])
}

pub fn run(context: &GateContext) -> Result<()> {
    let directory = context
        .root
        .join("dev/wallet-integration-runs")
        .join(&context.run_id);
    fs::create_dir_all(&directory)?;
    let result = exercise(context);
    let mut client = context.default_session("cross-mint-wallet-live", "experiment-agent")?;

    let _ = client.call(
        "run_finish",
        json!({"request_id":"4597","run_id":EXPERIMENT}),
    );
    if let Ok(export) = crate::cell::evidence(&mut client, json!({"run_id":EXPERIMENT,})) {
        fs::write(
            directory.join("evidence.json"),
            serde_json::to_vec_pretty(&export)?,
        )?;
    }
    fs::write(
        directory.join("outcome.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":result.is_ok(),"error":result.as_ref().err().map(|error|format!("{error:#}"))}),
        )?,
    )?;
    // A failed assertion must still retire the disposable cell through its finalizer.
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    let closed = cell::wait_closed(&mut client, INSTANCE)?;
    fs::write(
        directory.join("closed.json"),
        serde_json::to_vec_pretty(&closed)?,
    )?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("cross-implementation gate did not verify teardown");
    }
    result.context("cross-implementation wallet regression failed")
}

fn exercise(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("cross-mint-wallet-live", "experiment-agent")?;

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":context.document(cell_document())?,"request_id":"create-cross-mint-wallet"}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;

    for (component, catalog_id, version, config_version) in [
        ("cache", "redis", "8.10.1", "redis/8.10/v1"),
        ("cdk-mint", "cdk", "0.18.1", "cdk-mintd/0.18/v1"),
        (
            "nutshell-mint",
            "nutshell",
            "0.21.0",
            "nutshell-mint/0.20/v1",
        ),
    ] {
        let entry = expect::array(&published, "/lock/entries")?
            .iter()
            .find(|entry| entry.get("component_id").and_then(Value::as_str) == Some(component))
            .ok_or_else(|| anyhow::anyhow!("no lock entry for {component}"))?;
        if expect::string(entry, "/catalog_id")? != catalog_id
            || expect::string(entry, "/version")? != context.selected_version(catalog_id, version)
            || expect::string(entry, "/config_version")? != config_version
            || !expect::string(entry, "/image")?.contains("@sha256:")
        {
            bail!("unexpected pinned lock for {component}: {entry}");
        }
    }

    crate::cell::apply(&mut client, &preview)?;
    let ready = cell::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let components = client.call(
        "cell_component_status_list",
        json!({"name": INSTANCE, "limit": 50}),
    )?;
    let mut actual: Vec<&str> = expect::array(&components, "/components")?
        .iter()
        .filter(|component| component.get("ready").and_then(Value::as_bool) == Some(true))
        .map(|component| expect::string(component, "/id"))
        .collect::<Result<_>>()?;
    actual.sort_unstable();
    let mut wanted = [
        "cache",
        "cdk-mint",
        "cdk-recipient",
        "cdk-wallet",
        "chain",
        "mint-lnd",
        "nutshell-mint",
        "nutshell-recipient",
        "nutshell-wallet",
        "payer-lnd",
    ];
    wanted.sort_unstable();
    if actual != wanted {
        bail!("cross-implementation topology is not fully ready: {components}");
    }

    let public_config =
        context
            .kubectl
            .get_json(&["get", "configmap/nutshell-mint-config", "-n", namespace])?;
    for (key, value) in [
        ("MINT_REDIS_CACHE_ENABLED", "TRUE"),
        ("MINT_REDIS_CACHE_TTL", "900"),
        ("MINT_REDIS_CACHE_CLUSTER", "FALSE"),
    ] {
        expect::equals(&public_config, &format!("/data/{key}"), &Value::from(value))?;
    }
    let data = expect::object(&public_config, "/data")?;
    if data.contains_key("MINT_REDIS_CACHE_URL")
        || data
            .values()
            .any(|value| value.as_str().is_some_and(|text| text.contains("redis://")))
    {
        bail!("public Nutshell configuration contains the private Redis URL");
    }

    let cache_secret_args = [
        "get",
        "secret/cache-credentials",
        "-n",
        namespace,
        "-o",
        "json",
    ];
    let cache_secret_digest = context.kubectl.digest(&cache_secret_args)?;
    let cache_secret =
        context
            .kubectl
            .get_json(&["get", "secret/cache-credentials", "-n", namespace])?;
    let mut keys: Vec<&str> = expect::object(&cache_secret, "/data")?
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    if keys != ["PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"] {
        bail!("generated Redis Secret has an unexpected key contract: {keys:?}");
    }

    let rendered = context.kubectl.exec(
        namespace,
        "deployment/nutshell-mint",
        &["/opt/proofstorm/driver", "nutshell", "redis-settings"],
    )?;
    let cache_settings: Value = serde_json::from_str(rendered.trim())?;
    let expected_cache = json!({
        "enabled": true,
        "host": "cache",
        "password_length": 64,
        "ttl": 900,
        "cluster": false
    });
    if cache_settings != expected_cache {
        bail!("live Nutshell Redis settings differ: {cache_settings}");
    }

    client.call(
        "run_start",
        json!({"request_id":"10001","run_id": EXPERIMENT, "name": INSTANCE}),
    )?;

    crate::native::bootstrap(
        &mut client,
        INSTANCE,
        EXPERIMENT,
        "cross-mint-bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;

    for (implementation, mint, wallet) in [
        ("cdk", "cdk-mint", "cdk-wallet"),
        ("nutshell", "nutshell-mint", "nutshell-wallet"),
    ] {
        let prefix = format!("{implementation}-wallet");
        let mut native = crate::native::Session::new(&mut client, INSTANCE, EXPERIMENT);
        native.nutshell_initialize(wallet, mint, &format!("{prefix}-initialize"))?;
        anyhow::ensure!(
            native.nutshell_balance(wallet, mint, &format!("{prefix}-balance"))? == 0,
            "{implementation} wallet did not start empty"
        );
        anyhow::ensure!(
            native.nutshell_fund(wallet, mint, "payer-lnd", &format!("{prefix}-fund"), 1000)?
                == 1000,
            "{implementation} wallet funding balance differs"
        );
        anyhow::ensure!(
            native.nutshell_fund(
                wallet,
                mint,
                "payer-lnd",
                &format!("{prefix}-round-trip-fund"),
                1000
            )? == 2000,
            "{implementation} round-trip funding balance differs"
        );
        native.nutshell_swap(wallet, mint, &format!("{prefix}-round-trip"), 100)?;

        let recipient = format!("{implementation}-recipient");
        native.nutshell_initialize(&recipient, mint, &format!("{prefix}-recipient-initialize"))?;
        let quote_id = native.nutshell_invoice(
            &recipient,
            mint,
            &format!("{prefix}-recipient-invoice"),
            100,
        )?;
        let invoice = native.nutshell_invoice_projection(
            &recipient,
            mint,
            &format!("{prefix}-invoice-read"),
            &quote_id,
            100,
        )?;
        let before = native.nutshell_balance(wallet, mint, &format!("{prefix}-before-pay"))?;
        let melt = native.nutshell_melt(
            wallet,
            mint,
            &format!("{prefix}-pay"),
            expect::string(&invoice, "/payment_request")?,
            100,
        )?;
        anyhow::ensure!(melt["state"] == "PAID", "native payment did not settle");
        let after = native.nutshell_balance(wallet, mint, &format!("{prefix}-after-pay"))?;
        native.nutshell_claim(&recipient, mint, &format!("{prefix}-claim"), &quote_id, 100)?;
        anyhow::ensure!(
            native.nutshell_balance(&recipient, mint, &format!("{prefix}-received"))? == 100,
            "recipient did not receive 100 sat"
        );
        let accounting = if implementation == "nutshell" {
            let mint_observation = native.nutshell_mint_melt(
                wallet,
                mint,
                &format!("{prefix}-mint-observe"),
                &melt,
            )?;
            crate::native::assert_payment_accounting(before, after, &melt, &mint_observation)?;
            json!({"before_sat":before,"after_sat":after,"wallet":melt,"mint":mint_observation,"conserved":true})
        } else {
            // The installed passive mint-fee reader supports Nutshell SQLite.
            // Wallet fee fields cannot establish CDK mint-side conservation.
            anyhow::ensure!(
                before
                    .checked_sub(after)
                    .is_some_and(|spent| (100..=110).contains(&spent)),
                "CDK payment exceeded the fixture fee bound"
            );
            json!({"before_sat":before,"after_sat":after,"wallet":melt,"reason":"authoritative_mint_fee_unavailable","conservation_claimed":false})
        };
        fs::write(
            context
                .root
                .join("dev/wallet-integration-runs")
                .join(&context.run_id)
                .join(format!("{prefix}-conservation.json")),
            serde_json::to_vec_pretty(&accounting)?,
        )?;
    }

    let cache_size: u64 = redis(context, namespace, "dbsize")?.trim().parse()?;
    if cache_size < 1 {
        bail!("Nutshell wallet workflow did not populate Redis");
    }
    redis(
        context,
        namespace,
        "set proofstorm:restart-canary present >/dev/null",
    )?;

    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    if context.kubectl.digest(&cache_secret_args)? != cache_secret_digest {
        bail!("controller restart rotated the Redis credentials");
    }

    context
        .kubectl
        .rollout_restart(namespace, "deployment/cache")?;
    let canary = redis(context, namespace, "exists proofstorm:restart-canary")?;
    if canary.trim() != "0" {
        bail!("ephemeral Redis cache survived restart unexpectedly: {canary}");
    }

    context
        .kubectl
        .rollout_restart(namespace, "deployment/nutshell-mint")?;
    cell::wait_phase(&mut client, INSTANCE, "ready", 80, Duration::from_secs(3))?;

    let closed_experiment = client.call(
        "run_finish",
        json!({"request_id":"18796","run_id": EXPERIMENT}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    println!(
        "Native CDK and Nutshell mint payments, exact Nutshell accounting, bounded CDK balances and cache restart checks passed; verifying teardown next"
    );
    Ok(())
}
