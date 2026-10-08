//! Preserve the identity and complete native Bark wallet across process restarts.
use std::{
    fs,
    io::{Read as _, Write as _},
    os::unix::process::CommandExt,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};

const IDENTITY: &str = ".proofstorm-bark-identity";
const DATABASES: [&str; 2] = ["db.sqlite", "onchain_state.redb"];

pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::File::open(path).context("read required Bark identity file")?;
    ensure!(
        file.metadata()?.is_file(),
        "Bark identity must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Bark identity file exceeds its size limit"
    );
    Ok(bytes)
}

pub(crate) fn mnemonic(path: &Path) -> Result<String> {
    let bytes = read_bounded(path, 512)?;
    let value = std::str::from_utf8(&bytes).context("Bark identity is not UTF-8")?;
    // Do not attach the parser error: credential words must never reach diagnostics.
    value
        .trim()
        .parse::<bip39::Mnemonic>()
        .map(|seed| seed.to_string())
        .map_err(|_| anyhow::anyhow!("invalid Bark mnemonic"))
}

fn identity_record(mnemonic: &str) -> String {
    format!(
        "proofstorm-bark-wallet/v1\n{:x}\n",
        Sha256::digest(mnemonic.as_bytes())
    )
}

fn require_database(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .context("Bark wallet state is incomplete; refusing reinitialization")?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "Bark wallet state is incomplete; refusing reinitialization"
    );
    Ok(())
}

fn bind_identity(data: &Path, mnemonic: &str) -> Result<()> {
    ensure!(
        fs::symlink_metadata(data)?.is_dir(),
        "Bark data volume must be an existing directory"
    );
    let marker = data.join(IDENTITY);
    let expected = identity_record(mnemonic);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "Bark identity marker must be a regular file"
            );
            ensure!(
                read_bounded(&marker, 128)? == expected.as_bytes(),
                "Bark wallet identity mismatch; refusing to replace existing state"
            );
            for name in DATABASES {
                require_database(&data.join(name))?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for entry in fs::read_dir(data)? {
                let entry = entry?;
                // A freshly formatted PVC can contain an empty filesystem recovery directory.
                ensure!(
                    entry.file_name() == "lost+found"
                        && entry.file_type()?.is_dir()
                        && fs::read_dir(entry.path())?.next().is_none(),
                    "Bark state exists without its identity marker; refusing reinitialization"
                );
            }
            let mut temporary = tempfile::NamedTempFile::new_in(data)?;
            temporary.write_all(expected.as_bytes())?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&marker)
                .context("bind Bark wallet identity once")?;
            fs::File::open(data)?.sync_all()?;
        }
        Err(error) => return Err(error).context("inspect Bark identity marker"),
    }
    Ok(())
}

/// The rendered method selection. Upstream reads an absent list as every
/// method, so a missing or malformed value is refused rather than widened.
fn payment_methods(value: Option<&str>) -> Result<String> {
    value
        .and_then(|value| proofstorm_core::ProcessorProfile::Bark.parse_methods(value))
        .map(|methods| proofstorm_core::method_list(&methods))
        .context("BARK_PAYMENT_METHODS must list the rendered payment methods")
}

