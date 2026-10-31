//! Start the native Spark federation, SSP and payment processor from
//! controller-generated Secrets. Native programs own their state; these commands
//! validate the shared federation description and bind owned volumes to their seed.
use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    os::unix::{fs::FileTypeExt as _, process::CommandExt as _},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};

use crate::{
    bark::{mnemonic, read_bounded},
    http::required,
};

const OPERATORS: &str = "/spark-operators";
const CA: &str = "/spark-operators/ca.crt";
const SOCKET: &str = "/run/spark/frost.sock";
const OPERATOR_PORT: u16 = 8535;
const SSP_PORT: u16 = 8536;
const MAX_OPERATORS: usize = 5;
const SSP_MNEMONIC: &str = "/ssp-identity/mnemonic";
const ADMIN_TOKEN: &str = "/ssp-identity/admin-token";
const SSP_MARKER: &str = ".proofstorm-open-ssp-identity";
/// open-ssp chmods its seed file to 0600, which a Secret mount refuses, so it
/// reads a private copy kept in memory beside the Secret.
const SSP_SEED: &str = "/run/open-ssp/spark.mnemonic";
/// Funding recipes send the admin token with `curl -H @<file>`, so it never
/// appears in a command line.
const ADMIN_HEADER: &str = "/run/open-ssp/admin.header";
const PROCESSOR_MARKER: &str = ".proofstorm-spark-processor-identity";
const PROCESSOR_RUN: &str = "/run/spark-processor";
const PROCESSOR_TLS: [&str; 3] = [
    "/processor-server/tls/ca.pem",
    "/processor-server/tls/server.pem",
    "/processor-server/tls/server.key",
];

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: usize,
    address: String,
    external_address: String,
    identity_public_key: String,
    cert_path: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Operator {
    host: String,
    public_key: String,
}

/// The public federation description shared by operators, SSP and processor.
#[derive(Debug, PartialEq, Eq)]
struct Federation {
    /// In index order; each operator serves 8535 and its SSP service on 8536.
    operators: Vec<Operator>,
    threshold: usize,
}

/// A canonical unsigned decimal: digits only, no sign and no leading zero.
fn decimal(value: &str) -> Option<usize> {
    if !value.is_empty()
        && value.len() <= 5
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
    {
        value.parse().ok()
    } else {
        None
    }
}

fn tcp_port(value: &str) -> Option<u16> {
    decimal(value)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|&port| port != 0)
}

