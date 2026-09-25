//! Two CDK mints in one cell own distinct controller-generated seeds: their
//! keysets are disjoint, one wallet holds and spends from both independently,
//! and controller and mint restarts preserve each identity.

use std::{collections::BTreeSet, thread::sleep, time::Duration};

use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, json as expect};

const INSTANCE: &str = "cdk-mint-identity-instance";
const EXPERIMENT: &str = "cdk-mint-identity-experiment";
const MINTS: [&str; 2] = ["mint-a", "mint-b"];
const WALLET: &str = "wallet";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cdk-mint-identity-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-identity-mint"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-identity-payer"}},
            {"id": "mint-a", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK Identity A", "description": "Independent mint identity acceptance"}},
            {"id": "mint-b", "kind": "mint", "implementation": "cdk", "version": "0.18.1", "config_version": "cdk-mintd/0.18/v1", "control": "target", "config": {"name": "Proofstorm CDK Identity B", "description": "Independent mint identity acceptance"}},
            {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
        ],
        "links": [
            {"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-a-bolt11", "kind": "payment_backend", "from": "mint-a", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
            {"id": "mint-b-bolt11", "kind": "payment_backend", "from": "mint-b", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let result = exercise(context);
    let mut client = context.default_session("cdk-mint-identity-live", "experiment-agent")?;
    let _ = client.call(
        "run_finish",
        json!({"request_id":"7101","run_id":EXPERIMENT}),
    );
    // A failed assertion must still retire the disposable cell through its finalizer.
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    let closed = cell::wait_closed(&mut client, INSTANCE)?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("CDK mint identity gate did not verify teardown");
    }
    result
}

fn secret_args(mint: &str, namespace: &str) -> [String; 6] {
    [
        "get".into(),
        format!("secret/{mint}-secrets"),
        "-n".into(),
        namespace.into(),
        "-o".into(),
        "json".into(),
    ]
}

/// Secret digests and base64 seed values, compared but never printed.
fn seeds(context: &GateContext, namespace: &str, mint: &str) -> Result<(String, [String; 3])> {
    let args = secret_args(mint, namespace);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let secret = context.kubectl.get_json(&args[..4])?;
    let data = expect::object(&secret, "/data")?;
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys
        != [
            "PROOFSTORM_SECRET_KIND",
            "bdk-mnemonic",
            "bitcoin-rpc-password",
            "ldk-mnemonic",
            "mint-mnemonic",
        ]
    {
        bail!("{mint} Secret has an unexpected key contract: {keys:?}");
    }
    Ok((
        context.kubectl.digest(&args)?,
        [
            expect::string(&secret, "/data/mint-mnemonic")?.to_owned(),
            expect::string(&secret, "/data/ldk-mnemonic")?.to_owned(),
            expect::string(&secret, "/data/bdk-mnemonic")?.to_owned(),
        ],
    ))
}

fn keysets(context: &GateContext, namespace: &str, mint: &str) -> Result<BTreeSet<String>> {
    let body = context.kubectl.exec(
        namespace,
        &format!("deployment/{mint}"),
        &[
            "wget",
            "-q",
            "-T",
            "5",
            "-O",
            "-",
            "http://127.0.0.1:3338/v1/keysets",
        ],
    )?;
    let response: Value = serde_json::from_str(body.trim())?;
    let ids = expect::array(&response, "/keysets")?
        .iter()
        .map(|keyset| expect::string(keyset, "/id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(!ids.is_empty(), "{mint} publishes no keysets");
    Ok(ids)
}

fn exercise(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("cdk-mint-identity-live", "experiment-agent")?;
    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":context.document(cell_document())?,"request_id":"create-cdk-mint-identity"}),
    )?;
    cell::review(&mut client, &preview)?;
    cell::apply(&mut client, &preview)?;
    let ready = cell::wait_phase(&mut client, INSTANCE, "ready", 200, Duration::from_secs(3))?;
    let namespace = expect::string(&ready, "/instance_namespace")?.to_owned();
    let namespace = namespace.as_str();

    let (digest_a, seeds_a) = seeds(context, namespace, MINTS[0])?;
    let (digest_b, seeds_b) = seeds(context, namespace, MINTS[1])?;
    let distinct: BTreeSet<&String> = seeds_a.iter().chain(&seeds_b).collect();
    ensure!(
        distinct.len() == 6,
        "CDK mints share a mint or payment-wallet seed"
    );
    let keysets_a = keysets(context, namespace, MINTS[0])?;
    let keysets_b = keysets(context, namespace, MINTS[1])?;
    ensure!(
        keysets_a.is_disjoint(&keysets_b),
        "CDK mints publish overlapping keysets: {keysets_a:?} / {keysets_b:?}"
    );

    client.call(
        "run_start",
        json!({"request_id":"7001","run_id":EXPERIMENT,"name":INSTANCE}),
    )?;
    crate::native::bootstrap(
        &mut client,
        INSTANCE,
        EXPERIMENT,
        "cdk-mint-identity-bootstrap",
        "chain",
        "mint-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        5_000_000,
    )?;
    {
        let mut native = crate::native::Session::new(&mut client, INSTANCE, EXPERIMENT);
        for (mint, amount) in [(MINTS[0], 1000), (MINTS[1], 2000)] {
            native.nutshell_initialize(WALLET, mint, &format!("{mint}-initialize"))?;
            ensure!(
                native.nutshell_fund(WALLET, mint, "payer-lnd", &format!("{mint}-fund"), amount)?
                    == amount,
                "{mint} funding balance differs"
            );
        }
        // Shared keysets would let one mint's proofs appear under, or be spent at, the other.
        for (mint, amount) in [(MINTS[0], 1000), (MINTS[1], 2000)] {
            let after = native.nutshell_swap(WALLET, mint, &format!("{mint}-swap"), 10)?;
            ensure!(
                (amount - 10..=amount).contains(&after),
                "{mint} balance was not independent of its sibling: {after}"
            );
        }
    }

    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    sleep(Duration::from_secs(5));
    for mint in MINTS {
        context
            .kubectl
            .rollout_restart(namespace, &format!("deployment/{mint}"))?;
    }
    cell::wait_phase(&mut client, INSTANCE, "ready", 80, Duration::from_secs(3))?;
    ensure!(
        seeds(context, namespace, MINTS[0])?.0 == digest_a
            && seeds(context, namespace, MINTS[1])?.0 == digest_b,
        "a restart rotated a CDK mint's seeds"
    );
    ensure!(
        keysets(context, namespace, MINTS[0])? == keysets_a
            && keysets(context, namespace, MINTS[1])? == keysets_b,
        "a restart changed a CDK mint's keysets"
    );

    println!(
        "Two CDK mints hold distinct seeds and keysets, one wallet uses both independently, and restarts preserve each identity; verifying teardown next"
    );
    Ok(())
}
