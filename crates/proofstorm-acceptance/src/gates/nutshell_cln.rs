//! Nutshell 0.20.3 + Core Lightning 26.06.7 REST: restricted rune contract,
//! wallet round trip, conservation, and verified teardown.
//!
//! Ported from `tests/kubernetes/nutshell_cln_mcp_client.py`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{EXPERIMENT_CAPABILITIES, GateContext, json as expect, lab};

/// Runs inside the mint image, which ships `httpx` and the rune file.
const RUNE_PROBE: &str = include_str!("../../drivers/cln_rune_probe.py");

const INSTANCE: &str = "nutshell-cln-instance";
const EXPERIMENT: &str = "nutshell-cln-experiment";
const LEASE: &str = "nutshell-cln-session";
const DRAFT: &str = "nutshell-cln";

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "nutshell-cln-live-lab",
        "components": [
            {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
            {"id": "seed-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-seed"}},
            {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-payer"}},
            {"id": "mint-cln", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-mint"}},
            {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell CLN", "description": "Core Lightning REST acceptance", "clnrest_enable_mpp": true}},
            {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}}
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

fn common(operation: &str) -> Value {
    json!({
        "instance_id": INSTANCE,
        "experiment_id": EXPERIMENT,
        "session_id": LEASE,
        "operation_id": operation
    })
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(target), Value::Object(source)) = (base.as_object_mut(), extra) {
        for (key, value) in source {
            target.insert(key, value);
        }
    }
    base
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut client = context.session(
        "nutshell-cln-live",
        "experiment-agent",
        EXPERIMENT_CAPABILITIES,
    )?;

    let catalog = client.call(
        "proofstorm_catalog_list",
        json!({"implementations": ["nutshell"]}),
    )?;
    let summary = &expect::array(&catalog, "/items")?[0];
    let nutshell = client.call(
        "proofstorm_catalog_entry_read",
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

    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": DRAFT, "lab": lab_document(), "idempotency_key": "create-nutshell-cln"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": DRAFT, "expected_version": 1, "idempotency_key": "publish-nutshell-cln", "include_revision": true}),
    )?;

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

    client.call(
        "proofstorm_lab_materialize",
        json!({"instance_id": INSTANCE, "revision_digest": expect::string(&published, "/digest")?, "idempotency_key": "materialize-nutshell-cln"}),
    )?;
    let ready = lab::wait_phase(
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
        ("MINT_CLNREST_RUNE", "/app/data/.proofstorm/cln.rune"),
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
            text.to_lowercase().contains("rune") && text != "/app/data/.proofstorm/cln.rune"
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
        let raw = kubectl.exec(namespace, "deployment/mint", &["python3", "-c", RUNE_PROBE])?;
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
        "proofstorm_experiment_create",
        json!({"experiment_id": EXPERIMENT, "instance_id": INSTANCE, "idempotency_key": "create-nutshell-cln-experiment"}),
    )?;
    client.call(
        "proofstorm_session_start",
        json!({"experiment_id": EXPERIMENT, "session_id": LEASE, "idempotency_key": "acquire-nutshell-cln-session"}),
    )?;

    client.call(
        "proofstorm_liquidity_bootstrap",
        with(
            common("nutshell-cln-bootstrap"),
            json!({"chain": "chain", "mint_lightning": "seed-lnd", "payer_lightning": "payer-lnd", "funding_sat": 50_000_000, "channel_sat": 10_000_000, "push_sat": 1_000_000, "idempotency_key": "bootstrap-nutshell-cln"}),
        ),
    )?;
    let bootstrap = lab::wait_succeeded(&mut client, "nutshell-cln-bootstrap")?;
    if !expect::boolean(lab::artifact_content(&bootstrap)?, "/ready")? {
        bail!("LND bootstrap failed: {bootstrap}");
    }

    client.call(
        "proofstorm_peer_connect",
        with(
            common("nutshell-cln-peer"),
            json!({"from_lightning": "payer-lnd", "to_lightning": "mint-cln", "idempotency_key": "peer-nutshell-cln"}),
        ),
    )?;
    let peer = lab::wait_succeeded(&mut client, "nutshell-cln-peer")?;
    if !expect::boolean(lab::artifact_content(&peer)?, "/connected")? {
        bail!("LND-to-CLN peer connection failed: {peer}");
    }

    client.call(
        "proofstorm_channel_open",
        with(
            common("nutshell-cln-channel"),
            json!({"chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "mint-cln", "channel_sat": 4_000_000, "push_sat": 1_000_000, "idempotency_key": "channel-nutshell-cln"}),
        ),
    )?;
    let channel = lab::wait_succeeded(&mut client, "nutshell-cln-channel")?;
    if !expect::boolean(lab::artifact_content(&channel)?, "/active")? {
        bail!("LND-to-CLN channel failed: {channel}");
    }

    let wallet = json!({"wallet": "wallet", "mint": "mint"});

    client.call(
        "proofstorm_wallet_initialize",
        with(
            with(common("nutshell-cln-initialize"), wallet.clone()),
            json!({"idempotency_key": "initialize-nutshell-cln"}),
        ),
    )?;
    let initialized = lab::wait_succeeded(&mut client, "nutshell-cln-initialize")?;
    if !expect::boolean(lab::artifact_content(&initialized)?, "/initialized")? {
        bail!("Nutshell CLN wallet initialization failed: {initialized}");
    }

    client.call(
        "proofstorm_wallet_balance",
        with(
            with(common("nutshell-cln-balance"), wallet.clone()),
            json!({"idempotency_key": "balance-nutshell-cln"}),
        ),
    )?;
    let balance = lab::wait_succeeded(&mut client, "nutshell-cln-balance")?;
    if expect::integer(lab::artifact_content(&balance)?, "/balance_sat")? != 0 {
        bail!("Nutshell CLN wallet did not start empty: {balance}");
    }

    client.call(
        "proofstorm_wallet_fund",
        with(
            with(common("nutshell-cln-fund"), wallet.clone()),
            json!({"payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": "fund-nutshell-cln"}),
        ),
    )?;
    let funded = lab::wait_succeeded(&mut client, "nutshell-cln-fund")?;
    let fund_content = lab::artifact_content(&funded)?;
    if expect::integer(fund_content, "/funded_sat")? != 1000
        || expect::integer(fund_content, "/balance_sat")? != 1000
    {
        bail!("Nutshell CLN wallet funding failed: {funded}");
    }

    client.call(
        "proofstorm_wallet_balance",
        with(
            with(
                common("nutshell-cln-balance-before-round-trip"),
                wallet.clone(),
            ),
            json!({"idempotency_key": "balance-before-round-trip-nutshell-cln"}),
        ),
    )?;
    let baseline = lab::wait_succeeded(&mut client, "nutshell-cln-balance-before-round-trip")?;
    if expect::integer(lab::artifact_content(&baseline)?, "/balance_sat")? != 1000 {
        bail!("Nutshell CLN wallet baseline is invalid: {baseline}");
    }

    client.call(
        "proofstorm_wallet_round_trip",
        with(
            with(common("nutshell-cln-round-trip"), wallet.clone()),
            json!({"payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": "round-trip-nutshell-cln"}),
        ),
    )?;
    let round_trip = lab::wait_succeeded(&mut client, "nutshell-cln-round-trip")?;
    let round_content = lab::artifact_content(&round_trip)?;
    if expect::boolean(round_content, "/inflation")?
        || expect::integer(round_content, "/minted_sat")? != 1000
    {
        bail!("Nutshell CLN wallet round trip failed: {round_trip}");
    }

    client.call(
        "proofstorm_conservation_oracle",
        with(
            with(common("nutshell-cln-conservation"), wallet),
            json!({
                "baseline_operation_id": "nutshell-cln-balance-before-round-trip",
                "treatment_operation_id": "nutshell-cln-round-trip",
                "idempotency_key": "conservation-nutshell-cln"
            }),
        ),
    )?;
    let oracle = lab::wait_succeeded(&mut client, "nutshell-cln-conservation")?;
    if !expect::boolean(lab::artifact_content(&oracle)?, "/conserved")? {
        bail!("Nutshell CLN conservation failed: {oracle}");
    }

    client.call(
        "proofstorm_session_finish",
        json!({"session_id": LEASE, "idempotency_key": "release-nutshell-cln-session"}),
    )?;
    let closed_experiment = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id": EXPERIMENT, "idempotency_key": "close-nutshell-cln-experiment"}),
    )?;
    expect::equals(&closed_experiment, "/phase", &Value::from("closed"))?;

    client.call("proofstorm_lab_close", json!({"instance_id": INSTANCE}))?;
    lab::wait_phase(
        &mut client,
        INSTANCE,
        "closed",
        80,
        std::time::Duration::from_secs(3),
    )?;

    println!(
        "Nutshell 0.20.3 + Core Lightning 26.06.7 REST, restricted rune, wallet round-trip, conservation, and teardown passed"
    );
    Ok(())
}