/// Load the private controller-generated seed and exec the native processor.
/// The native binary remains the only wallet initializer. Once startup has been
/// attempted, both databases are required; interrupted initialization fails closed.
/// # Errors
/// Refuses absent/invalid identity or methods, partial state, changed seed and failed exec.
pub fn exec_processor() -> Result<()> {
    let methods = payment_methods(std::env::var("BARK_PAYMENT_METHODS").ok().as_deref())?;
    let seed = mnemonic(Path::new("/processor-identity/mnemonic"))?;
    for path in [
        "/chain-rpc/rpc.cookie",
        "/processor-server/tls/ca.pem",
        "/processor-server/tls/server.pem",
        "/processor-server/tls/server.key",
    ] {
        ensure!(
            !read_bounded(Path::new(path), 65_536)?.is_empty(),
            "required Bark credential file is empty"
        );
    }
    bind_identity(Path::new("/data"), &seed)?;
    let error = std::process::Command::new("cdk-payment-processor-bark")
        .env("BARK_MNEMONIC", seed)
        .env("BARK_DATA_DIR", "/data")
        .env("BARK_NETWORK", "regtest")
        .env("BARK_PAYMENT_METHODS", methods)
        .env_remove("BARK_ESPLORA_ADDRESS")
        .exec();
    Err(error).context("execute native Bark payment processor")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(byte: u8) -> String {
        bip39::Mnemonic::from_entropy(&[byte; 16])
            .unwrap()
            .to_string()
    }

    fn populate(data: &Path) {
        for name in DATABASES {
            fs::write(data.join(name), format!("retained {name}")).unwrap();
        }
    }

    #[test]
    fn restart_retains_identity_and_both_databases_without_reinitialization() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("lost+found")).unwrap();
        let seed = seed(1);
        bind_identity(dir.path(), &seed).unwrap();
        let marker = fs::read(dir.path().join(IDENTITY)).unwrap();
        assert!(!String::from_utf8_lossy(&marker).contains(&seed));
        populate(dir.path());
        for _ in 0..3 {
            bind_identity(dir.path(), &seed).unwrap();
        }
        assert_eq!(fs::read(dir.path().join(IDENTITY)).unwrap(), marker);
        for name in DATABASES {
            assert_eq!(
                fs::read_to_string(dir.path().join(name)).unwrap(),
                format!("retained {name}")
            );
        }
        assert!(bind_identity(dir.path(), &self::seed(2)).is_err());
        assert_eq!(fs::read(dir.path().join(IDENTITY)).unwrap(), marker);
    }

    #[test]
    fn partial_or_unbound_state_never_gets_a_fresh_wallet() {
        for missing in [None, Some("db.sqlite"), Some("onchain_state.redb")] {
            let dir = tempfile::tempdir().unwrap();
            bind_identity(dir.path(), &seed(1)).unwrap();
            if let Some(missing) = missing {
                populate(dir.path());
                fs::remove_file(dir.path().join(missing)).unwrap();
            }
            assert!(bind_identity(dir.path(), &seed(1)).is_err());
        }
        for name in ["db.sqlite", "onchain_state.redb", "unexpected-data"] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(name), b"existing").unwrap();
            assert!(bind_identity(dir.path(), &seed(1)).is_err());
            assert!(!dir.path().join(IDENTITY).exists());
        }
    }

    #[test]
    fn empty_or_redirected_state_is_refused() {
        for name in [IDENTITY, "db.sqlite", "onchain_state.redb"] {
            for symlink in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                bind_identity(dir.path(), &seed(1)).unwrap();
                populate(dir.path());
                fs::remove_file(dir.path().join(name)).unwrap();
                if symlink {
                    let other = dir.path().join("other");
                    fs::write(&other, b"state").unwrap();
                    std::os::unix::fs::symlink(other, dir.path().join(name)).unwrap();
                } else {
                    fs::write(dir.path().join(name), b"").unwrap();
                }
                assert!(bind_identity(dir.path(), &seed(1)).is_err());
            }
        }
    }

    #[test]
    fn rendered_methods_pass_through_and_absent_methods_are_refused() {
        assert_eq!(
            payment_methods(Some("arkoor,bolt11")).unwrap(),
            "bolt11,arkoor"
        );
        assert_eq!(
            payment_methods(Some("bolt11,onchain,arkoor")).unwrap(),
            "bolt11,onchain,arkoor"
        );
        for value in [None, Some(""), Some("bolt12"), Some("bolt11,bolt11")] {
            assert!(payment_methods(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn projected_seed_is_bounded_validated_and_never_printed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mnemonic");
        assert!(mnemonic(&file).is_err());
        for value in [
            String::new(),
            "private invalid words".into(),
            "x".repeat(513),
        ] {
            fs::write(&file, &value).unwrap();
            let error = format!("{:#}", mnemonic(&file).unwrap_err());
            assert!(!error.contains("private invalid words"));
        }
        fs::write(&file, format!("{}\n", seed(1))).unwrap();
        let projected = dir.path().join("projected");
        std::os::unix::fs::symlink(&file, &projected).unwrap();
        assert_eq!(mnemonic(&projected).unwrap(), seed(1));
    }
}