/// Lowercase DNS labels, as Kubernetes service and pod names use.
fn dns_name(value: &str) -> bool {
    value.len() <= 253
        && value.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn host_port(value: &str) -> Option<(&str, u16)> {
    let (host, port) = value.rsplit_once(':')?;
    dns_name(host).then_some((host, tcp_port(port)?))
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// A compressed secp256k1 public key in lowercase hex.
fn public_key(value: &str) -> bool {
    lower_hex(value, 66) && (value.starts_with("02") || value.starts_with("03"))
}

/// Generated text files end in at most one newline.
fn line(bytes: &[u8]) -> Option<&str> {
    let value = std::str::from_utf8(bytes).ok()?;
    Some(value.strip_suffix('\n').unwrap_or(value))
}

/// PostgreSQL folds unquoted identifiers and limits them to 63 bytes.
fn postgres_identifier(value: &str) -> bool {
    value.len() <= 63
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_federation(list: &[u8], threshold: &[u8]) -> Result<Federation> {
    let entries: Vec<Entry> =
        serde_json::from_slice(list).context("parse the Spark operator list")?;
    ensure!(
        (1..=MAX_OPERATORS).contains(&entries.len()),
        "a Spark federation has 1 to {MAX_OPERATORS} operators"
    );
    let mut operators: Vec<Operator> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let host = host_port(&entry.address)
            .filter(|&(_, port)| port == OPERATOR_PORT)
            .filter(|_| {
                entry.id == index
                    && host_port(&entry.external_address).is_some()
                    && public_key(&entry.identity_public_key)
                    && entry.cert_path == CA
            })
            .with_context(|| format!("Spark operator {index} is not a valid federation entry"))?
            .0
            .to_owned();
        ensure!(
            operators
                .iter()
                .all(|other| other.host != host && other.public_key != entry.identity_public_key),
            "Spark operators must have distinct addresses and identities"
        );
        operators.push(Operator {
            host,
            public_key: entry.identity_public_key,
        });
    }
    let threshold = line(threshold)
        .and_then(decimal)
        .filter(|threshold| (1..=operators.len()).contains(threshold))
        .context("the Spark threshold must be between 1 and the operator count")?;
    Ok(Federation {
        operators,
        threshold,
    })
}

fn federation(directory: &Path) -> Result<Federation> {
    parse_federation(
        &read_bounded(&directory.join("operators.json"), 16_384)?,
        &read_bounded(&directory.join("threshold"), 16)?,
    )
}

/// Operator `i` runs in pod `{svc}-{i}` and is listed as `{svc}-{i}.{svc}`.
fn operator_index(hostname: &str, federation: &Federation) -> Option<usize> {
    let (service, ordinal) = hostname.rsplit_once('-')?;
    let index = decimal(ordinal)?;
    (dns_name(service) && federation.operators.get(index)?.host == format!("{hostname}.{service}"))
        .then_some(index)
}

/// The private key is passed to the operator as a path; only its shape is checked.
fn operator_identity(directory: &Path, index: usize, federation: &Federation) -> Result<()> {
    let key = read_bounded(&directory.join(format!("operator-{index}.key")), 128)?;
    ensure!(
        line(&key).is_some_and(|key| lower_hex(key, 64)),
        "Spark operator identity key is malformed"
    );
    let public = read_bounded(&directory.join(format!("operator-{index}.pub")), 128)?;
    ensure!(
        line(&public) == Some(federation.operators[index].public_key.as_str()),
        "Spark operator identity does not match the federation list"
    );
    Ok(())
}

/// Main and ephemeral database URLs. The password reaches atlas and the operator
/// only through the inherited `PGPASSWORD`, never a URL or argument.
fn database_urls(host: &str, port: &str, base: &str, index: usize) -> Result<[String; 2]> {
    let port = tcp_port(port).context("invalid Spark database port")?;
    ensure!(dns_name(host), "invalid Spark database host");
    let main = format!("{base}_{index}");
    let ephemeral = format!("{main}_ephemeral");
    ensure!(
        postgres_identifier(base) && postgres_identifier(&ephemeral),
        "invalid Spark database name"
    );
    Ok([main, ephemeral].map(|database| {
        format!("postgresql://proofstorm@{host}:{port}/{database}?sslmode=disable")
    }))
}

fn operator_arguments(index: usize, threshold: usize, databases: &[String; 2]) -> Vec<String> {
    [
        "-config",
        "/config/operator.config.yaml",
        "-index",
        &index.to_string(),
        "-key",
        &format!("{OPERATORS}/operator-{index}.key"),
        "-operators",
        &format!("{OPERATORS}/operators.json"),
        "-threshold",
        &threshold.to_string(),
        "-signer",
        &format!("unix://{SOCKET}"),
        "-port",
        &OPERATOR_PORT.to_string(),
        "-database",
        &databases[0],
        "-ephemeral-database",
        &databases[1],
        "-server-cert",
        &format!("{OPERATORS}/server-{index}.crt"),
        "-server-key",
        &format!("{OPERATORS}/server-{index}.key"),
        "-ssp-grpc-port",
        &SSP_PORT.to_string(),
        "-local",
        "true",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket(),
                "Spark signer socket path holds something other than a socket"
            );
            fs::remove_file(path).context("remove stale Spark signer socket")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect Spark signer socket"),
    }
}

fn wait_for_socket(path: &Path, limit: Duration) -> Result<()> {
    let deadline = Instant::now() + limit;
    loop {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Spark FROST signer did not create its socket"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Replace a stale socket from a previous signer, then exec the FROST signer.
/// # Errors
/// Refuses a socket path holding other data and a failed exec.
pub fn exec_signer() -> Result<()> {
    remove_stale_socket(Path::new(SOCKET))?;
    Err(Command::new("spark-frost-signer")
        .args(["-u", SOCKET])
        .exec())
    .context("execute Spark FROST signer")
}

/// Migrate this pod's operator databases, wait for its signer and exec the operator.
/// # Errors
/// Refuses an invalid federation, pod identity or database input, failed
/// migrations, an absent signer and a failed exec.
pub fn exec_operator() -> Result<()> {
    let directory = Path::new(OPERATORS);
    let federation = federation(directory)?;
    let index = operator_index(&required("HOSTNAME")?, &federation)
        .context("this pod is not an operator of the mounted Spark federation")?;
    operator_identity(directory, index, &federation)?;
    let databases = database_urls(
        &required("PROOFSTORM_SPARK_DATABASE_HOST")?,
        &required("PROOFSTORM_SPARK_DATABASE_PORT")?,
        &required("PROOFSTORM_SPARK_DATABASE_NAME")?,
        index,
    )?;
    // Inherited by atlas and the operator; only its presence is checked here.
    required("PGPASSWORD")?;
    for (migrations, url) in [
        "file:///opt/spark/migrations",
        "file:///opt/spark/ephemeral_migrations",
    ]
    .into_iter()
    .zip(&databases)
    {
        let status = Command::new("atlas")
            .args(["migrate", "apply", "--dir", migrations, "--url", url])
            .status()
            .context("run Spark operator database migrations")?;
        ensure!(status.success(), "Spark operator database migration failed");
    }
    wait_for_socket(Path::new(SOCKET), Duration::from_secs(30))?;
    Err(Command::new("spark-operator")
        .args(operator_arguments(index, federation.threshold, &databases))
        .exec())
    .context("execute Spark operator")
}

#[derive(Clone, PartialEq, prost::Message)]
struct HealthCheckRequest {
    #[prost(string, tag = "1")]
    service: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct HealthCheckResponse {
    #[prost(int32, tag = "1")]
    status: i32,
}

const SERVING: i32 = 1;

async fn operator_ready_at(address: &str, ca: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(read_bounded(ca, 65_536)?))
            .domain_name("localhost");
        let channel = Endpoint::from_shared(address.to_owned())?
            .tls_config(tls)?
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(1))
            .connect()
            .await?;
        let mut client = tonic::client::Grpc::new(channel).max_decoding_message_size(1024);
        client.ready().await?;
        let response = client
            .unary(
                tonic::Request::new(HealthCheckRequest {
                    service: "spark.SparkService".into(),
                }),
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/grpc.health.v1.Health/Check",
                ),
                tonic_prost::ProstCodec::<HealthCheckRequest, HealthCheckResponse>::default(),
            )
            .await?;
        ensure!(
            response.into_inner().status == SERVING,
            "Spark operator is not serving"
        );
        Ok(())
    })
    .await
    .context("Spark operator readiness deadline")?
}

