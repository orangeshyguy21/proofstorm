//! The same monetary assertions for every declared external backend, wallet
//! version and storage selection. Observations come from both participants.
use anyhow::{Context, Result, ensure};
use proofstorm_qualification::{Component, MintRoundtrip};
use serde_json::{Value, json};

use crate::{GateContext, cell, json as expect, native, postgres};

const INSTANCE: &str = "qualification-mint";
const RUN: &str = "qualification-payments";
const WALLET_IDENTITY: &str = "set -eu; umask 077; identity_file=$(mktemp /wallet/.identity.XXXXXX); trap 'rm -f \"$identity_file\"' EXIT; cd /app; cashu -h http://mint:3338 -u sat -w wallet -t -y info --mnemonic >\"$identity_file\"; test -s \"$identity_file\"; sha256sum <\"$identity_file\"";
const CLN: &str =
    "lightning-cli --notifications=none --lightning-dir=/home/cln/.lightning --network=regtest";

fn node(id: &str, component: &Component, platform: &str) -> Result<Value> {
    let catalog = proofstorm_qualification::catalog(platform)?;
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == component.implementation && entry.version == component.version)
        .context("fixture component absent from catalog")?;
    ensure!(entry.image == component.image, "fixture image changed");
    Ok(
        json!({"id":id,"kind":entry.kind,"implementation":entry.id,"version":entry.version,"config_version":entry.config_version,"control":if id == "mint" { "target" } else { "cell" },"config":{}}),
    )
}

