//! Nutshell 0.21.0 + Core Lightning 26.06.7 REST: restricted rune contract,
//! wallet round trip, balance accounting, and verified teardown.
//!
//! Ported from `tests/kubernetes/nutshell_cln_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, cell, json as expect};

const INSTANCE: &str = "nutshell-cln-instance";
const EXPERIMENT: &str = "nutshell-cln-experiment";

fn cell_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-cln-live-cell",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "31.1", "config_version": "bitcoin-core/31/v1", "control": "cell", "config": {}},
            {"id": "seed-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cln-seed"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.21.3-beta", "config_version": "lnd/0.20/v1", "control": "cell", "config": {"alias": "proofstorm-cln-payer"}},
            {"id": "mint-cln", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "cell", "config": {"alias": "proofstorm-cln-mint"}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.21.0", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell CLN", "description": "Core Lightning REST acceptance", "clnrest_enable_mpp": true}},
            {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.21.0", "config_version": "nutshell-wallet/0.20/v1", "control": "cell", "config": {}}
        ],
        "links": [
            {"id": "seed-chain", "kind": "chain_backend", "from": "seed-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "payer-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "cln-chain", "kind": "chain_backend", "from": "mint-cln", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
            {"id": "mint-cln-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-cln", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}}
        ],
        "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
    })
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.default_session("nutshell-cln-live", "experiment-agent")?;

    let catalog = client.call("catalog_list", json!({"implementations": ["nutshell"]}))?;
    let summary = &expect::array(&catalog, "/items")?[0];
    let nutshell = client.call(
        "catalog_entry_read",
        json!({
            "id": expect::string(summary, "/id")?,
            "version": expect::string(summary, "/version")?
        }),
    )?;
    let mut backends: Vec<&str> = expect::array(&nutshell, "/support_matrix/payment_backends")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    backends.sort_unstable();
    if backends != ["cln", "lnd"] {
        bail!(
            "Nutshell does not advertise exact CLN and LND support: {}",
            nutshell["support_matrix"]
        );
    }
    let advertises_cln = expect::array(&nutshell, "/support_matrix/payment_bindings")?
        .iter()
        .any(|binding| {
            binding
                .pointer("/backend/implementation")
                .and_then(Value::as_str)
                == Some("cln")
                && binding.pointer("/backend/versions") == Some(&json!(["26.06.7"]))
        });
    if !advertises_cln {
        bail!("Nutshell does not advertise its exact Core Lightning binding");
    }

    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"cell":cell_document(),"request_id":"create-nutshell-cln"}),
    )?;
    let published = crate::cell::review(&mut client, &preview)?;

    for (component, catalog_id) in [("mint", "nutshell"), ("mint-cln", "cln")] {
        let entry = expect::array(&published, "/lock/entries")?
            .iter()
            .find(|entry| entry.get("component_id").and_then(Value::as_str) == Some(component))
            .ok_or_else(|| anyhow::anyhow!("no lock entry for {component}"))?;
        expect::equals(entry, "/catalog_id", &Value::from(catalog_id))?;
        let image = expect::string(entry, "/image")?;
        if !image.contains("@sha256:") {
            bail!("{component} image is not digest-pinned: {image}");
        }
    }

    crate::cell::apply(&mut client, &preview)?;
    let ready = cell::wait_phase(
        &mut client,
        INSTANCE,
        "ready",
        220,
        std::time::Duration::from_secs(3),
    )?;
    let namespace = expect::string(&ready, "/instance_namespace")?;

    let mint_config =
        context
            .kubectl
            .get_json(&["get", "configmap/mint-config", "-n", namespace])?;
    for (key, value) in [
        ("MINT_BACKEND_BOLT11_SAT", "CLNRestWallet"),
        ("MINT_CLNREST_ENABLE_MPP", "TRUE"),
        ("MINT_CLNREST_RUNE", "/app/data/.proofstorm/cln-xpay.rune"),
        ("MINT_CLNREST_URL", "http://mint-cln:3010"),
    ] {
        expect::equals(&mint_config, &format!("/data/{key}"), &Value::from(value))?;
    }
    let data = expect::object(&mint_config, "/data")?;
    if data.keys().any(|key| key.starts_with("MINT_LND_")) {
        bail!("Nutshell CLN public configuration contains an LND setting");
    }
    if data.values().any(|value| {
        value.as_str().is_some_and(|text| {
            text.to_lowercase().contains("rune") && text != "/app/data/.proofstorm/cln-xpay.rune"
        })
    }) {
        bail!("Nutshell CLN public configuration contains rune material");
    }

    let service = context
        .kubectl
        .get_json(&["get", "service/mint-cln", "-n", namespace])?;
    let mut ports: Vec<(String, u64)> = expect::array(&service, "/spec/ports")?
        .iter()
        .map(|port| {
            Ok((
                expect::string(port, "/name")?.to_string(),
                expect::integer(port, "/port")?,
            ))
        })
        .collect::<Result<_>>()?;
    ports.sort();
    if ports != [("p2p".to_string(), 9735), ("rest".to_string(), 3010)] {
        bail!("Core Lightning service contract differs: {ports:?}");
    }

    let probe = |kubectl: &crate::Kubectl| -> Result<Value> {
        let raw = kubectl.exec(
            namespace,
            "deployment/mint",
            &["/opt/proofstorm/driver", "nutshell", "rune-probe"],
        )?;
        Ok(serde_json::from_str(raw.trim())?)
    };
    let before = probe(&context.kubectl)?;
    let length = expect::integer(&before, "/length")?;
    let allowed = expect::integer(&before, "/allowed")?;
    let forbidden = expect::integer(&before, "/forbidden")?;
    if length < 32
        || expect::string(&before, "/mode")? != "0o600"
        || !matches!(allowed, 200 | 201)
        || !matches!(forbidden, 401 | 403)
    {
        bail!("restricted CLN rune contract failed: {before}");
    }

    context
        .kubectl
        .rollout_restart(namespace, "deployment/mint")?;
    let after = probe(&context.kubectl)?;
    if after != before {
        bail!(
            "Nutshell restart changed its restricted CLN rune contract: before={before} after={after}"
        );
    }

    client.call(
        "run_start",
        json!({"request_id":"8067","run_id": EXPERIMENT, "name": INSTANCE}),
    )?;

    crate::native::bootstrap(
        &mut client,
        INSTANCE,
        EXPERIMENT,
        "nutshell-cln-bootstrap",
        "chain",
        "seed-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        1_000_000,
    )?;
    let mut native = crate::native::Session::new(&mut client, INSTANCE, EXPERIMENT);
    let identity = native.json(
        "mint-cln",
        "nutshell-cln-identity",
        "lightning-cli --lightning-dir=/home/cln/.lightning --network=regtest getinfo",
    )?;
    let pubkey = expect::string(&identity, "/id")?;
    native.execute(
        "payer-lnd",
        "nutshell-cln-peer",
        &format!(
            "{} connect {}",
            crate::native::LND,
            crate::native::quote(&format!("{pubkey}@mint-cln:9735"))
        ),
    )?;
    let opened = native.json(
        "payer-lnd",
        "nutshell-cln-open",
        &format!(
            "{} openchannel --node_key={} --local_amt=4000000 --push_amt=1000000",
            crate::native::LND,
            crate::native::quote(pubkey)
        ),
    )?;
    native.mine("chain", "nutshell-cln-confirm", 6)?;
    native.poll(
        "payer-lnd",
        "nutshell-cln-active",
        &format!("{} listchannels", crate::native::LND),
        |channels| crate::native::active_channel_point(&opened, channels),
    )?;

    let mut native = crate::native::Session::new(&mut client, INSTANCE, EXPERIMENT);
    native.nutshell_initialize("wallet", "mint", "nutshell-cln-initialize")?;
    anyhow::ensure!(
        native.nutshell_balance("wallet", "mint", "nutshell-cln-balance")? == 0,
        "Nutshell CLN wallet did not start empty"
    );
    let funded = native.nutshell_fund("wallet", "mint", "payer-lnd", "nutshell-cln-fund", 1000)?;
    anyhow::ensure!(funded == 1000, "Nutshell CLN funding balance differs");
    let before = native.nutshell_fund(
        "wallet",
        "mint",
        "payer-lnd",
        "nutshell-cln-round-trip-fund",
        1000,
    )?;
    anyhow::ensure!(
        before == 2000,
        "Nutshell CLN round-trip funding balance differs"
    );
    let after = native.nutshell_swap("wallet", "mint", "nutshell-cln-round-trip", 100)?;
    anyhow::ensure!(
        (before - 100..=before).contains(&after),
        "Nutshell CLN round-trip balance accounting failed"
    );

    let closed_experiment = client.call(
        "run_finish",
        json!({"request_id":"13781","run_id": EXPERIMENT}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    client.call("cell_remove", json!({"name": INSTANCE}))?;
    cell::wait_phase(
        &mut client,
        INSTANCE,
        "closed",
        80,
        std::time::Duration::from_secs(3),
    )?;

    println!(
        "Nutshell 0.21.0 + Core Lightning 26.06.7 REST, restricted rune, wallet round-trip, balance accounting, and teardown passed"
    );
    Ok(())
}
