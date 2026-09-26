//! Experimental image/dependency qualification, not a supported catalog cell.
#![cfg(unix)]
#![allow(
    clippy::too_many_lines,
    reason = "linear integration scenario keeps ownership and assertions visible"
)]
mod docker;
mod payments;

use anyhow::{Context, Result, ensure};
use docker::Docker;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use toml_edit::{DocumentMut, Item, Table, value};

fn random_password() -> Result<String> {
    let mut bytes = [0; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn certificates(docker: &Docker, role: &str) -> Result<BTreeMap<String, String>> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let key = KeyPair::generate()?;
    let ca = ca_params.self_signed(&key)?;
    let mut contents = BTreeMap::from([
        ("ca.pem".to_owned(), ca.pem()),
        ("ca-key.pem".to_owned(), key.serialize_pem()),
    ]);
    for (name, usage) in [
        ("server", ExtendedKeyUsagePurpose::ServerAuth),
        ("client", ExtendedKeyUsagePurpose::ClientAuth),
    ] {
        let mut params =
            CertificateParams::new(vec![role.into(), "localhost".into(), "127.0.0.1".into()])?;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![usage];
        let leaf_key = KeyPair::generate()?;
        let leaf = params.signed_by(&leaf_key, &ca, &key)?;
        contents.insert(format!("{name}.pem"), leaf.pem());
        contents.insert(format!("{name}-key.pem"), leaf_key.serialize_pem());
    }
    contents
        .into_iter()
        .map(|(file, contents)| {
            Ok((
                file.clone(),
                docker.file(&format!("{role}-{file}"), &contents)?,
            ))
        })
        .collect()
}

fn seed_tls(
    d: &mut Docker,
    image: &str,
    volume: &str,
    role: &str,
    files: &BTreeMap<String, String>,
) -> Result<()> {
    let helper = d.name(&format!("tls-{role}"));
    let mut args = vec![
        "create".into(),
        "--name".into(),
        helper.clone(),
        "--label".into(),
        d.label(),
        "--network".into(),
        "none".into(),
        "--user".into(),
        "0:0".into(),
        "--entrypoint".into(),
        "sh".into(),
        "-v".into(),
        format!("{volume}:/data"),
    ];
    for (name, path) in files {
        args.extend(["-v".into(), format!("{path}:/seed/{name}:ro")]);
    }
    let directory = if role == "cln" {
        "/data/regtest"
    } else {
        "/data/regtest/hold"
    };
    args.extend([image.into(), "-ec".into(), format!("mkdir -p {directory}; cp /seed/* {directory}/; chown -R 1000:1000 /data; chmod 600 {directory}/*key.pem")]);
    d.run(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    d.run(&["start", "-a", &helper])?;
    d.run(&["rm", "-v", &helper])?;
    Ok(())
}

fn catalog_image(id: &str) -> Result<String> {
    let catalog =
        proofstorm_core::catalog_for_platform(proofstorm_core::CatalogPlatform::LinuxArm64);
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .context("catalog image missing")?;
    proofstorm_core::catalog_image_source(&entry.image).map_err(anyhow::Error::msg)
}

fn inventory(d: &mut Docker) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for (kind, args) in [
        ("containers", vec!["ps", "-aq", "--no-trunc"]),
        ("volumes", vec!["volume", "ls", "-q"]),
        ("networks", vec!["network", "ls", "-q", "--no-trunc"]),
    ] {
        let output = d.run(&args)?;
        let mut ids: Vec<_> = output.lines().collect();
        ids.sort_unstable();
        result.insert(kind.into(), json!(ids));
    }
    Ok(Value::Object(result))
}

fn run(d: &mut Docker) -> Result<Value> {
    eprintln!("Checking native images and preparing private configuration...");
    ensure!(
        std::env::var("PROOFSTORM_BARK_DRIVER_IMAGE").is_ok_and(|s| !s.trim().is_empty()),
        "set PROOFSTORM_BARK_DRIVER_IMAGE to a local controller image containing /usr/local/lib/proofstorm-driver"
    );
    ensure!(
        d.run(&["info", "--format", "{{.OSType}}/{{.Architecture}}"])? == "linux/aarch64",
        "native ARM64 Docker host required"
    );
    let server_image = d.image("proofstorm-bark-server:dev-0.7", false)?;
    let cln_image = d.image("proofstorm-cln-hold:dev-0.3.3", false)?;
    let bitcoin_image = d.image(&catalog_image("bitcoin-core")?, true)?;
    let postgres_image = d.image(&catalog_image("postgresql")?, true)?;
    d.save("images.json", &json!({"server":server_image,"cln_hold":cln_image,"bitcoin":bitcoin_image,"postgresql":postgres_image}))?;
    let template = d.run(&[
        "run",
        "--rm",
        "--label",
        &d.label(),
        "--network",
        "none",
        "--entrypoint",
        "cat",
        &server_image,
        "/usr/local/share/bark/captaind.default.toml",
    ])?;
    let mut config = template.parse::<DocumentMut>()?;
    let chain_password = random_password()?;
    let pg_password = random_password()?;
    let cookie = d.file("rpc.cookie", &format!("proofstorm:{chain_password}"))?;
    let bitcoin_config = d.file("bitcoin.conf", &format!("regtest=1\nserver=1\ntxindex=1\nfallbackfee=0.00001\nrpcuser=proofstorm\nrpcpassword={chain_password}\n[regtest]\nrpcbind=0.0.0.0\nrpcallowip=0.0.0.0/0\n"))?;
    let pg_env = d.file(
        "postgres.env",
        &format!(
            "POSTGRES_USER=proofstorm\nPOSTGRES_PASSWORD={pg_password}\nPOSTGRES_DB=postgres\n"
        ),
    )?;
    let cln_config = d.file("lightning.conf", &format!("network=regtest\nlightning-dir=/data\nbitcoin-rpcconnect=chain\nbitcoin-rpcport=18443\nbitcoin-rpcuser=proofstorm\nbitcoin-rpcpassword={chain_password}\naddr=0.0.0.0:9735\ngrpc-host=0.0.0.0\ngrpc-port=9988\nplugin=/usr/local/bin/hold\nhold-grpc-host=0.0.0.0\nhold-grpc-port=9292\nhold-database=sqlite:///data/regtest/hold/hold.sqlite3\n"))?;
    let cln_tls = certificates(d, "cln")?;
    let hold_tls = certificates(d, "hold")?;
    config["data_dir"] = value("/data");
    config["network"] = value("regtest");
    config["rpc"]["public_address"] = value("0.0.0.0:3535");
    config["rpc"]["admin_address"] = value("127.0.0.1:3536");
    config["rpc"]["integration_address"] = value("127.0.0.1:3537");
    config["bitcoind"]["url"] = value("http://chain:18443");
    config["bitcoind"]["cookie"] = value("/chain-auth/rpc.cookie");
    config["postgres"]["host"] = value("postgres");
    config["postgres"]["user"] = value("proofstorm");
    config["postgres"]["password"] = value(pg_password);
    config["postgres"]["name"] = value("bark");
    let mut node = Table::new();
    node["uri"] = value("https://cln:9988");
    node["priority"] = value(0);
    for key in ["server_cert_path", "client_cert_path", "client_key_path"] {
        let file = match key {
            "server_cert_path" => "ca.pem",
            "client_cert_path" => "client.pem",
            _ => "client-key.pem",
        };
        node[key] = value(format!("/cln/{file}"));
    }
    let mut hold = Table::new();
    hold["uri"] = value("https://hold:9292");
    for (key, file) in [
        ("server_cert_path", "ca.pem"),
        ("client_cert_path", "client.pem"),
        ("client_key_path", "client-key.pem"),
    ] {
        hold[key] = value(format!("/hold/{file}"));
    }
    node["hold_invoice"] = Item::Table(hold);
    let mut nodes = toml_edit::ArrayOfTables::new();
    nodes.push(node);
    config["cln_array"] = Item::ArrayOfTables(nodes);
    let server_config = d.file("captaind.toml", &config.to_string())?;
    let network = d.name("net");
    d.run(&[
        "network",
        "create",
        "--internal",
        "--label",
        &d.label(),
        &network,
    ])?;
    let chain_volume = d.volume("chain-data", &server_image)?;
    let cln_volume = d.volume("cln-data", &server_image)?;
    let server_volume = d.volume("server-data", &server_image)?;
    let pg_volume = d.volume("pg-data", &server_image)?;
    seed_tls(d, &server_image, &cln_volume, "cln", &cln_tls)?;
    seed_tls(d, &server_image, &cln_volume, "hold", &hold_tls)?;
    let chain = d.name("chain");
    eprintln!("Starting owned Bitcoin, PostgreSQL and CLN/hold dependencies...");
    let pg = d.name("postgres");
    let cln = d.name("cln");
    let server = d.name("server");
    d.run(&[
        "create",
        "--name",
        &chain,
        "--label",
        &d.label(),
        "--network",
        &network,
        "--network-alias",
        "chain",
        "--user",
        "1000:1000",
        "--entrypoint",
        "bitcoind",
        "-v",
        &format!("{chain_volume}:/data"),
        "-v",
        &format!("{bitcoin_config}:/config/bitcoin.conf:ro"),
        &bitcoin_image,
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
        "-printtoconsole",
    ])?;
    d.run(&["start", &chain])?;
    d.wait(&[
        "exec",
        &chain,
        "bitcoin-cli",
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
        "getblockchaininfo",
    ])?;
    d.run(&[
        "exec",
        &chain,
        "bitcoin-cli",
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
        "createwallet",
        "miner",
    ])?;
    let address = d.run(&[
        "exec",
        &chain,
        "bitcoin-cli",
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
        "getnewaddress",
    ])?;
    d.run(&[
        "exec",
        &chain,
        "bitcoin-cli",
        "-conf=/config/bitcoin.conf",
        "-datadir=/data",
        "generatetoaddress",
        "110",
        &address,
    ])?;
    d.run(&[
        "create",
        "--name",
        &pg,
        "--label",
        &d.label(),
        "--network",
        &network,
        "--network-alias",
        "postgres",
        "--env-file",
        &pg_env,
        "-v",
        &format!("{pg_volume}:/var/lib/postgresql/data"),
        &postgres_image,
    ])?;
    d.run(&["start", &pg])?;
    d.wait(&[
        "exec",
        &pg,
        "pg_isready",
        "-U",
        "proofstorm",
        "-d",
        "postgres",
    ])?;
    d.run(&[
        "create",
        "--name",
        &cln,
        "--label",
        &d.label(),
        "--network",
        &network,
        "--network-alias",
        "cln",
        "--network-alias",
        "hold",
        "-v",
        &format!("{cln_volume}:/data"),
        "-v",
        &format!("{cln_config}:/config/lightning.conf:ro"),
        &cln_image,
    ])?;
    d.run(&["start", &cln])?;
    let cln_info: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &cln,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "getinfo",
    ])?)?;
    let payment_hash = format!("{:x}", Sha256::digest(d.owner.as_bytes()));
    let invoice = d.json(&[
        "exec",
        &cln,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "holdinvoice",
        &payment_hash,
        "1000",
    ])?;
    d.save("hold-invoice.json", &invoice)?;
    let held_before = d.json(&[
        "exec",
        &cln,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "listholdinvoices",
        &payment_hash,
    ])?;
    let mut server_args = vec![
        "create".into(),
        "--name".into(),
        server.clone(),
        "--label".into(),
        d.label(),
        "--network".into(),
        network.clone(),
        "--network-alias".into(),
        "bark-server".into(),
        "-v".into(),
        format!("{server_volume}:/data"),
        "-v".into(),
        format!("{server_config}:/config/captaind.toml:ro"),
        "-v".into(),
        format!("{cookie}:/chain-auth/rpc.cookie:ro"),
    ];
    for (role, files) in [("cln", &cln_tls), ("hold", &hold_tls)] {
        for name in ["ca.pem", "client.pem", "client-key.pem"] {
            server_args.extend(["-v".into(), format!("{}:/{role}/{name}:ro", files[name])]);
        }
    }
    // Create is deliberately a separate one-shot operation. A normal restart
    // always starts the existing identity and cannot implicitly initialize anew.
    server_args.push(server_image);
    let initializer = d.name("server-create");
    let mut initialize_args = server_args.clone();
    initialize_args[2].clone_from(&initializer);
    initialize_args.extend(["sh".into(), "-ec".into(), "captaind check-config /config/captaind.toml && exec captaind --config /config/captaind.toml create".into()]);
    eprintln!("Initializing Bark and checking duplicate-initialization refusal...");
    d.run(
        &initialize_args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )?;
    d.run(&["start", "-a", &initializer])?;
    let duplicate = d.raw(&["start", "-a", &initializer])?;
    ensure!(
        !duplicate.status.success()
            && String::from_utf8_lossy(&duplicate.stderr).contains("already initialized"),
        "duplicate server initialization not refused"
    );
    d.run(&["rm", "-v", &initializer])?;
    d.run(&server_args.iter().map(String::as_str).collect::<Vec<_>>())?;
    d.run(&["start", &server])?;
    let wallet_before: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &server,
        "captaind",
        "--config",
        "/config/captaind.toml",
        "rpc",
        "wallet",
    ])?)?;
    d.save("wallet-before.json", &wallet_before)?;
    let seed_before = d.run(&["exec", &server, "sh", "-ec", "sha256sum /data/mnemonic"])?;
    eprintln!("Checking CLN, hold-invoice, PostgreSQL and Bark restart preservation...");
    d.run(&["restart", "--time", "15", &cln])?;
    let cln_after: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &cln,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "getinfo",
    ])?)?;
    ensure!(
        cln_info["id"].is_string() && cln_info["id"] == cln_after["id"],
        "CLN identity changed"
    );
    let held_after = d.json(&[
        "exec",
        &cln,
        "lightning-cli",
        "--lightning-dir=/data",
        "--network=regtest",
        "listholdinvoices",
        &payment_hash,
    ])?;
    ensure!(
        held_before == held_after && held_after.to_string().contains(&payment_hash),
        "hold invoice state changed across restart"
    );
    d.run(&["restart", "--time", "15", &pg])?;
    d.wait(&["exec", &pg, "pg_isready", "-U", "proofstorm", "-d", "bark"])?;
    d.run(&["restart", "--time", "15", &server])?;
    let wallet_after: Value = serde_json::from_str(&d.wait(&[
        "exec",
        &server,
        "captaind",
        "--config",
        "/config/captaind.toml",
        "rpc",
        "wallet",
    ])?)?;
    let seed_after = d.run(&["exec", &server, "sh", "-ec", "sha256sum /data/mnemonic"])?;
    ensure!(seed_before == seed_after, "server mnemonic changed");
    d.save("wallet-after.json", &wallet_after)?;
    let payments = payments::run(
        d,
        &network,
        &cookie,
        &chain_password,
        &cln_image,
        &address,
        &wallet_after,
    )?;
    Ok(
        json!({"server_initialized":true,"duplicate_initialization_refused":true,"server_mnemonic_preserved":true,"cln_identity_preserved":true,"hold_invoice_preserved":true,"postgres_restart":true,"payments":payments}),
    )
}

