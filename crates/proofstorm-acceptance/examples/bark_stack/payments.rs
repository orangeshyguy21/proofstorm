//! Funded BOLT11 qualification using the existing native CDK mint and wallet.
use super::{Docker, catalog_image, certificates};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

const WALLET: &str = "cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";
const MINT: &str = "http://mint:3338";
// Public BIP39 test vectors are restricted to this isolated regtest fixture.
const PROCESSOR_SEED: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const MINT_SEED: &str =
    "legal winner thank year wave sausage worth useful legal winner thank yellow";

fn text<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string {path}"))
}
fn chain(d: &mut Docker, tail: &[&str]) -> Result<String> {
    let name = d.name("chain");
    let mut args = vec![
        "exec",
        &name,
        "bitcoin-cli",
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
    ];
    args.extend_from_slice(tail);
    d.run(&args)
}
fn cln(d: &mut Docker, role: &str, tail: &[&str]) -> Result<Value> {
    let name = d.name(role);
    let mut args = vec![
        "exec",
        &name,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "--notifications=none",
    ];
    args.extend_from_slice(tail);
    d.json(&args)
}
fn execute(d: &mut Docker, role: &str, script: &str) -> Result<String> {
    let name = d.name(role);
    d.run(&["exec", &name, "sh", "-ec", script])
}
fn start(
    d: &mut Docker,
    role: &str,
    network: &str,
    image: &str,
    options: &[String],
    command: &[&str],
) -> Result<()> {
    let mut args = vec![
        "create".into(),
        "--name".into(),
        d.name(role),
        "--label".into(),
        d.label(),
        "--network".into(),
        network.into(),
        "--network-alias".into(),
        role.into(),
    ];
    args.extend_from_slice(options);
    args.push(image.into());
    args.extend(command.iter().map(|s| (*s).into()));
    d.run(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    d.run(&["start", &d.name(role)])?;
    Ok(())
}
fn bind(options: &mut Vec<String>, source: &str, destination: &str) {
    options.extend(["-v".into(), format!("{source}:{destination}:ro")]);
}
fn observe(d: &mut Docker) -> Result<Value> {
    let result = execute(
        d,
        "wallet",
        "PROOFSTORM_DATABASE=/wallet/cdk/cdk-cli.sqlite PROOFSTORM_WALLET=wallet PROOFSTORM_MINT=mint PROOFSTORM_MINT_URL=http://mint:3338 /driver observe cdk-cli-wallet",
    )?;
    Ok(serde_json::from_str(&result)?)
}
fn settled_balance(value: &Value, expected: u64) -> Result<()> {
    ensure!(
        value["balance_sat"] == expected
            && value["reserved_sat"] == 0
            && value["pending_sat"] == 0
            && value["pending_spent_sat"] == 0,
        "unexpected passive wallet balance"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    d: &mut Docker,
    network: &str,
    cookie: &str,
    chain_password: &str,
    cln_image: &str,
    mining_address: &str,
    server_wallet: &Value,
) -> Result<Value> {
    eprintln!("Funding Bark and opening an independently observed Lightning channel...");
    let processor_image = d.image("proofstorm-cdk-bark:dev-rpc", false)?;
    let mint_image = d.image(&catalog_image("cdk")?, true)?;
    let wallet_image = d.image(&catalog_image("cdk-cli-wallet")?, true)?;
    let driver_ref = std::env::var("PROOFSTORM_BARK_DRIVER_IMAGE").context("set PROOFSTORM_BARK_DRIVER_IMAGE to a local Proofstorm controller image containing /usr/local/lib/proofstorm-driver")?;
    let driver_image = d.image(&driver_ref, false)?;
    d.save("payment-images.json", &json!({"processor":processor_image,"mint":mint_image,"wallet":wallet_image,"driver_source":driver_image}))?;
    let helper = d.name("driver-copy");
    d.run(&[
        "create",
        "--name",
        &helper,
        "--label",
        &d.label(),
        "--network",
        "none",
        &driver_image,
    ])?;
    let driver = d.work.join("driver").display().to_string();
    d.run(&[
        "cp",
        &format!("{helper}:/usr/local/lib/proofstorm-driver"),
        &driver,
    ])?;
    d.run(&["rm", "-v", &helper])?;
    d.save(
        "driver.json",
        &json!({"sha256":format!("{:x}",Sha256::digest(fs::read(&driver)?))}),
    )?;

    chain(
        d,
        &[
            "sendtoaddress",
            text(server_wallet, "/rounds/address")?,
            "20",
        ],
    )?;
    let peer_volume = d.volume("peer-data", cln_image)?;
    let peer_config = d.file("peer.conf", &format!("network=regtest\nlightning-dir=/data\nbitcoin-rpcconnect=chain\nbitcoin-rpcport=18443\nbitcoin-rpcuser=proofstorm\nbitcoin-rpcpassword={chain_password}\naddr=0.0.0.0:9735\n"))?;
    start(
        d,
        "peer",
        network,
        cln_image,
        &[
            "-v".into(),
            format!("{peer_volume}:/data"),
            "-v".into(),
            format!("{peer_config}:/config/lightning.conf:ro"),
        ],
        &[],
    )?;
    let peer = d.name("peer");
    let peer_info: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &peer,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "getinfo",
    ])?)?;
    let address = cln(d, "cln", &["newaddr"])?;
    chain(d, &["sendtoaddress", text(&address, "/bech32")?, "1"])?;
    chain(d, &["generatetoaddress", "6", mining_address])?;
    let lightning = d.name("cln");
    d.poll(
        &[
            "exec",
            &lightning,
            "lightning-cli",
            "--lightning-dir=/data",
            "--network=regtest",
            "listfunds",
        ],
        |v| {
            v["outputs"]
                .as_array()
                .is_some_and(|xs| xs.iter().any(|x| x["status"] == "confirmed"))
        },
    )?;
    cln(
        d,
        "cln",
        &["connect", text(&peer_info, "/id")?, "peer", "9735"],
    )?;
    cln(
        d,
        "cln",
        &[
            "-k",
            "fundchannel",
            &format!("id={}", text(&peer_info, "/id")?),
            "amount=2000000sat",
            "push_msat=1000000000msat",
        ],
    )?;
    chain(d, &["generatetoaddress", "6", mining_address])?;
    for role in ["cln", "peer"] {
        let name = d.name(role);
        let channels = d.poll(
            &[
                "exec",
                &name,
                "lightning-cli",
                "--lightning-dir=/data",
                "--network=regtest",
                "listpeerchannels",
            ],
            |v| {
                v["channels"]
                    .as_array()
                    .is_some_and(|xs| xs.iter().any(|x| x["state"] == "CHANNELD_NORMAL"))
            },
        )?;
        d.save(&format!("{role}-channels.json"), &channels)?;
    }
    let server = d.name("server");
    let funded = d.poll(
        &[
            "exec",
            &server,
            "captaind",
            "--config",
            "/config/captaind.toml",
            "rpc",
            "wallet",
        ],
        |v| {
            v["rounds"]["trusted_balance"]
                .as_u64()
                .is_some_and(|n| n > 1_000_000)
        },
    )?;
    d.save("server-funded.json", &funded)?;

    let tls = certificates(d, "processor")?;
    let processor_config = d.file("processor.toml", &format!("address = \"0.0.0.0\"\nport = 50051\ntls_enable = true\nallow_insecure = false\ntls_cert_path = \"/tls/server.pem\"\ntls_key_path = \"/tls/server.key\"\ntls_client_ca_path = \"/tls/ca.pem\"\n[bark]\nmnemonic = \"{PROCESSOR_SEED}\"\nnetwork = \"regtest\"\nserver_address = \"http://bark-server:3535\"\nbitcoind_address = \"http://chain:18443\"\nbitcoind_cookiefile = \"/chain-auth/rpc.cookie\"\ndata_dir = \"/data\"\nevent_poll_interval_ms = 100\npayment_methods = [\"bolt11\"]\n"))?;
    let processor_volume = d.volume("processor-data", cln_image)?;
    let mut options = vec![
        "--workdir".into(),
        "/config".into(),
        "-v".into(),
        format!("{processor_volume}:/data"),
    ];
    bind(&mut options, &processor_config, "/config/config.toml");
    bind(&mut options, cookie, "/chain-auth/rpc.cookie");
    for (source, dest) in [
        ("ca.pem", "ca.pem"),
        ("server.pem", "server.pem"),
        ("server-key.pem", "server.key"),
    ] {
        bind(&mut options, &tls[source], &format!("/tls/{dest}"));
    }
    start(d, "processor", network, &processor_image, &options, &[])?;
    let mint_volume = d.volume("mint-data", cln_image)?;
    let mint_config = d.file("mint.toml", &format!("[info]\nurl = \"{MINT}\"\nlisten_host = \"0.0.0.0\"\nlisten_port = 3338\nmnemonic = \"file:/secrets/mint-mnemonic\"\ninput_fee_ppk = 0\n[database]\nengine = \"sqlite\"\n[payment_backend]\nbackend = \"grpcprocessor\"\nunit = \"sat\"\nmin_mint = 1\nmax_mint = 1000000\nmin_melt = 1\nmax_melt = 1000000\n[grpc_processor]\nsupported_units = [\"sat\"]\naddress = \"processor\"\nport = 50051\ntls_dir = \"/tls\"\nallow_insecure = false\n"))?;
    let mut options = vec![
        "--user".into(),
        "1000:1000".into(),
        "--entrypoint".into(),
        "cdk-mintd".into(),
        "-e".into(),
        "CDK_MINTD_WORK_DIR=/data".into(),
        "-v".into(),
        format!("{mint_volume}:/data"),
    ];
    bind(&mut options, &mint_config, "/config/config.toml");
    let mint_seed = d.file("mint-mnemonic", MINT_SEED)?;
    bind(&mut options, &mint_seed, "/secrets/mint-mnemonic");
    for (source, dest) in [
        ("ca.pem", "ca.pem"),
        ("client.pem", "client.pem"),
        ("client-key.pem", "client.key"),
    ] {
        bind(&mut options, &tls[source], &format!("/tls/{dest}"));
    }
    start(
        d,
        "mint-create",
        network,
        &mint_image,
        &options,
        &[
            "config",
            "init",
            "--new-mint",
            "--file",
            "/config/config.toml",
        ],
    )?;
    let initializer = d.name("mint-create");
    ensure!(
        d.run(&["wait", &initializer])? == "0",
        "native mint initialization failed"
    );
    d.run(&["rm", "-v", &initializer])?;
    start(d, "mint", network, &mint_image, &options, &[])?;
    let wallet_volume = d.volume("wallet-data", cln_image)?;
    let mut options = vec!["-v".into(), format!("{wallet_volume}:/wallet")];
    bind(&mut options, &driver, "/driver");
    start(d, "wallet", network, &wallet_image, &options, &[])?;
    let wallet = d.name("wallet");
    let info: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &wallet,
        "/driver",
        "http-json",
        &format!("{MINT}/v1/info"),
    ])?)?;
    d.save("mint-info.json", &info)?;
    execute(d, "wallet", &format!("{WALLET} balance"))?;
    settled_balance(&observe(d)?, 0)?;

    eprintln!("Minting 100,000 sat through Bark's mutually authenticated processor...");
    // Owned container tracks this detached operation; poll its result, never resubmit it.
    d.run(&["exec","-d",&wallet,"sh","-c",&format!("{WALLET} mint {MINT} 100000 --wait-duration 180 > /wallet/mint.log 2>&1; echo $? > /wallet/mint.exit")])?;
    d.wait(&[
        "exec",
        &wallet,
        "/driver",
        "cdk-quote",
        "await",
        "UNPAID",
        "/wallet/cdk/cdk-cli.sqlite",
        MINT,
        "100000",
    ])?;
    let invoice = execute(
        d,
        "wallet",
        &format!("/driver cdk-quote invoice UNPAID /wallet/cdk/cdk-cli.sqlite {MINT} 100000"),
    )?;
    let quote = execute(
        d,
        "wallet",
        &format!("/driver cdk-quote id UNPAID /wallet/cdk/cdk-cli.sqlite {MINT} 100000"),
    )?;
    let payment = cln(d, "peer", &["pay", &invoice])?;
    ensure!(
        payment["status"] == "complete" && payment["amount_msat"] == 100_000_000,
        "incoming Lightning payment not independently settled"
    );
    d.save("mint-payer.json", &payment)?;
    d.wait(&["exec", &wallet, "sh", "-ec", "test -f /wallet/mint.exit"])?;
    ensure!(
        execute(d, "wallet", "cat /wallet/mint.exit")? == "0",
        "mint command failed; inspect wallet log"
    );
    // Non-interactive CDK may return after creating the quote. Reconcile that
    // exact quote before issuing; never replace it with a second invoice.
    let paid = d.poll(
        &[
            "exec",
            &wallet,
            "/driver",
            "http-json",
            &format!("{MINT}/v1/mint/quote/bolt11/{quote}"),
        ],
        |v| v["state"] == "PAID" || v["state"] == "ISSUED",
    )?;
    if paid["state"] == "PAID" {
        execute(
            d,
            "wallet",
            &format!(
                "{WALLET} mint {MINT} --quote-id {} >> /wallet/mint.log 2>&1",
                proofstorm_acceptance::native::quote(&quote)
            ),
        )?;
    }
    let issued = d.json(&[
        "exec",
        &wallet,
        "/driver",
        "http-json",
        &format!("{MINT}/v1/mint/quote/bolt11/{quote}"),
    ])?;
    ensure!(
        issued["state"] == "ISSUED" && issued["amount"] == 100_000,
        "mint quote was not issued"
    );
    d.save("mint-issued.json", &issued)?;
    let before = observe(d)?;
    settled_balance(&before, 100_000)?;
    d.save("wallet-minted.json", &before)?;

    // Verify the existing Ark wallet and quote survive a real processor restart.
    d.run(&["restart", "--time", "15", &d.name("processor")])?;
    d.run(&["restart", "--time", "15", &d.name("mint")])?;
    d.wait(&[
        "exec",
        &wallet,
        "/driver",
        "http-json",
        &format!("{MINT}/v1/info"),
    ])?;
    let recovered = d.json(&[
        "exec",
        &wallet,
        "/driver",
        "http-json",
        &format!("{MINT}/v1/mint/quote/bolt11/{quote}"),
    ])?;
    ensure!(
        recovered["state"] == "ISSUED" && recovered["quote"] == issued["quote"],
        "issued quote lost on restart"
    );
    d.save("mint-recovered.json", &recovered)?;
    settled_balance(&observe(d)?, 100_000)?;
    let invoice = cln(
        d,
        "peer",
        &["invoice", "30000000msat", "bark-melt", "Bark qualification"],
    )?;
    eprintln!("Melting 30,000 sat and checking the recipient and wallet conservation...");
    execute(
        d,
        "wallet",
        &format!(
            "{WALLET} melt --mint-url {MINT} --invoice {} > /wallet/melt.log 2>&1",
            proofstorm_acceptance::native::quote(text(&invoice, "/bolt11")?)
        ),
    )?;
    let receipt = d.json(&[
        "exec",
        &wallet,
        "/driver",
        "cdk-melt-receipt",
        "/wallet/melt.log",
    ])?;
    ensure!(
        receipt["state"] == "PAID" && receipt["amount_sat"] == 30_000,
        "native melt did not succeed"
    );
    let log = execute(d, "wallet", "cat /wallet/melt.log")?;
    let quotes: Vec<_> = log
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Quote ID: "))
        .collect();
    ensure!(quotes.len() == 1, "missing or ambiguous native melt quote");
    let melt_quote = d.json(&[
        "exec",
        &wallet,
        "/driver",
        "http-json",
        &format!("{MINT}/v1/melt/quote/bolt11/{}", quotes[0]),
    ])?;
    ensure!(
        melt_quote["state"] == "PAID" && melt_quote["amount"] == 30_000,
        "mint did not independently confirm melt settlement"
    );
    d.save("melt-quote.json", &melt_quote)?;
    let recipient = cln(d, "peer", &["listinvoices", "bark-melt"])?;
    let invoices = recipient["invoices"]
        .as_array()
        .context("missing recipient invoice list")?;
    ensure!(
        invoices.len() == 1
            && invoices[0]["status"] == "paid"
            && invoices[0]["payment_hash"] == invoice["payment_hash"]
            && invoices[0]["amount_received_msat"] == 30_000_000,
        "recipient settlement mismatch"
    );
    let fee = receipt["fee_paid_sat"]
        .as_u64()
        .context("missing melt fee")?;
    ensure!(
        fee <= melt_quote["fee_reserve"]
            .as_u64()
            .context("missing fee reserve")?,
        "melt exceeded its quoted fee reserve"
    );
    let remaining = 70_000_u64
        .checked_sub(fee)
        .context("melt fee exceeds balance")?;
    let after = observe(d)?;
    settled_balance(&after, remaining)?;
    d.save("melt-receipt.json", &receipt)?;
    d.save("melt-recipient.json", &recipient)?;
    d.save("wallet-after-melt.json", &after)?;
    Ok(
        json!({"mint_sat":100_000,"melt_sat":30_000,"fee_sat":fee,"remaining_sat":remaining,"recipient_settled":true,"processor_and_mint_restart":true,"wallet_conserved":true}),
    )
}