/// Check the local operator's gRPC health over the federation's TLS.
/// # Errors
/// Rejects untrusted servers, failed RPCs, any status but SERVING and timeouts.
pub async fn operator_ready() -> Result<()> {
    operator_ready_at("https://localhost:8535", Path::new(CA)).await
}

fn admin_token(path: &Path) -> Result<String> {
    let bytes = read_bounded(path, 128)?;
    line(&bytes)
        .filter(|token| lower_hex(token, 64))
        .map(str::to_owned)
        .context("open-ssp admin token is malformed")
}

fn identity_record(kind: &str, mnemonic: &str) -> String {
    format!(
        "proofstorm-{kind}/v1\n{:x}\n",
        Sha256::digest(mnemonic.as_bytes())
    )
}

/// Bind an owned volume to its seed once, as Bark does. The native layout is
/// upstream's, so a bound volume is checked only by its marker.
fn bind_identity(data: &Path, marker: &str, record: &str) -> Result<()> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "Spark data volume must be an existing directory"
    );
    let marker = data.join(marker);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "Spark identity marker must be a regular file"
            );
            ensure!(
                read_bounded(&marker, 128)? == record.as_bytes(),
                "Spark identity mismatch; refusing to replace existing state"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for entry in fs::read_dir(data)? {
                let entry = entry?;
                // A freshly formatted PVC can contain an empty filesystem recovery directory.
                ensure!(
                    entry.file_name() == "lost+found"
                        && entry.file_type()?.is_dir()
                        && fs::read_dir(entry.path())?.next().is_none(),
                    "Spark state exists without its identity marker; refusing reinitialization"
                );
            }
            let mut temporary = tempfile::NamedTempFile::new_in(data)?;
            temporary.write_all(record.as_bytes())?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&marker)
                .context("bind Spark identity once")?;
            fs::File::open(data)?.sync_all()?;
        }
        Err(error) => return Err(error).context("inspect Spark identity marker"),
    }
    Ok(())
}

fn joined(federation: &Federation, item: impl Fn(&Operator) -> String) -> String {
    federation
        .operators
        .iter()
        .map(item)
        .collect::<Vec<_>>()
        .join(",")
}

/// Variables set over Kubernetes' rendered environment for open-ssp.
fn admin_header(token: &str) -> String {
    format!("Authorization: Bearer {token}\n")
}

fn ssp_environment(federation: &Federation, token: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "SO_HOSTS",
            joined(federation, |operator| {
                format!("{}:{OPERATOR_PORT}", operator.host)
            }),
        ),
        (
            "SO_IDENTITY_PUBKEYS",
            joined(federation, |operator| operator.public_key.clone()),
        ),
        ("SO_CERT_FILES", joined(federation, |_| CA.to_owned())),
        (
            "SSP_OPERATOR_HOSTS",
            joined(federation, |operator| {
                format!("{}:{SSP_PORT}", operator.host)
            }),
        ),
        (
            "SSP_OPERATOR_CERT_FILES",
            joined(federation, |_| CA.to_owned()),
        ),
        ("SSP_FROST_THRESHOLD", federation.threshold.to_string()),
        ("SPARK_ADMIN_TOKEN", token.to_owned()),
        ("SPARK_MNEMONIC_FILE", SSP_SEED.to_owned()),
        ("SPARK_MNEMONIC_REQUIRED", "1".to_owned()),
    ]
}

/// Bind the SSP volume to its seed and exec open-ssp against the federation.
/// open-ssp reads the seed file itself and must not create a new identity. It
/// refuses a seed file it cannot restrict to mode 0600, and the Secret projection
/// is root-owned, group-readable and read-only, so it reads a private copy.
/// # Errors
/// Refuses an invalid federation, token or seed, changed state and a failed exec.
pub fn exec_open_ssp() -> Result<()> {
    let federation = federation(Path::new(OPERATORS))?;
    let token = admin_token(Path::new(ADMIN_TOKEN))?;
    let seed = mnemonic(Path::new(SSP_MNEMONIC))?;
    bind_identity(
        Path::new("/data"),
        SSP_MARKER,
        &identity_record("open-ssp", &seed),
    )?;
    replace_private(Path::new(SSP_SEED), format!("{seed}\n").as_bytes())?;
    replace_private(Path::new(ADMIN_HEADER), admin_header(&token).as_bytes())?;
    Err(Command::new("open-ssp")
        .envs(ssp_environment(&federation, &token))
        .exec())
    .context("execute open-ssp")
}

fn ssp_identity(value: &Value) -> Option<String> {
    value["identityPublicKey"]
        .as_str()
        .filter(|key| public_key(key))
        .map(str::to_owned)
}

/// Live only with its Lightning backend up, both identities matching the
/// served one, and no Spark wallet error.
fn ssp_live(identity: &str, status: &Value) -> bool {
    status["ldk_mode"] == "live"
        && status["ssp_identity_pubkey"] == identity
        && status["spark"]["identity_pubkey"] == identity
        && (status["spark_error"].is_null() || status["spark_error"] == "")
}

async fn open_ssp_ready_at(base: &str, token: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let client = crate::http::client(Duration::from_secs(3))?;
        let (status, value) = crate::http::json(client.get(format!("{base}/identity"))).await?;
        let identity = ssp_identity(&value)
            .filter(|_| status.is_success())
            .context("open-ssp identity unavailable")?;
        let (status, value) =
            crate::http::json(client.get(format!("{base}/status")).bearer_auth(token)).await?;
        ensure!(
            status.is_success() && ssp_live(&identity, &value),
            "open-ssp is not live"
        );
        Ok(())
    })
    .await
    .context("open-ssp readiness deadline")?
}