fn document(case: &proofstorm_qualification::Case, selection: &MintRoundtrip) -> Result<Value> {
    let find = |implementation: &str| -> Result<&Component> {
        let catalog = proofstorm_qualification::catalog(&case.platform)?;
        let preferred = catalog
            .entries
            .iter()
            .find(|entry| {
                entry.id == implementation
                    && entry.support_lifecycle == proofstorm_core::SupportLifecycle::Preferred
            })
            .context("missing preferred dependency")?;
        case.components
            .iter()
            .find(|entry| {
                entry.implementation == implementation && entry.version == preferred.version
            })
            .context("missing fixture dependency")
    };
    let mut components = vec![
        node("chain", find("bitcoin-core")?, &case.platform)?,
        node("seed-lnd", find("lnd")?, &case.platform)?,
        node("payer-lnd", find("lnd")?, &case.platform)?,
        node("backend", &selection.lightning, &case.platform)?,
        node("mint", &selection.mint, &case.platform)?,
        node("wallet", &selection.wallet, &case.platform)?,
    ];
    if selection.mint.implementation == "cdk" {
        components[4]["config"]["input_fee_ppk"] = 0.into();
    }
    let mut document = json!({
        "api_version":"proofstorm/v1alpha1","name":INSTANCE,"components":components,
        "links":[
            {"id":"seed-chain","kind":"chain_backend","from":"seed-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"payer-chain","kind":"chain_backend","from":"payer-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"backend-chain","kind":"chain_backend","from":"backend","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"payment","kind":"payment_backend","from":"mint","to":"backend","binding":{"type":"payment","method":"bolt11","unit":"sat"}}
        ],"policy":{"allow":["component.exec_live","component.control"],"limits":{"max_components":12,"max_links":16,"max_config_bytes":32768}}
    });
    let database = selection.storage == "postgres";
    ensure!(
        database || selection.storage == "sqlite",
        "unmapped storage"
    );
    postgres::augment_cell(database, &mut document, "qualification_mint");
    Ok(document)
}

pub(super) fn run(context: &GateContext, selection: &MintRoundtrip) -> Result<()> {
    let case = context
        .qualification
        .as_ref()
        .context("qualification case missing")?;
    let document = document(case, selection)?;
    context.qualification_stage("materialize")?;
    let database = selection.storage == "postgres";
    let mut client = context.default_session("qualification-mint", "qualifier")?;
    let preview = client.call(
        "cell_plan",
        json!({"name":INSTANCE,"request_id":"create","cell":document}),
    )?;
    let published = cell::review(&mut client, &preview)?;
    for selected in [&selection.mint, &selection.wallet, &selection.lightning] {
        let locked = published["lock"]["entries"]
            .as_array()
            .context("missing lock entries")?
            .iter()
            .find(|entry| {
                entry["catalog_id"] == selected.implementation
                    && entry["version"] == selected.version
            })
            .context("missing exact selected lock entry")?;
        ensure!(
            locked["version"] == selected.version && locked["image"] == selected.image,
            "qualified cell lock differs from the planned image"
        );
    }
    cell::apply(&mut client, &preview)?;
    let ready = cell::wait_ready(&mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?.to_owned();
    client.call(
        "run_start",
        json!({"name":INSTANCE,"run_id":RUN,"request_id":"run"}),
    )?;
    context.qualification_stage("funding")?;
    native::bootstrap(
        &mut client,
        INSTANCE,
        RUN,
        "fund",
        "chain",
        "seed-lnd",
        "payer-lnd",
        50_000_000,
        10_000_000,
        1_000_000,
    )?;
    let mut native = native::Session::new(&mut client, INSTANCE, RUN);
    let backend_command = if selection.lightning.implementation == "cln" {
        CLN
    } else {
        native::LND
    };
    let backend_info = native.json(
        "backend",
        "backend-identity",
        &format!("{backend_command} getinfo"),
    )?;
    let key = if selection.lightning.implementation == "cln" {
        "/id"
    } else {
        "/identity_pubkey"
    };
    let pubkey = expect::string(&backend_info, key)?.to_owned();
    native.execute(
        "payer-lnd",
        "backend-connect",
        &format!(
            "{} connect {}",
            native::LND,
            native::quote(&format!("{pubkey}@backend:9735"))
        ),
    )?;
    let opened = native.json(
        "payer-lnd",
        "backend-open",
        &format!(
            "{} openchannel --node_key={} --local_amt=4000000 --push_amt=1000000",
            native::LND,
            native::quote(&pubkey)
        ),
    )?;
    native.mine("chain", "backend-confirm", 6)?;
    native.poll(
        "payer-lnd",
        "backend-active",
        &format!("{} listchannels", native::LND),
        |channels| native::active_channel_point(&opened, channels),
    )?;
    context.qualification_stage("issuance")?;
    native.nutshell_initialize("wallet", "mint", "wallet-initialize")?;
    ensure!(
        native.nutshell_balance("wallet", "mint", "empty")? == 0,
        "wallet must begin empty"
    );
    let funded = native.nutshell_fund("wallet", "mint", "payer-lnd", "fund-wallet", 2000)?;
    ensure!(funded == 2000, "incorrect mint credit");
    let identity_before = native.execute("wallet", "wallet-identity", WALLET_IDENTITY)?;
    context.qualification_stage("swap-and-melt")?;
    let after_swap = native.nutshell_swap("wallet", "mint", "swap", 50)?;
    let after_payment = payment(&mut native, "first", after_swap)?;
    drop(native);
    context.qualification_stage("restart")?;
    if database {
        postgres::seed_sentinel(true, &context.kubectl, &namespace, "qualification")?;
        postgres::restart_database(true, &context.kubectl, &namespace)?;
        postgres::verify_sentinel(true, &context.kubectl, &namespace, "qualification")?;
    }
    for component in ["mint", "wallet", "backend"] {
        client.call("component_restart", json!({"name":INSTANCE,"run_id":RUN,"request_id":format!("restart-{component}"),"component":component}))?;
        cell::wait_operation(&mut client, &format!("restart-{component}"), 80)?;
        cell::wait_ready(&mut client, INSTANCE)?;
    }
    let mut native = native::Session::new(&mut client, INSTANCE, RUN);
    ensure!(
        native.nutshell_balance("wallet", "mint", "persisted-balance")? == after_payment,
        "wallet balance changed through restart"
    );
    let identity_after = native.execute("wallet", "wallet-identity-after", WALLET_IDENTITY)?;
    ensure!(
        identity_before["stdout"] == identity_after["stdout"],
        "wallet identity changed through restart"
    );
    let backend_after = native.json(
        "backend",
        "backend-identity-after",
        &format!("{backend_command} getinfo"),
    )?;
    ensure!(
        expect::string(&backend_after, key)? == pubkey,
        "node identity changed through restart"
    );
    context.qualification_stage("payment-after-restart")?;
    let final_balance = payment(&mut native, "after-restart", after_payment)?;
    drop(native);
    context.record("qualification-observations.json", &json!({"mint":selection.mint,"wallet":selection.wallet,"backend":selection.lightning,"storage":selection.storage,"issued_sat":2000,"recipient_paid_sat":200,"final_balance_sat":final_balance,"identities_preserved":true}))?;
    client.call("run_finish", json!({"run_id":RUN,"request_id":"finish"}))?;
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;
    Ok(())
}

fn payment(native: &mut native::Session<'_>, id: &str, before: u64) -> Result<u64> {
    let invoice = native.json(
        "payer-lnd",
        &format!("{id}-invoice"),
        &format!("{} addinvoice --amt=100", native::LND),
    )?;
    let request = expect::string(&invoice, "/payment_request")?;
    let melt = native.nutshell_melt("wallet", "mint", &format!("{id}-melt"), request, 100)?;
    ensure!(melt["state"] == "PAID", "melt did not reach PAID");
    let hash = expect::string(&invoice, "/r_hash")?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid payment hash"
    );
    let recipient = native.json(
        "payer-lnd",
        &format!("{id}-recipient"),
        &format!(
            "{} lookupinvoice --rhash={}",
            native::LND,
            native::quote(hash)
        ),
    )?;
    ensure!(
        recipient["settled"] == true && recipient["amt_paid_sat"] == "100",
        "independent recipient did not receive exactly 100 sat"
    );
    let after = native.nutshell_balance("wallet", "mint", &format!("{id}-balance"))?;
    let debit = before
        .checked_sub(after)
        .context("payment inflated wallet balance")?;
    // Upstream wallet/mint input fees are distinct from the Lightning amount.
    // Assert a bounded debit and independently settled value, not conservation.
    ensure!(
        (100..=150).contains(&debit),
        "payment debit is outside its explicit fee bound"
    );
    Ok(after)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_planned_mint_fixture_resolves_its_exact_platform_images() {
        let plan = proofstorm_qualification::plan(
            proofstorm_qualification::Identity {
                revision: "a".repeat(40),
                run_id: "1".into(),
                attempt: 1,
            },
            true,
        )
        .unwrap();
        for case in &plan.cases {
            let proofstorm_qualification::Scenario::Mint { configuration } = &case.scenario else {
                continue;
            };
            let document: proofstorm_core::CellSpec =
                serde_json::from_value(super::document(case, configuration).unwrap()).unwrap();
            let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
            let lock = proofstorm_core::resolve_lock(&document, &catalog)
                .unwrap_or_else(|error| panic!("{}: {error}", case.id));
            let json = serde_json::to_value(lock).unwrap();
            for selected in &case.components {
                assert!(
                    json["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["catalog_id"] == selected.implementation
                            && entry["version"] == selected.version
                            && entry["image"] == selected.image),
                    "{}: missing {}@{}",
                    case.id,
                    selected.implementation,
                    selected.version
                );
            }
        }
    }
}