fn probe(work: &Path, cancelled: Arc<AtomicBool>) -> Result<()> {
    let mut docker = Docker::new(work, cancelled)?;
    let before = inventory(&mut docker)?;
    docker.save("inventory-before.json", &before)?;
    let result = run(&mut docker);
    eprintln!("Removing owned resources and checking resource inventory...");
    let cleanup = docker.cleanup();
    let after = inventory(&mut docker)?;
    docker.save("inventory-after.json", &after)?;
    docker.save("result.json", &json!({"checks":result.as_ref().ok(),"error":result.as_ref().err().map(ToString::to_string),"cleanup_passed":cleanup.is_ok(),"resource_inventory_preserved":before==after,"scope":"experimental native ARM64 Bark payment qualification; not a catalog cell or full preservation qualification"}))?;
    result?;
    cleanup?;
    ensure!(
        before == after,
        "resource inventory changed; inspect snapshots"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let work = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: bark_stack NEW_PRIVATE_WORK_DIRECTORY")?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let mut worker = tokio::task::spawn_blocking(move || probe(&work, flag));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = &mut worker => result?,
        _ = tokio::signal::ctrl_c() => { cancelled.store(true, Ordering::SeqCst); worker.await? },
        _ = terminate.recv() => { cancelled.store(true, Ordering::SeqCst); worker.await? },
    }
}