/// Check that the local SSP serves its own identity and is live.
/// # Errors
/// Rejects an invalid token, failed requests, mismatched identities and timeouts.
pub async fn open_ssp_ready() -> Result<()> {
    open_ssp_ready_at(
        "http://127.0.0.1:5000",
        &admin_token(Path::new(ADMIN_TOKEN))?,
    )
    .await
}

/// Exactly `http://{host}:{port}`, so it is safe as a URL base and a config value.
fn ssp_url(value: &str) -> Result<&str> {
    value
        .strip_prefix("http://")
        .and_then(host_port)
        .map(|_| value)
        .context("PROOFSTORM_SPARK_SSP_URL must be http://{host}:{port}")
}

async fn fetch_ssp_identity(base: &str, limit: Duration) -> Result<String> {
    let client = crate::http::client(Duration::from_secs(2))?;
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if let Ok((status, value)) = crate::http::json(client.get(format!("{base}/identity"))).await
            && status.is_success()
            && let Some(identity) = ssp_identity(&value)
        {
            return Ok(identity);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "open-ssp identity did not become available"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The processor reads `config.toml` from its working directory. Every value is
/// validated above, and JSON string escapes are valid TOML basic strings.
fn processor_config(port: u16, federation: &Federation, ssp: &str, identity: &str) -> String {
    let quote = |value: &str| serde_json::to_string(value).expect("serialize a TOML string");
    let mut text = format!(
        "address = \"0.0.0.0\"\n\
         port = {port}\n\
         tls_enable = true\n\
         allow_insecure = false\n\
         tls_cert_path = \"/processor-server/tls/server.pem\"\n\
         tls_key_path = \"/processor-server/tls/server.key\"\n\
         tls_client_ca_path = \"/processor-server/tls/ca.pem\"\n\
         \n\
         [spark]\n\
         network = \"regtest\"\n\
         data_dir = \"/data/spark\"\n\
         split_secret_threshold = {}\n",
        federation.threshold
    );
    for (index, operator) in federation.operators.iter().enumerate() {
        write!(
            text,
            "\n[[spark.operators]]\n\
             address = {}\n\
             identifier = \"{:064x}\"\n\
             identity_public_key = {}\n\
             ca_cert_path = {}\n",
            quote(&format!("https://{}:{OPERATOR_PORT}", operator.host)),
            index + 1,
            quote(&operator.public_key),
            quote(CA),
        )
        .expect("write processor configuration");
    }
    write!(
        text,
        "\n[spark.ssp]\n\
         base_url = {}\n\
         identity_public_key = {}\n\
         schema_endpoint = \"graphql/spark/rc\"\n",
        quote(ssp),
        quote(identity),
    )
    .expect("write processor configuration");
    text
}

/// Atomically replace a file with one only its owner can read (mode 0600).
fn replace_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("Spark runtime directory missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("replace Spark runtime file")?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Bind the processor volume to its seed, configure it against the federation
/// and SSP, and exec it. The seed reaches it only through its environment.
/// # Errors
/// Refuses invalid inputs, changed state, an SSP without an identity after 60 s
/// and a failed exec.
pub async fn exec_processor() -> Result<()> {
    let ssp = required("PROOFSTORM_SPARK_SSP_URL")?;
    let ssp = ssp_url(&ssp)?;
    let port = tcp_port(&required("PROOFSTORM_SPARK_PROCESSOR_PORT")?)
        .context("invalid Spark processor port")?;
    let federation = federation(Path::new(OPERATORS))?;
    let seed = mnemonic(Path::new("/processor-identity/mnemonic"))?;
    for path in PROCESSOR_TLS.into_iter().chain([CA]) {
        ensure!(
            !read_bounded(Path::new(path), 65_536)?.is_empty(),
            "required Spark processor credential file is empty"
        );
    }
    bind_identity(
        Path::new("/data"),
        PROCESSOR_MARKER,
        &identity_record("spark-processor", &seed),
    )?;
    let identity = fetch_ssp_identity(ssp, Duration::from_secs(60)).await?;
    replace_private(
        &Path::new(PROCESSOR_RUN).join("config.toml"),
        processor_config(port, &federation, ssp, &identity).as_bytes(),
    )?;
    Err(Command::new("cdk-payment-processor-spark")
        .current_dir(PROCESSOR_RUN)
        .env("SPARK_MNEMONIC", seed)
        .exec())
    .context("execute native Spark payment processor")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn key(index: usize) -> String {
        format!("0{}{index:064x}", 2 + index % 2)
    }

    fn list(count: usize) -> Vec<u8> {
        serde_json::to_vec(
            &(0..count)
                .map(|id| {
                    json!({
                        "id": id,
                        "address": format!("ops-{id}.ops:8535"),
                        "external_address": format!("ops-{id}.ops:8535"),
                        "identity_public_key": key(id),
                        "cert_path": CA,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn three() -> Federation {
        parse_federation(&list(3), b"2\n").unwrap()
    }

    fn seed(byte: u8) -> String {
        bip39::Mnemonic::from_entropy(&[byte; 16])
            .unwrap()
            .to_string()
    }

    #[test]
    fn federation_list_is_validated_and_fails_closed() {
        let federation = three();
        assert_eq!(federation.threshold, 2);
        assert_eq!(federation.operators[2].host, "ops-2.ops");
        assert_eq!(federation.operators[2].public_key, key(2));
        for count in 1..=5 {
            assert!(parse_federation(&list(count), b"1").is_ok());
            assert!(parse_federation(&list(count), count.to_string().as_bytes()).is_ok());
        }
        for (operators, threshold) in [
            (list(0), "1\n"),
            (list(6), "1\n"),
            (list(3), "0\n"),
            (list(3), "4\n"),
            (list(3), "02\n"),
            (list(3), " 2\n"),
            (list(3), "2\n\n"),
            (list(3), ""),
            (b"not json".to_vec(), "1\n"),
        ] {
            assert!(parse_federation(&operators, threshold.as_bytes()).is_err());
        }
        let operators: Value = serde_json::from_slice(&list(3)).unwrap();
        for (field, value) in [
            ("id", Some(json!(2))),
            ("address", Some(json!("ops-1.ops:8536"))),
            ("address", Some(json!("ops-1.ops"))),
            ("address", Some(json!("Ops-1.ops:8535"))),
            ("address", Some(json!("ops_1.ops:8535"))),
            ("address", Some(json!("-ops.ops:8535"))),
            ("address", Some(json!("ops-0.ops:8535"))),
            ("address", Some(json!("https://ops-1.ops:8535"))),
            ("external_address", Some(json!("ops-1.ops:0"))),
            ("external_address", None),
            ("identity_public_key", Some(json!(key(0)))),
            (
                "identity_public_key",
                Some(json!(format!("03{}", "AB".repeat(32)))),
            ),
            (
                "identity_public_key",
                Some(json!(format!("04{}", &key(1)[2..]))),
            ),
            ("identity_public_key", Some(json!(&key(1)[..64]))),
            ("cert_path", Some(json!("/elsewhere/ca.crt"))),
            ("extra", Some(json!(true))),
        ] {
            let changed = changed(&operators, &["1", field], value);
            let result = parse_federation(&serde_json::to_vec(&changed).unwrap(), b"2");
            assert!(result.is_err(), "{changed}");
        }
    }

    /// A copy with the value at `path` replaced, or removed when `None`.
    fn changed(original: &Value, path: &[&str], value: Option<Value>) -> Value {
        let mut copy = original.clone();
        let (last, parents) = path.split_last().unwrap();
        let parent = parents.iter().fold(&mut copy, |value, key| match value {
            Value::Array(items) => &mut items[key.parse::<usize>().unwrap()],
            value => &mut value[*key],
        });
        match value {
            Some(value) => parent[*last] = value,
            None => {
                parent.as_object_mut().unwrap().remove(*last);
            }
        }
        copy
    }

    #[test]
    fn operator_index_is_the_statefulset_ordinal_of_a_listed_pod() {
        let federation = three();
        assert_eq!(operator_index("ops-0", &federation), Some(0));
        assert_eq!(operator_index("ops-2", &federation), Some(2));
        for hostname in [
            "ops-3",
            "ops-02",
            "ops-+1",
            "ops-",
            "ops",
            "-1",
            "other-1",
            "ops.ops-1",
            "OPS-1",
            "",
        ] {
            assert_eq!(operator_index(hostname, &federation), None, "{hostname}");
        }
        let long = parse_federation(
            &serde_json::to_vec(&json!([{
                "id": 0,
                "address": "spark-operators-0.spark-operators:8535",
                "external_address": "spark-operators-0.spark-operators:8535",
                "identity_public_key": key(0),
                "cert_path": CA,
            }]))
            .unwrap(),
            b"1\n",
        )
        .unwrap();
        assert_eq!(operator_index("spark-operators-0", &long), Some(0));
        assert_eq!(operator_index("operators-0", &long), None);
    }

    #[test]
    fn operator_identity_files_must_match_the_federation_list() {
        let dir = tempfile::tempdir().unwrap();
        let federation = three();
        assert!(operator_identity(dir.path(), 1, &federation).is_err());
        fs::write(
            dir.path().join("operator-1.key"),
            format!("{}\n", "ab".repeat(32)),
        )
        .unwrap();
        fs::write(dir.path().join("operator-1.pub"), format!("{}\n", key(1))).unwrap();
        operator_identity(dir.path(), 1, &federation).unwrap();
        fs::write(dir.path().join("operator-1.pub"), format!("{}\n", key(0))).unwrap();
        assert!(operator_identity(dir.path(), 1, &federation).is_err());
        fs::write(dir.path().join("operator-1.pub"), key(1)).unwrap();
        for private in [
            "AB".repeat(32),
            "ab".repeat(31),
            "ab".repeat(33),
            String::new(),
        ] {
            fs::write(dir.path().join("operator-1.key"), &private).unwrap();
            let error = format!(
                "{:#}",
                operator_identity(dir.path(), 1, &federation).unwrap_err()
            );
            assert!(private.is_empty() || !error.contains(&private));
        }
    }

    #[test]
    fn operator_databases_and_arguments_follow_the_contract_without_a_password() {
        let databases = database_urls("postgres", "5432", "spark_operators", 2).unwrap();
        assert_eq!(
            databases,
            [
                "postgresql://proofstorm@postgres:5432/spark_operators_2?sslmode=disable",
                "postgresql://proofstorm@postgres:5432/spark_operators_2_ephemeral?sslmode=disable",
            ]
        );
        let longest = "a".repeat(63 - "_4_ephemeral".len());
        assert!(database_urls("postgres", "5432", &longest, 4).is_ok());
        for (host, port, base) in [
            ("postgres", "5432", format!("{longest}a")),
            ("postgres", "5432", "Spark".into()),
            ("postgres", "5432", "1spark".into()),
            ("postgres", "5432", "spark-operators".into()),
            ("postgres", "5432", String::new()),
            ("postgres", "0", "spark".into()),
            ("postgres", "65536", "spark".into()),
            ("postgres", "05432", "spark".into()),
            ("user:secret@postgres", "5432", "spark".into()),
            ("postgres/other", "5432", "spark".into()),
            ("", "5432", "spark".into()),
        ] {
            assert!(
                database_urls(host, port, &base, 0).is_err(),
                "{host} {port} {base}"
            );
        }
        assert_eq!(
            operator_arguments(2, 3, &databases).join(" "),
            "-config /config/operator.config.yaml -index 2 \
             -key /spark-operators/operator-2.key -operators /spark-operators/operators.json \
             -threshold 3 -signer unix:///run/spark/frost.sock -port 8535 \
             -database postgresql://proofstorm@postgres:5432/spark_operators_2?sslmode=disable \
             -ephemeral-database postgresql://proofstorm@postgres:5432/spark_operators_2_ephemeral?sslmode=disable \
             -server-cert /spark-operators/server-2.crt -server-key /spark-operators/server-2.key \
             -ssp-grpc-port 8536 -local true"
        );
    }

    #[test]
    fn signer_socket_is_replaced_only_when_it_is_a_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("frost.sock");
        remove_stale_socket(&socket).unwrap();
        let start = Instant::now();
        assert!(wait_for_socket(&socket, Duration::from_millis(300)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        wait_for_socket(&socket, Duration::from_secs(1)).unwrap();
        drop(listener);
        remove_stale_socket(&socket).unwrap();
        assert!(!socket.exists());
        fs::write(&socket, b"data").unwrap();
        assert!(wait_for_socket(&socket, Duration::from_millis(300)).is_err());
        assert!(remove_stale_socket(&socket).is_err());
        assert_eq!(fs::read(&socket).unwrap(), b"data");
    }

    #[test]
    fn open_ssp_environment_lists_every_operator_in_index_order() {
        let token = "ab".repeat(32);
        let environment = ssp_environment(&three(), &token);
        let value = |name: &str| {
            environment
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.as_str())
                .unwrap()
        };
        assert_eq!(environment.len(), 9);
        assert_eq!(
            value("SO_HOSTS"),
            "ops-0.ops:8535,ops-1.ops:8535,ops-2.ops:8535"
        );
        assert_eq!(
            value("SO_IDENTITY_PUBKEYS"),
            format!("{},{},{}", key(0), key(1), key(2))
        );
        let certificates = [CA; 3].join(",");
        assert_eq!(value("SO_CERT_FILES"), certificates);
        assert_eq!(
            value("SSP_OPERATOR_HOSTS"),
            "ops-0.ops:8536,ops-1.ops:8536,ops-2.ops:8536"
        );
        assert_eq!(value("SSP_OPERATOR_CERT_FILES"), certificates);
        assert_eq!(value("SSP_FROST_THRESHOLD"), "2");
        assert_eq!(value("SPARK_ADMIN_TOKEN"), token);
        assert_eq!(value("SPARK_MNEMONIC_FILE"), "/run/open-ssp/spark.mnemonic");
        assert_eq!(value("SPARK_MNEMONIC_REQUIRED"), "1");
    }

    #[test]
    fn admin_token_is_bounded_hex_and_never_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-token");
        assert!(admin_token(&path).is_err());
        let token = "0f".repeat(32);
        fs::write(&path, &token).unwrap();
        assert_eq!(admin_token(&path).unwrap(), token);
        fs::write(&path, format!("{token}\n")).unwrap();
        assert_eq!(admin_token(&path).unwrap(), token);
        for value in [
            token.to_uppercase(),
            format!("{token}0"),
            format!("{token}\n\n"),
            format!(" {token}"),
            "x".repeat(200),
        ] {
            fs::write(&path, &value).unwrap();
            let error = format!("{:#}", admin_token(&path).unwrap_err());
            assert!(!error.contains(&value));
        }
        // curl reads this file as one header line.
        assert_eq!(
            admin_header(&token),
            format!("Authorization: Bearer {token}\n")
        );
        assert_eq!(ADMIN_HEADER, "/run/open-ssp/admin.header");
    }

    #[test]
    fn data_binding_survives_restarts_and_refuses_other_seeds_or_unbound_state() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("lost+found")).unwrap();
        let record = identity_record("open-ssp", &seed(1));
        assert!(!record.contains(&seed(1)));
        assert_ne!(record, identity_record("spark-processor", &seed(1)));
        bind_identity(dir.path(), SSP_MARKER, &record).unwrap();
        fs::write(dir.path().join("native-state"), b"retained").unwrap();
        for _ in 0..3 {
            bind_identity(dir.path(), SSP_MARKER, &record).unwrap();
        }
        assert_eq!(
            fs::read(dir.path().join(SSP_MARKER)).unwrap(),
            record.as_bytes()
        );
        assert_eq!(
            fs::read(dir.path().join("native-state")).unwrap(),
            b"retained"
        );
        let other = identity_record("open-ssp", &seed(2));
        assert!(bind_identity(dir.path(), SSP_MARKER, &other).is_err());
        // A volume bound for one component is not adopted by the other.
        assert!(bind_identity(dir.path(), PROCESSOR_MARKER, &record).is_err());
        assert_eq!(
            fs::read(dir.path().join(SSP_MARKER)).unwrap(),
            record.as_bytes()
        );

        let unbound = tempfile::tempdir().unwrap();
        fs::write(unbound.path().join("spark"), b"existing").unwrap();
        assert!(bind_identity(unbound.path(), SSP_MARKER, &record).is_err());
        assert!(!unbound.path().join(SSP_MARKER).exists());
        let recovered = tempfile::tempdir().unwrap();
        fs::create_dir_all(recovered.path().join("lost+found/file")).unwrap();
        assert!(bind_identity(recovered.path(), SSP_MARKER, &record).is_err());

        let redirected = tempfile::tempdir().unwrap();
        let elsewhere = redirected.path().join("elsewhere");
        fs::write(&elsewhere, &record).unwrap();
        std::os::unix::fs::symlink(&elsewhere, redirected.path().join(SSP_MARKER)).unwrap();
        assert!(bind_identity(redirected.path(), SSP_MARKER, &record).is_err());
        assert!(bind_identity(&elsewhere, SSP_MARKER, &record).is_err());
    }

    #[test]
    fn runtime_files_are_replaced_atomically_and_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spark.mnemonic");
        fs::write(&path, b"stale").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        replace_private(&path, b"fresh\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh\n");
        // open-ssp refuses a seed file whose mode it cannot narrow to its owner.
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn processor_config_matches_the_contract_text() {
        let identity = key(7);
        let federation = parse_federation(&list(2), b"2\n").unwrap();
        let expected = format!(
            r#"address = "0.0.0.0"
port = 50051
tls_enable = true
allow_insecure = false
tls_cert_path = "/processor-server/tls/server.pem"
tls_key_path = "/processor-server/tls/server.key"
tls_client_ca_path = "/processor-server/tls/ca.pem"

[spark]
network = "regtest"
data_dir = "/data/spark"
split_secret_threshold = 2

[[spark.operators]]
address = "https://ops-0.ops:8535"
identifier = "0000000000000000000000000000000000000000000000000000000000000001"
identity_public_key = "{}"
ca_cert_path = "/spark-operators/ca.crt"

[[spark.operators]]
address = "https://ops-1.ops:8535"
identifier = "0000000000000000000000000000000000000000000000000000000000000002"
identity_public_key = "{}"
ca_cert_path = "/spark-operators/ca.crt"

[spark.ssp]
base_url = "http://ssp:5000"
identity_public_key = "{identity}"
schema_endpoint = "graphql/spark/rc"
"#,
            key(0),
            key(1),
        );
        assert_eq!(
            processor_config(50051, &federation, "http://ssp:5000", &identity),
            expected
        );
    }

    #[test]
    fn ssp_url_is_a_plain_service_address() {
        assert_eq!(ssp_url("http://ssp:5000").unwrap(), "http://ssp:5000");
        for value in [
            "https://ssp:5000",
            "http://ssp",
            "http://ssp:5000/",
            "http://ssp:5000/graphql",
            "http://user@ssp:5000",
            "http://ssp:5000?x=1",
            "http://SSP:5000",
            "ssp:5000",
            "",
        ] {
            assert!(ssp_url(value).is_err(), "{value}");
        }
    }

    #[test]
    fn ssp_is_live_only_with_live_lightning_matching_identities_and_no_error() {
        let identity = key(4);
        let live = json!({
            "ldk_mode": "live",
            "ssp_identity_pubkey": identity,
            "spark": {"identity_pubkey": identity, "available_sats": 1},
        });
        assert!(ssp_live(&identity, &live));
        for error in [Value::Null, json!("")] {
            let mut status = live.clone();
            status["spark_error"] = error;
            assert!(ssp_live(&identity, &status));
        }
        for (path, value) in [
            (&["ldk_mode"][..], Some(json!("mock"))),
            (&["ssp_identity_pubkey"], Some(json!(key(5)))),
            (&["spark", "identity_pubkey"], Some(json!(key(5)))),
            (&["spark_error"], Some(json!("operators unreachable"))),
            (&["spark_error"], Some(json!(false))),
            (&["spark"], None),
        ] {
            let status = changed(&live, path, value);
            assert!(!ssp_live(&identity, &status), "{status}");
        }
        assert!(!ssp_live(&identity, &json!([])));
        assert_eq!(
            ssp_identity(&json!({"identityPublicKey": identity})),
            Some(identity.clone())
        );
        for value in [
            json!({}),
            json!({"identityPublicKey": "02"}),
            json!(identity),
        ] {
            assert_eq!(ssp_identity(&value), None);
        }
    }

    /// A one-request-per-connection HTTP server answering from `reply(head, count)`;
    /// status 0 never answers.
    async fn stub(
        reply: impl Fn(&str, usize) -> (u16, String) + Send + 'static,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for count in 0..usize::MAX {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
                    head.push(byte[0]);
                }
                let (code, body) = reply(&String::from_utf8_lossy(&head), count);
                if code == 0 {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                let response = format!(
                    "HTTP/1.1 {code} Stub\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (base, task)
    }

    #[tokio::test]
    async fn ssp_readiness_authenticates_status_and_requires_the_served_identity() {
        let token = "ab".repeat(32);
        let identity = key(4);
        let status = json!({
            "ldk_mode": "live",
            "ssp_identity_pubkey": identity,
            "spark": {"identity_pubkey": identity},
            "spark_error": null,
        });
        let authorization = format!("authorization: bearer {token}");
        let (base, task) = stub(move |head, _| {
            if head.starts_with("GET /identity ") {
                (200, json!({"identityPublicKey": identity}).to_string())
            } else if head.starts_with("GET /status ")
                && head.to_ascii_lowercase().contains(&authorization)
            {
                (200, status.to_string())
            } else {
                (401, "{}".into())
            }
        })
        .await;
        open_ssp_ready_at(&base, &token).await.unwrap();
        assert!(open_ssp_ready_at(&base, &"cd".repeat(32)).await.is_err());
        task.abort();

        let (base, task) =
            stub(|_, _| (200, json!({"identityPublicKey": key(4)}).to_string())).await;
        assert!(open_ssp_ready_at(&base, &token).await.is_err());
        task.abort();
        let (base, task) = stub(|_, _| (0, String::new())).await;
        let start = Instant::now();
        assert!(open_ssp_ready_at(&base, &token).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
        task.abort();
    }

    #[tokio::test]
    async fn processor_waits_for_the_ssp_identity_within_its_limit() {
        let identity = key(6);
        let expected = identity.clone();
        let (base, task) = stub(move |_, count| {
            if count < 2 {
                (503, "{}".into())
            } else {
                (200, json!({"identityPublicKey": identity}).to_string())
            }
        })
        .await;
        assert_eq!(
            fetch_ssp_identity(&base, Duration::from_secs(10))
                .await
                .unwrap(),
            expected
        );
        task.abort();
        let (base, task) =
            stub(|_, _| (200, json!({"identityPublicKey": "bad"}).to_string())).await;
        let start = Instant::now();
        assert!(
            fetch_ssp_identity(&base, Duration::from_secs(2))
                .await
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        task.abort();
    }

    mod health {
        use super::super::{HealthCheckRequest, HealthCheckResponse};
        use std::{
            convert::Infallible,
            task::{Context, Poll},
            time::Duration,
        };
        use tonic::{
            Request, Response, Status,
            codegen::{Body, BoxFuture, Service, StdError, http},
        };

        /// Serves a fixed status for `spark.SparkService`; a negative status stalls.
        #[derive(Clone)]
        pub struct Health(pub i32);
        impl tonic::server::NamedService for Health {
            const NAME: &'static str = "grpc.health.v1.Health";
        }
        impl tonic::server::UnaryService<HealthCheckRequest> for Health {
            type Response = HealthCheckResponse;
            type Future = BoxFuture<Response<HealthCheckResponse>, Status>;
            fn call(&mut self, request: Request<HealthCheckRequest>) -> Self::Future {
                let status = self.0;
                let service = request.into_inner().service;
                Box::pin(async move {
                    if service != "spark.SparkService" {
                        return Err(Status::not_found("unknown service"));
                    }
                    if status < 0 {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    Ok(Response::new(HealthCheckResponse { status }))
                })
            }
        }
        impl<B> Service<http::Request<B>> for Health
        where
            B: Body + Send + 'static,
            B::Error: Into<StdError> + Send + 'static,
        {
            type Response = http::Response<tonic::body::Body>;
            type Error = Infallible;
            type Future = BoxFuture<Self::Response, Self::Error>;
            fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, request: http::Request<B>) -> Self::Future {
                assert_eq!(request.uri().path(), "/grpc.health.v1.Health/Check");
                let service = self.clone();
                Box::pin(async move {
                    let codec =
                        tonic_prost::ProstCodec::<HealthCheckResponse, HealthCheckRequest>::default(
                        );
                    Ok(tonic::server::Grpc::new(codec)
                        .unary(service, request)
                        .await)
                })
            }
        }
    }

    /// A CA and a server identity for `name`, as the controller generates them.
    fn tls(name: &str) -> (String, tonic::transport::Identity) {
        use rcgen::{
            BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
            KeyUsagePurpose,
        };
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca = params.self_signed(&ca_key).unwrap();
        let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &ca, &ca_key).unwrap();
        (
            ca.pem(),
            tonic::transport::Identity::from_pem(certificate.pem(), key.serialize_pem()),
        )
    }

    async fn health_server(
        identity: tonic::transport::Identity,
        status: i32,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("https://{}", listener.local_addr().unwrap());
        let mut server = tonic::transport::Server::builder()
            .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
            .unwrap();
        let router = server.add_service(health::Health(status));
        let task = tokio::spawn(async move {
            let _ = router
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await;
        });
        (address, task)
    }

    #[tokio::test]
    async fn operator_readiness_requires_a_trusted_serving_health_check() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.crt");
        let (pem, identity) = tls("localhost");
        fs::write(&ca, pem).unwrap();
        let (address, task) = health_server(identity.clone(), SERVING).await;
        operator_ready_at(&address, &ca).await.unwrap();
        let (other, _) = tls("localhost");
        let untrusted = dir.path().join("other.crt");
        fs::write(&untrusted, other).unwrap();
        assert!(operator_ready_at(&address, &untrusted).await.is_err());
        task.abort();
        for status in [0, 2, 3, -1] {
            let (address, task) = health_server(identity.clone(), status).await;
            let start = Instant::now();
            assert!(operator_ready_at(&address, &ca).await.is_err(), "{status}");
            assert!(start.elapsed() < Duration::from_secs(3));
            task.abort();
        }
        let (pem, identity) = tls("wrong.example");
        fs::write(&ca, pem).unwrap();
        let (address, task) = health_server(identity, SERVING).await;
        assert!(operator_ready_at(&address, &ca).await.is_err());
        task.abort();
    }
}
