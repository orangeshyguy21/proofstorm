//! Native stack startup. A failed first initialization requires explicit recovery.
use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::Write as _,
    os::unix::{fs::DirBuilderExt as _, process::CommandExt as _},
    path::Path,
    process::Command,
};

const SERVER: &str = ".proofstorm-bark-server";
const SERVER_STARTED: &str = ".proofstorm-bark-server-started";
const CLN: &str = ".proofstorm-cln-hold";
const CLN_STARTED: &str = ".proofstorm-cln-hold-started";
const TEMPLATE: &str = "/usr/local/share/bark/captaind.default.toml";
const SERVER_DATA: &str = "native";
const TLS_FILES: [&str; 6] = [
    "ca.pem",
    "ca-key.pem",
    "server.pem",
    "server-key.pem",
    "client.pem",
    "client-key.pem",
];

/// Wait for the linked regtest node before binding fresh persistent state.
/// # Errors
/// Refuses absent credentials and a chain that never becomes ready.
pub async fn wait_chain(url: &str, cookie: &Path) -> Result<()> {
    let bytes = super::bark::read_bounded(cookie, 1024)?;
    let cookie = std::str::from_utf8(&bytes).context("invalid chain credential encoding")?;
    let (user, password) = cookie
        .trim()
        .split_once(':')
        .context("invalid chain credential format")?;
    let client = crate::http::client(std::time::Duration::from_secs(2))?;
    for _ in 0..120 {
        let request = client.post(url).basic_auth(user, Some(password))
            .json(&serde_json::json!({"jsonrpc":"2.0","id":"bark-start","method":"getblockchaininfo","params":[]}));
        if let Ok((status, value)) = crate::http::json(request).await {
            if status.is_success()
                && value["error"].is_null()
                && value["result"]["chain"] == "regtest"
            {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    anyhow::bail!("linked regtest Bitcoin node did not become ready")
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn regular(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .context("required native state is missing; restore owned state")?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "native state must be a nonempty regular file"
    );
    super::bark::read_bounded(path, 65_536)
}

fn write_once(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("state directory missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .context("preserve native state identity once")?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn fresh(data: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "native state volume must be a directory"
    );
    for entry in fs::read_dir(data)? {
        let entry = entry?;
        ensure!(
            entry.file_name() == "lost+found"
                && entry.file_type()?.is_dir()
                && fs::read_dir(entry.path())?.next().is_none(),
            "native state exists without a completed identity; refusing reinitialization"
        );
    }
    Ok(())
}

fn server_context() -> Result<String> {
    let fields = [
        "POSTGRES__HOST",
        "POSTGRES__PORT",
        "POSTGRES__NAME",
        "POSTGRES__USER",
        "BITCOIND__URL",
    ];
    let values = fields
        .into_iter()
        .map(|field| {
            std::env::var(format!("BARK_SERVER__{field}"))
                .with_context(|| format!("missing Bark {field}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(hash(serde_json::to_string(&values)?.as_bytes()))
}

fn server_identity(data: &Path, context: &str) -> Result<String> {
    let data = data.join(SERVER_DATA);
    ensure!(
        fs::symlink_metadata(&data)?.is_dir(),
        "Bark native directory was redirected"
    );
    regular(&data.join("mnemonic"))?;
    let seed = super::bark::mnemonic(&data.join("mnemonic"))?;
    Ok(hash(format!("{context}\n{seed}").as_bytes()))
}

fn verify_server(data: &Path, context: &str) -> Result<()> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "Bark native volume must be a directory"
    );
    ensure!(
        regular(&data.join(SERVER_STARTED))? == context.as_bytes(),
        "Bark database/chain binding changed; restore its original dependencies"
    );
    ensure!(
        regular(&data.join(SERVER))? == server_identity(data, context)?.as_bytes(),
        "Bark native identity changed or initialization is incomplete"
    );
    Ok(())
}

/// Initialize only a fresh owned Bark volume; subsequent invocations validate it.
/// # Errors
/// Refuses missing identity, changed dependencies, partial initialization and native failure.
pub fn prepare_server() -> Result<()> {
    let context = server_context()?;
    prepare_server_with(Path::new("/data"), Path::new("/runtime"), &context, || {
        let status = Command::new("captaind")
            .args(["--config", TEMPLATE, "create"])
            .status()
            .context("initialize native Bark server")?;
        ensure!(
            status.success(),
            "native Bark initialization failed; explicit recovery required"
        );
        Ok(())
    })
}

fn prepare_server_with(
    data: &Path,
    runtime: &Path,
    context: &str,
    initialize: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if data.join(SERVER_STARTED).try_exists()? {
        return verify_server(data, context);
    }
    fresh(data)?;
    write_once(&data.join(SERVER_STARTED), context.as_bytes())?;
    // Kubernetes owns the volume root. The native server chmods its data
    // directory, so create a private child owned by our unprivileged UID.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(data.join(SERVER_DATA))?;
    initialize()?;
    let identity = server_identity(data, context)?;
    write_once(&data.join(SERVER), identity.as_bytes())?;
    write_once(&runtime.join("bark-created"), b"created")?;
    Ok(())
}

/// Start a previously initialized native server, without any create fallback.
/// # Errors
/// Refuses identity/dependency changes or failed native exec.
pub fn exec_server() -> Result<()> {
    verify_server(Path::new("/data"), &server_context()?)?;
    Err(Command::new("captaind")
        .args(["--config", TEMPLATE, "start"])
        .exec())
    .context("execute native Bark server")
}

fn tls_material(cln: &Path, hold: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut material = Vec::new();
    for (role, dir) in [("cln", cln), ("hold", hold)] {
        for name in TLS_FILES {
            // Kubernetes Secret projections are symlinks, unlike native state files.
            let bytes = super::bark::read_bounded(&dir.join(name), 65_536)?;
            ensure!(
                !bytes.is_empty(),
                "CLN/hold projected TLS identity is incomplete"
            );
            material.push((format!("{role}/{name}"), bytes));
        }
    }
    Ok(material)
}

fn tls_fingerprint(material: &[(String, Vec<u8>)]) -> Result<String> {
    Ok(hash(&serde_json::to_vec(material)?))
}

fn native_tls_path(data: &Path, name: &str) -> std::path::PathBuf {
    let (role, file) = name.split_once('/').expect("internal TLS name");
    if role == "cln" {
        data.join("regtest").join(file)
    } else {
        data.join("regtest/hold").join(file)
    }
}

fn cln_identity(data: &Path) -> Result<String> {
    let hsm = regular(&data.join("regtest/hsm_secret"))?;
    validate_cln_hsm(&hsm)?;
    for file in ["regtest/lightningd.sqlite3", "regtest/hold/hold.sqlite3"] {
        let metadata = fs::symlink_metadata(data.join(file))
            .context("CLN/hold database is missing; refusing reinitialization")?;
        ensure!(
            metadata.is_file() && metadata.len() > 0,
            "CLN/hold database must be nonempty native state"
        );
    }
    Ok(hash(&hsm))
}

fn validate_cln_hsm(hsm: &[u8]) -> Result<()> {
    // CLN 26.06.7 common/hsm_secret.c supports the legacy binary secret and
    // BIP39 prefixed by a 32-byte passphrase hash. Our noninteractive profile
    // creates the latter with no passphrase (an all-zero prefix).
    if hsm.len() == 32 {
        return Ok(());
    }
    let mnemonic = hsm
        .strip_prefix(&[0; 32])
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .context("unsupported CLN native HSM identity format")?;
    bip39::Mnemonic::parse_in_normalized(bip39::Language::English, mnemonic)
        .map_err(|_| anyhow::anyhow!("CLN native HSM mnemonic is invalid"))?;
    Ok(())
}

fn prepare_cln(data: &Path, material: &[(String, Vec<u8>)]) -> Result<()> {
    let fingerprint = tls_fingerprint(material)?;
    if data.join(CLN_STARTED).try_exists()? {
        for dir in [
            data.to_path_buf(),
            data.join("regtest"),
            data.join("regtest/hold"),
        ] {
            ensure!(
                fs::symlink_metadata(dir)?.is_dir(),
                "CLN/hold state directory was redirected"
            );
        }
        ensure!(
            regular(&data.join(CLN_STARTED))? == fingerprint.as_bytes(),
            "CLN/hold TLS identity changed; refusing replacement"
        );
        ensure!(
            regular(&data.join(CLN))? == cln_identity(data)?.as_bytes(),
            "CLN/hold initialization incomplete or HSM identity changed"
        );
        for (name, bytes) in material {
            ensure!(
                regular(&native_tls_path(data, name))? == *bytes,
                "CLN/hold native TLS identity missing or changed"
            );
        }
    } else {
        fresh(data)?;
        write_once(&data.join(CLN_STARTED), fingerprint.as_bytes())?;
        fs::create_dir_all(data.join("regtest/hold"))?;
        for (name, bytes) in material {
            write_once(&native_tls_path(data, name), bytes)?;
        }
    }
    Ok(())
}

/// Preserve the paired CLN and hold stores and privately seed their native TLS paths.
/// # Errors
/// Refuses identity replacement, partial stores and failed native exec.
pub fn exec_cln_hold() -> Result<()> {
    let material = tls_material(Path::new("/cln-tls"), Path::new("/hold-tls"))?;
    prepare_cln(Path::new("/data"), &material)?;
    Err(Command::new("lightningd")
        .arg("--conf=/config/lightning.conf")
        .exec())
    .context("execute native CLN/hold")
}

/// Check both native APIs before sealing the first successful initialization.
/// # Errors
/// Refuses failed APIs, incomplete state or changed identities.
pub async fn cln_hold_ready() -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        for command in ["getinfo", "listholdinvoices"] {
            crate::cln::rpc(
                Path::new("/data/regtest/lightning-rpc"),
                command,
                serde_json::json!({}),
            )
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("CLN/hold readiness deadline")??;
    for port in [9988, 9292] {
        std::net::TcpStream::connect_timeout(
            &([127, 0, 0, 1], port).into(),
            std::time::Duration::from_secs(1),
        )?;
    }
    seal_cln(Path::new("/data"))
}

fn seal_cln(data: &Path) -> Result<()> {
    let identity = cln_identity(data)?;
    if data.join(CLN).try_exists()? {
        ensure!(
            regular(&data.join(CLN))? == identity.as_bytes(),
            "CLN HSM identity changed"
        );
    } else {
        write_once(&data.join(CLN), identity.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    #[test]
    fn server_initializes_once_and_never_retries_incomplete_initialization() {
        let data = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let seed = bip39::Mnemonic::from_entropy(&[42; 16])
            .unwrap()
            .to_string();
        let initialize = || {
            let native = data.path().join(SERVER_DATA);
            assert_eq!(fs::metadata(&native)?.permissions().mode() & 0o777, 0o700);
            fs::write(native.join("mnemonic"), &seed)?;
            Ok(())
        };
        prepare_server_with(data.path(), runtime.path(), "context", initialize).unwrap();
        prepare_server_with(data.path(), runtime.path(), "context", || {
            panic!("native create ran twice")
        })
        .unwrap();
        assert!(runtime.path().join("bark-created").is_file());
        assert_eq!(
            fs::read_to_string(data.path().join(SERVER_DATA).join("mnemonic")).unwrap(),
            seed
        );
        fs::remove_file(data.path().join(SERVER)).unwrap();
        assert!(
            prepare_server_with(data.path(), runtime.path(), "context", || panic!(
                "partial state was reinitialized"
            ))
            .is_err()
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(
            prepare_server_with(empty.path(), runtime.path(), "context", || anyhow::bail!(
                "native failure"
            ))
            .is_err()
        );
        assert!(
            prepare_server_with(empty.path(), runtime.path(), "context", || panic!(
                "failed initialization was retried"
            ))
            .is_err()
        );
    }
    fn material() -> Vec<(String, Vec<u8>)> {
        ["cln", "hold"]
            .into_iter()
            .flat_map(|role| {
                TLS_FILES.map(|file| {
                    (
                        format!("{role}/{file}"),
                        format!("{role}-{file}").into_bytes(),
                    )
                })
            })
            .collect()
    }
    fn mnemonic_hsm() -> Vec<u8> {
        let mut hsm = vec![0; 32];
        hsm.extend_from_slice(
            bip39::Mnemonic::from_entropy(&[42; 16])
                .unwrap()
                .to_string()
                .as_bytes(),
        );
        hsm
    }
    #[test]
    fn cln_accepts_native_mnemonic_and_legacy_hsm_but_rejects_invalid_state() {
        validate_cln_hsm(&[42; 32]).unwrap();
        validate_cln_hsm(&mnemonic_hsm()).unwrap();
        assert!(validate_cln_hsm(&[42; 31]).is_err());
        let mut protected = mnemonic_hsm();
        protected[0] = 1;
        assert!(validate_cln_hsm(&protected).is_err());
        let mut invalid = vec![0; 32];
        invalid.extend_from_slice(b"not a valid native mnemonic");
        assert!(validate_cln_hsm(&invalid).is_err());
    }
    fn initialized_cln(data: &Path) {
        prepare_cln(data, &material()).unwrap();
        fs::write(data.join("regtest/hsm_secret"), mnemonic_hsm()).unwrap();
        fs::write(data.join("regtest/lightningd.sqlite3"), b"cln-state").unwrap();
        fs::write(data.join("regtest/hold/hold.sqlite3"), b"hold-state").unwrap();
        seal_cln(data).unwrap();
    }
    #[test]
    fn cln_restart_preserves_identity_tls_and_payment_databases() {
        let dir = tempfile::tempdir().unwrap();
        initialized_cln(dir.path());
        for _ in 0..3 {
            prepare_cln(dir.path(), &material()).unwrap();
        }
        assert_eq!(
            fs::read(dir.path().join("regtest/hold/hold.sqlite3")).unwrap(),
            b"hold-state"
        );
        let mut changed = material();
        changed[0].1 = b"new-ca".to_vec();
        assert!(prepare_cln(dir.path(), &changed).is_err());
    }
    #[test]
    fn cln_refuses_partial_initialization_and_erased_or_changed_state() {
        let dir = tempfile::tempdir().unwrap();
        prepare_cln(dir.path(), &material()).unwrap();
        assert!(prepare_cln(dir.path(), &material()).is_err());
        for file in [
            CLN,
            CLN_STARTED,
            "regtest/hsm_secret",
            "regtest/lightningd.sqlite3",
            "regtest/hold/hold.sqlite3",
            "regtest/hold/ca-key.pem",
        ] {
            let dir = tempfile::tempdir().unwrap();
            initialized_cln(dir.path());
            fs::remove_file(dir.path().join(file)).unwrap();
            assert!(prepare_cln(dir.path(), &material()).is_err(), "{file}");
        }
        let dir = tempfile::tempdir().unwrap();
        initialized_cln(dir.path());
        fs::write(dir.path().join("regtest/hsm_secret"), [43; 32]).unwrap();
        assert!(prepare_cln(dir.path(), &material()).is_err());
    }
    #[test]
    fn server_requires_retained_seed_and_original_database_binding() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join(SERVER_DATA);
        fs::create_dir(&native).unwrap();
        let seed = bip39::Mnemonic::from_entropy(&[42; 16])
            .unwrap()
            .to_string();
        fs::write(native.join("mnemonic"), &seed).unwrap();
        write_once(&dir.path().join(SERVER_STARTED), b"context").unwrap();
        let identity = server_identity(dir.path(), "context").unwrap();
        write_once(&dir.path().join(SERVER), identity.as_bytes()).unwrap();
        verify_server(dir.path(), "context").unwrap();
        assert!(verify_server(dir.path(), "new-database").is_err());
        fs::write(
            native.join("mnemonic"),
            bip39::Mnemonic::from_entropy(&[43; 16])
                .unwrap()
                .to_string(),
        )
        .unwrap();
        assert!(verify_server(dir.path(), "context").is_err());
        fs::remove_file(native.join("mnemonic")).unwrap();
        assert!(verify_server(dir.path(), "context").is_err());
        fs::remove_dir(&native).unwrap();
        let redirected = tempfile::tempdir().unwrap();
        fs::write(redirected.path().join("mnemonic"), &seed).unwrap();
        std::os::unix::fs::symlink(redirected.path(), &native).unwrap();
        assert!(verify_server(dir.path(), "context").is_err());
    }
}
