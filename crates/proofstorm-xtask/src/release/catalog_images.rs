//! Catalog image maintenance, separate from controller/release promotion and runtime setup.
mod audit;
mod bark;
use super::{archive::output_path, build, bundle, registry, sha256, text};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const NAMESPACE: &str = "ghcr.io/orangeshyguy21/proofstorm";
const RECIPES: &[&str] = &[
    "bitcoin-core",
    "cdk-mint",
    "nutshell-mint",
    "cdk-cli-wallet",
    "cocod-wallet",
    "ldk-server",
    "cdk-ldk-server-processor",
    "cdk-bark-processor",
    "bark-server",
    "cln-hold",
    "spark-operator",
    "open-ssp",
    "cdk-spark-processor",
];

fn recipe(name: &str) -> Result<&'static str> {
    Ok(match name {
        "bitcoin-core" => "docker/bitcoin/Dockerfile",
        "cdk-mint" | "cdk-mint-management" | "cdk-ldk-mint-management" => {
            "docker/mint/Dockerfile.kube-cdk"
        }
        "nutshell-mint" | "nutshell-mint-management" => "docker/mint/Dockerfile.kube-nutshell",
        "cdk-cli-wallet" => "docker/wallet/Dockerfile.kube-cdk",
        "cocod-wallet" => "docker/wallet/Dockerfile.kube-cocod",
        "ldk-server" => "docker/payment/Dockerfile.ldk-server",
        "cdk-ldk-server-processor" => "docker/payment/Dockerfile.cdk-ldk-server",
        "cdk-bark-processor" => "docker/payment/Dockerfile.cdk-bark",
        "bark-server" => "docker/payment/Dockerfile.bark-server",
        "cln-hold" => "docker/payment/Dockerfile.cln-hold",
        "spark-operator" => "docker/payment/Dockerfile.spark-operator",
        "open-ssp" => "docker/payment/Dockerfile.open-ssp",
        "cdk-spark-processor" => "docker/payment/Dockerfile.cdk-spark",
        _ => bail!("unknown catalog image; controller builds use release-controller-build"),
    })
}

// These defaults also interpret receipts written before explicit version selection.
// Do not change them when adding releases: old receipts retain their build identity.
fn legacy_version(name: &str) -> Result<&'static str> {
    Ok(match name {
        "bitcoin-core" => "31.1",
        "cdk-mint" | "cdk-mint-management" | "cdk-ldk-mint-management" | "cdk-cli-wallet" => {
            "0.18.0"
        }
        "nutshell-mint" | "nutshell-mint-management" => "0.20.3",
        "cocod-wallet" => "0.0.17-dev.44e5101c",
        "ldk-server" => "0.1.0-50fe752",
        "cdk-ldk-server-processor" | "cdk-bark-processor" => "0.1.0-fe468ca",
        "bark-server" => "0.7.0-6188e2d",
        "cln-hold" => "26.06.7-hold.0.3.3",
        "spark-operator" => "0.0.0-5e0c879",
        "open-ssp" => "0.1.0-9c25f69",
        "cdk-spark-processor" => "0.1.0-d7244e2",
        _ => bail!("unknown catalog image version"),
    })
}

// Reviewed recipes for releases after each image's legacy version.
const VERSIONED_RECIPES: &[(&str, &str, &str)] = &[
    ("bitcoin-core", "29.4", "docker/bitcoin/Dockerfile.29.4"),
    ("bitcoin-core", "30.3", "docker/bitcoin/Dockerfile.30.3"),
    (
        "nutshell-mint",
        "0.21.0",
        "docker/mint/Dockerfile.nutshell-0.21.0",
    ),
    ("cdk-mint", "0.18.1", "docker/mint/Dockerfile.cdk-0.18.1"),
    (
        "cdk-cli-wallet",
        "0.18.1",
        "docker/wallet/Dockerfile.cdk-0.18.1",
    ),
    ("cdk-mint", "0.18.2", "docker/mint/Dockerfile.cdk-0.18.2"),
    (
        "cdk-cli-wallet",
        "0.18.2",
        "docker/wallet/Dockerfile.cdk-0.18.2",
    ),
    // Macaroon authentication for open-ssp; the LDK Server processor keeps 50fe752.
    (
        "ldk-server",
        "0.1.0-762bc47",
        "docker/payment/Dockerfile.ldk-server-762bc47",
    ),
];

fn versioned_recipe(name: &str, version: &str) -> Result<&'static str> {
    if version == legacy_version(name)? {
        return recipe(name);
    }
    VERSIONED_RECIPES
        .iter()
        .find(|(recipe, reviewed, _)| *recipe == name && *reviewed == version)
        .map(|(_, _, path)| *path)
        .with_context(|| {
            format!("unreviewed catalog image version {name}@{version}; use catalog-image list")
        })
}

fn recipe_list() -> String {
    let versions = RECIPES
        .iter()
        .map(|name| (*name, legacy_version(name).expect("reviewed recipe")))
        .chain(
            VERSIONED_RECIPES
                .iter()
                .map(|(name, version, _)| (*name, *version)),
        )
        .map(|(name, version)| format!("{name}@{version}"))
        .collect::<Vec<_>>();
    format!(
        "Catalog recipes (linux/amd64 or linux/arm64; select RECIPE@VERSION):\n{}",
        versions.join("\n")
    )
}

fn selector(value: &str) -> Result<(&str, &str)> {
    let (name, version) = if let Some(pair) = value.split_once('@') {
        pair
    } else {
        (value, legacy_version(value)?)
    };
    ensure!(RECIPES.contains(&name), "select a current catalog recipe");
    versioned_recipe(name, version)?;
    Ok((name, version))
}

/// Provenance record and source patches that a payment-stack recipe must match exactly.
fn bound_inputs(name: &str, version: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match (name, version) {
        ("cdk-bark-processor", _) => (
            "docker/payment/cdk-bark-provenance.json",
            &["docker/payment/patches/bark-regtest-rpc.patch"],
        ),
        ("bark-server", _) => ("docker/payment/bark-server-provenance.json", &[]),
        ("cln-hold", _) => ("docker/payment/cln-hold-provenance.json", &[]),
        ("spark-operator", _) => ("docker/payment/spark-operator-provenance.json", &[]),
        ("open-ssp", _) => ("docker/payment/open-ssp-provenance.json", &[]),
        ("cdk-spark-processor", _) => (
            "docker/payment/cdk-spark-provenance.json",
            &[
                "docker/payment/patches/cdk-spark-no-auto-optimize.patch",
                "docker/payment/patches/cdk-spark-wait-for-send.patch",
            ],
        ),
        ("ldk-server", "0.1.0-762bc47") => {
            ("docker/payment/ldk-server-762bc47-provenance.json", &[])
        }
        _ => return None,
    })
}

fn verify_build_inputs(
    source: &Path,
    name: &str,
    version: &str,
    recipe_sha256: &str,
) -> Result<()> {
    let Some((provenance, patches)) = bound_inputs(name, version) else {
        return Ok(());
    };
    let record: proofstorm_core::BuildProvenance =
        serde_json::from_slice(&fs::read(source.join(provenance))?)?;
    let recipe = fs::read_to_string(source.join(versioned_recipe(name, version)?))?;
    ensure!(
        record.recipe_digest == format!("sha256:{recipe_sha256}")
            && record.platform == "linux/amd64,linux/arm64"
            && sha256(&record.artifact_sha256)
            && recipe.contains(&record.artifact_url)
            && recipe.contains(&record.artifact_sha256)
            && recipe.contains(&record.commit_sha)
            && record
                .dependency_lock_digest
                .as_deref()
                .is_some_and(|digest| {
                    digest
                        .strip_prefix("sha256:")
                        .is_some_and(|hash| sha256(hash) && recipe.contains(hash))
                })
            && record.build_image.as_deref().is_some_and(|image| {
                image
                    .rsplit_once('@')
                    .is_some_and(|(_, digest)| super::pinned(image) && recipe.contains(digest))
            })
            && record
                .runtime_image
                .rsplit_once('@')
                .is_some_and(
                    |(_, digest)| super::pinned(&record.runtime_image) && recipe.contains(digest)
                ),
        "{name} provenance does not match the frozen recipe"
    );
    // Secondary inputs (vendor archives, extra lockfiles, base images) are named
    // by digest in the transformations; each one must be checked by the recipe.
    ensure!(
        record
            .transformations
            .iter()
            .flat_map(|entry| entry
                .match_indices("sha256:")
                .map(|(at, _)| &entry[at + 7..]))
            .all(|rest| rest
                .get(..64)
                .is_some_and(|hash| sha256(hash) && recipe.contains(hash))),
        "{name} provenance names an input its recipe does not check"
    );
    for patch in patches {
        let digest = format!("{:x}", Sha256::digest(fs::read(source.join(patch))?));
        ensure!(
            recipe.contains(&digest)
                && record
                    .transformations
                    .iter()
                    .any(|entry| entry.contains(&format!("sha256:{digest}"))),
            "{name} patch identity differs from its provenance: {patch}"
        );
    }
    Ok(())
}

fn probe(name: &str) -> Result<&'static str> {
    Ok(match name {
        // Version-only probes run with a read-only root filesystem. Recent
        // bitcoind releases otherwise initialize settings before printing it.
        "bitcoin-core" => "bitcoind -nosettings --version",
        "cdk-mint" | "cdk-mint-management" | "cdk-ldk-mint-management" => {
            "cdk-mint-cli --version && cdk-mintd --version"
        }
        "nutshell-mint" | "nutshell-mint-management" => {
            "mint --version && cashu --help && mint-cli --help"
        }
        "cdk-cli-wallet" => "cdk-cli --version",
        "cocod-wallet" => "cocod --version",
        // Only images built from 762bc47 onwards record their revision.
        "ldk-server" => {
            "ldk-server --version && ldk-server-cli --version && if test -f /usr/local/share/ldk-server-revision; then cat /usr/local/share/ldk-server-revision; fi"
        }
        // Upstream has no version-only command. Verify the executable and pinned
        // build identity offline; the live gate separately exercises GetSettings.
        "cdk-ldk-server-processor" => {
            "test -x /usr/local/bin/cdk-payment-processor-ldk-server && cat /usr/local/share/processor-revision"
        }
        "cdk-bark-processor" => {
            "test -x /usr/local/bin/cdk-payment-processor-bark && cat /usr/local/share/processor-revision /usr/local/share/processor-patch-sha256"
        }
        "bark-server" => {
            "captaind --version && test -s /usr/local/share/bark/captaind.default.toml && cat /usr/local/share/bark/server-revision"
        }
        "cln-hold" => {
            "lightningd --version && test -x /usr/local/bin/hold && cat /usr/local/share/hold-revision"
        }
        // The operator has no version command and atlas may reach the network.
        "spark-operator" => {
            "spark-frost-signer --version && test -x /usr/local/bin/spark-operator && test -x /usr/local/bin/atlas && test -s /opt/spark/migrations/atlas.sum && test -s /opt/spark/ephemeral_migrations/atlas.sum && cat /usr/local/share/spark-revision"
        }
        // open-ssp has no version command; any invocation starts the service.
        "open-ssp" => "test -x /usr/local/bin/open-ssp && cat /usr/local/share/open-ssp-revision",
        "cdk-spark-processor" => {
            "test -x /usr/local/bin/cdk-payment-processor-spark && cat /usr/local/share/processor-revision /usr/local/share/processor-patch-sha256"
        }
        _ => bail!("unknown catalog probe"),
    })
}

/// New copies use the current names; existing receipts retain their destinations.
fn current_repository(name: &str) -> &str {
    match name {
        "cdk-ldk-mint-management" => "cdk-mint",
        "nutshell-mint-management" => "nutshell-mint",
        _ => name,
    }
}

fn copy_repository_matches(source: &str, destination: &str) -> bool {
    source == destination || current_repository(source) == destination
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Input {
    Build {
        source: Value,
        recipe_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Copy {
        image: String,
    },
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Publication {
    Prepared,
    UploadAttempted,
    Uploaded,
    Verified,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    format_version: u32,
    repository: String,
    platform: String,
    publication_id: String,
    tag: String,
    input: Input,
    local_image_id: Option<String>,
    local_verified: bool,
    publication: Publication,
    image: Option<String>,
    release_ready: bool,
}

impl Receipt {
    fn version(&self) -> Result<&str> {
        match &self.input {
            Input::Build {
                version: Some(version),
                ..
            } => Ok(version),
            _ => legacy_version(&self.repository),
        }
    }

    fn recipe(&self) -> Result<&'static str> {
        versioned_recipe(&self.repository, self.version()?)
    }

    fn validate(&self) -> Result<()> {
        self.recipe()?;
        registry::architecture(&self.platform)?;
        ensure!(
            self.format_version == 1
                && !self.release_ready
                && self.publication_id.len() == 32
                && self
                    .publication_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && self.tag
                    == format!(
                        "{NAMESPACE}/{}:upload-{}",
                        self.repository, self.publication_id
                    ),
            "invalid catalog image receipt or publication destination"
        );
        match &self.input {
            Input::Build {
                source,
                recipe_sha256,
                ..
            } => ensure!(
                source["dirty"] == false
                    && sha256(text(source, "sha256")?)
                    && sha256(recipe_sha256)
                    && text(source, "revision")?.len() == 40
                    && text(source, "revision")?
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit())
                    && (!self.local_verified || self.local_image_id.is_some())
                    && (self.publication == Publication::Prepared || self.local_verified),
                "unverified source snapshot"
            ),
            Input::Copy { image } => {
                let reference = registry::Reference::parse(image)?;
                ensure!(
                    reference
                        .repository
                        .rsplit('/')
                        .next()
                        .is_some_and(|source| {
                            copy_repository_matches(source, &self.repository)
                        })
                        && self.local_image_id.is_none()
                        && !self.local_verified,
                    "copy source repository changed"
                );
            }
        }
        if let Some(id) = &self.local_image_id {
            ensure!(
                id.strip_prefix("sha256:").is_some_and(sha256),
                "invalid built image identity"
            );
        }
        if let Some(image) = &self.image {
            let reference = registry::Reference::parse(image)?;
            ensure!(
                reference.host == "ghcr.io"
                    && image.starts_with(&format!("{NAMESPACE}/{}@", self.repository)),
                "foreign published image"
            );
        }
        ensure!(
            self.publication != Publication::Verified || self.image.is_some(),
            "invalid verification state"
        );
        Ok(())
    }
}

fn save(work: &Path, receipt: &Receipt) -> Result<()> {
    receipt.validate()?;
    let path = work.join("image.json");
    if fs::symlink_metadata(&path).is_ok() {
        crate::development::regular(&path)?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(work)?;
    temporary.write_all(&serde_json::to_vec_pretty(receipt)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

fn load(work: &Path) -> Result<Receipt> {
    crate::development::regular(&work.join("image.json"))?;
    let receipt: Receipt = serde_json::from_value(bundle::read_json(&work.join("image.json"))?)?;
    receipt.validate()?;
    if let Input::Build {
        source,
        recipe_sha256,
        ..
    } = &receipt.input
    {
        build::verify_snapshot(&work.join("source"), source)?;
        ensure!(
            format!(
                "{:x}",
                Sha256::digest(fs::read(work.join("source").join(receipt.recipe()?))?)
            ) == *recipe_sha256,
            "recipe changed after preparation"
        );
        if receipt.repository == "cocod-wallet" {
            verify_cocod_archive(work)?;
        }
    }
    Ok(receipt)
}

fn prepare_cocod(staging: &Path, recipe_sha256: &str) -> Result<()> {
    let record =
        bundle::read_json(&staging.join("source/docker/wallet/cocod-44e5101c-provenance.json"))?;
    let commit = text(&record, "commit_sha")?;
    ensure!(
        commit.len() == 40
            && commit.bytes().all(|b| b.is_ascii_hexdigit())
            && record["artifact_url"]
                == format!("https://codeload.github.com/cashubtc/coco/tar.gz/{commit}")
            && record["recipe_digest"] == format!("sha256:{recipe_sha256}"),
        "Cocod provenance/recipe mismatch"
    );
    let context = staging.join("context");
    fs::create_dir(&context)?;
    let archive = context.join("source.tar.gz");
    let status = Command::new("curl")
        .args([
            "-q",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "180",
            "--max-filesize",
            "268435456",
            "--silent",
            "--show-error",
            text(&record, "artifact_url")?,
            "--output",
        ])
        .arg(&archive)
        .status()?;
    ensure!(
        status.success() && fs::metadata(&archive)?.len() <= 256 * 1024 * 1024,
        "source archive download failed"
    );
    verify_cocod_archive(staging)?;
    Ok(())
}

fn verify_cocod_archive(work: &Path) -> Result<()> {
    let record =
        bundle::read_json(&work.join("source/docker/wallet/cocod-44e5101c-provenance.json"))?;
    let path = work.join("context/source.tar.gz");
    crate::development::regular(&path)?;
    ensure!(
        fs::metadata(&path)?.len() <= 256 * 1024 * 1024,
        "source archive too large"
    );
    ensure!(
        format!("{:x}", Sha256::digest(fs::read(path)?)) == text(&record, "artifact_sha256")?,
        "source archive checksum mismatch"
    );
    Ok(())
}

fn prepare(root: &Path, work: &Path, name: &str, platform: &str, copy: Option<&str>) -> Result<()> {
    let (name, version) = selector(name)?;
    ensure!(
        RECIPES.contains(&name),
        "select a current catalog recipe; mint images use cdk-mint and nutshell-mint"
    );
    let recipe = versioned_recipe(name, version)?;
    registry::architecture(platform)?;
    let publication = bundle::read_json(&root.join("release/ghcr.json"))?;
    ensure!(
        publication["namespace"] == NAMESPACE && publication["visibility"] == "public",
        "unapproved publisher configuration"
    );
    let work = output_path(work)?;
    ensure!(
        !work.starts_with(root) && fs::symlink_metadata(&work).is_err(),
        "image work directory must be new and outside the checkout"
    );
    let staging = tempfile::Builder::new()
        .prefix("storm-image-")
        .tempdir_in(work.parent().context("missing work parent")?)?;
    let input = if let Some(image) = copy {
        let reference = registry::Reference::parse(image)?;
        ensure!(
            reference
                .repository
                .rsplit('/')
                .next()
                .is_some_and(|source| copy_repository_matches(source, name)),
            "copy source repository mismatch"
        );
        registry::verify(image, None, platform, false)?;
        Input::Copy {
            image: image.into(),
        }
    } else {
        let source = build::snapshot(root, &staging.path().join("source"), false, None)?;
        let recipe_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(staging.path().join("source").join(recipe))?)
        );
        verify_build_inputs(
            &staging.path().join("source"),
            name,
            version,
            &recipe_sha256,
        )?;
        if name == "cocod-wallet" {
            prepare_cocod(staging.path(), &recipe_sha256)?;
        }
        Input::Build {
            source,
            recipe_sha256,
            version: Some(version.into()),
        }
    };
    let id = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}:{}",
                work.display(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            )
            .as_bytes()
        )
    )[..32]
        .to_owned();
    let receipt = Receipt {
        format_version: 1,
        repository: name.into(),
        platform: platform.into(),
        tag: format!("{NAMESPACE}/{name}:upload-{id}"),
        publication_id: id,
        input,
        local_image_id: None,
        local_verified: false,
        publication: Publication::Prepared,
        image: None,
        release_ready: false,
    };
    save(staging.path(), &receipt)?;
    fs::rename(staging.keep(), &work)?;
    Ok(())
}

fn inspect(work: &Path) -> Result<String> {
    let receipt = load(work)?;
    ensure!(
        matches!(receipt.input, Input::Build { .. }),
        "only builds have local image IDs"
    );
    let inspect = bundle::read_json(&work.join("inspect.json"))?;
    let images = inspect.as_array().context("invalid image inspection")?;
    ensure!(images.len() == 1, "expected one local image");
    let image = &images[0];
    let id = text(image, "Id")?;
    if let Input::Build { source, .. } = &receipt.input {
        ensure!(
            image["Config"]["Labels"]["dev.proofstorm.source-sha256"] == source["sha256"],
            "image/source label mismatch"
        );
    }
    ensure!(
        id.strip_prefix("sha256:").is_some_and(sha256)
            && image["Os"] == "linux"
            && image["Architecture"] == registry::architecture(&receipt.platform)?,
        "image identity/architecture mismatch"
    );
    if matches!(
        receipt.repository.as_str(),
        "bitcoin-core"
            | "cdk-cli-wallet"
            | "cocod-wallet"
            | "ldk-server"
            | "cdk-ldk-server-processor"
            | "cdk-bark-processor"
            | "bark-server"
            | "cln-hold"
    ) {
        ensure!(
            image["Config"]["User"] == "1000:1000",
            "wallet/Bitcoin image must run as non-root"
        );
    }
    if let Some(previous) = &receipt.local_image_id {
        ensure!(id == previous, "image changed after verification");
    }
    Ok(id.into())
}

fn valid_probe_version(repository: &str, version: &str, output: &str) -> bool {
    if versioned_recipe(repository, version).is_err() {
        return false;
    }
    match repository {
        "bitcoin-core" => output.lines().next().is_some_and(|line| {
            let suffix = if version == "29.4" { "" } else { " bitcoind" };
            line == format!("Bitcoin Core daemon version v{version}.0{suffix}")
        }),
        "cdk-cli-wallet" => output.trim() == format!("cdk-cli {version}"),
        "cocod-wallet" => output.trim() == "0.0.17",
        "ldk-server" if version == "0.1.0-762bc47" => {
            output.trim()
                == "ldk-server 0.1.0 (unknown)\nldk-server-cli 0.1.0 (unknown)\n762bc473342c8f945475785ec6743a9b26f56aff"
        }
        "ldk-server" => output.trim() == "ldk-server 0.1.0\nldk-server-cli 0.1.0",
        "cdk-ldk-server-processor" => output.trim() == "fe468cad486157683eddbc0df4ff87ba71b6c0a3",
        "cdk-bark-processor" => {
            output.trim()
                == "fe468cad486157683eddbc0df4ff87ba71b6c0a3\nabc3f967d754cdf5e484bf434ef52fa216dfb37cdcbb8fd896477e5c7b40321c"
        }
        "bark-server" => {
            output.trim()
                == "captaind 0.7.0-dev+6188e2d809f193716b2e571274179f069d9c19ca\n6188e2d809f193716b2e571274179f069d9c19ca"
        }
        "cln-hold" => output.trim() == "v26.06.7\naf0055b132f3b9f24d0b1d478a15005fcf8f014f",
        "spark-operator" => {
            output.trim() == "spark-frost-signer 0.1.0\n5e0c879de2775f6a34aea6d3b0c455d065a3a70f"
        }
        "open-ssp" => output.trim() == "9c25f69924490c22a66f03a98e5840015383dc79",
        "cdk-spark-processor" => {
            output.trim()
                == "d7244e261506b51d5b309a558bf19cd4ffc32f13\n5c2bdd606e691c48adb31fd621081aeecf53d5550ee5fa1c3e1262c1afb053a6\n499a3baa6e432020bf71ecf0656fdc4c3cb91acae89adaa6c5ae7032fe7c280d"
        }
        "cdk-mint" | "cdk-mint-management" | "cdk-ldk-mint-management" => {
            let lines: Vec<_> = output.lines().filter(|line| !line.is_empty()).collect();
            lines
                == [
                    format!("cdk-mint-rpc {version}"),
                    format!("cdk-mintd {version}"),
                ]
        }
        "nutshell-mint" | "nutshell-mint-management" => {
            output.lines().next() == Some(format!("Nutshell, version {version}").as_str())
                && output.contains("Usage: cashu [OPTIONS] COMMAND [ARGS]...")
                && output.contains("Usage: mint-cli [OPTIONS] COMMAND [ARGS]...")
        }
        _ => false,
    }
}

#[cfg(test)]
fn valid_probe(repository: &str, output: &str) -> bool {
    legacy_version(repository).is_ok_and(|version| valid_probe_version(repository, version, output))
}

fn local(work: &Path) -> Result<()> {
    let id = inspect(work)?;
    let mut receipt = load(work)?;
    crate::development::regular(&work.join("probe.stdout"))?;
    let output = fs::read_to_string(work.join("probe.stdout"))?;
    ensure!(
        valid_probe_version(&receipt.repository, receipt.version()?, &output),
        "native image probe did not match its reviewed recipe"
    );
    receipt.local_image_id = Some(id);
    receipt.local_verified = true;
    save(work, &receipt)
}

fn authorize(work: &Path, namespace: &str) -> Result<()> {
    ensure!(
        namespace == NAMESPACE,
        "exact namespace confirmation required"
    );
    let mut receipt = load(work)?;
    ensure!(
        receipt.publication == Publication::Prepared,
        "publication already attempted; inspect its receipt and use verify-work, not another push"
    );
    match &receipt.input {
        Input::Build { .. } => {
            ensure!(receipt.local_verified, "local probes are incomplete");
            local(work)?;
        }
        Input::Copy { image } => {
            registry::verify(image, None, &receipt.platform, false)?;
        }
    }
    receipt.publication = Publication::UploadAttempted;
    save(work, &receipt)
}

fn published(work: &Path) -> Result<()> {
    let mut receipt = load(work)?;
    ensure!(
        matches!(
            receipt.publication,
            Publication::Uploaded | Publication::Verified
        ),
        "no recorded upload"
    );
    let result = bundle::read_json(&work.join("published.json"))?;
    let digest = text(&result, "digest")?;
    ensure!(
        digest.strip_prefix("sha256:").is_some_and(sha256),
        "invalid published digest"
    );
    if let Input::Copy { image } = &receipt.input {
        ensure!(
            registry::Reference::parse(image)?.digest == digest,
            "copied manifest digest changed"
        );
    }
    let image = format!("{NAMESPACE}/{}@{digest}", receipt.repository);
    if let Some(previous) = &receipt.image {
        ensure!(
            previous == &image,
            "published tag moved after its digest was recorded"
        );
    }
    receipt.image = Some(image.clone());
    save(work, &receipt)?;
    registry::verify(
        &image,
        receipt.local_image_id.as_deref(),
        &receipt.platform,
        matches!(receipt.input, Input::Build { .. }),
    )?;
    receipt.publication = Publication::Verified;
    save(work, &receipt)
}

fn fields(work: &Path) -> Result<()> {
    let receipt = load(work)?;
    let recipe = receipt.recipe()?;
    let (kind, source) = match &receipt.input {
        Input::Build { .. } => ("build", receipt.local_image_id.clone().unwrap_or_default()),
        Input::Copy { image } => ("copy", image.clone()),
    };
    let mut mint_image = String::new();
    if kind == "build"
        && matches!(
            receipt.repository.as_str(),
            "cdk-mint" | "cdk-ldk-mint-management"
        )
    {
        let version = receipt.version()?;
        let provenance = if version == legacy_version(&receipt.repository)? {
            "source/docker/mint/cdk-ldk-management-provenance.json".to_owned()
        } else {
            format!("source/docker/mint/cdk-{version}-provenance.json")
        };
        let value = bundle::read_json(&work.join(provenance))?;
        text(&value, "runtime_image")?.clone_into(&mut mint_image);
        ensure!(
            mint_image.starts_with("docker.io/cashubtc/mintd@sha256:")
                && super::pinned(&mint_image),
            "unreviewed LDK runtime image"
        );
    }
    let context = work.join(if receipt.repository == "cocod-wallet" {
        "context"
    } else {
        "source"
    });
    for field in [
        kind.to_owned(),
        receipt.tag,
        receipt.platform,
        source,
        work.join("source")
            .join(recipe)
            .to_string_lossy()
            .into_owned(),
        context.to_string_lossy().into_owned(),
        mint_image,
        probe(&receipt.repository)?.to_owned(),
        match &receipt.input {
            Input::Build { source, .. } => text(source, "sha256")?.into(),
            Input::Copy { .. } => String::new(),
        },
    ] {
        std::io::stdout().write_all(field.as_bytes())?;
        std::io::stdout().write_all(&[0])?;
    }
    Ok(())
}

fn verify(image: &str, platform: &str, output: &Path) -> Result<()> {
    let reference = registry::Reference::parse(image)?;
    let name = reference
        .repository
        .rsplit('/')
        .next()
        .context("missing repository")?;
    recipe(name)?;
    ensure!(
        image.starts_with(&format!("{NAMESPACE}/{name}@")),
        "only approved published catalog images may be verified here"
    );
    registry::architecture(platform)?;
    let output = output_path(output)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let result = registry::verify(image, None, platform, false);
    file.write_all(&serde_json::to_vec_pretty(&json!({"format_version":1,"image":image,"required_platform":platform,"anonymous_verified":result.is_ok(),"platforms":result.as_ref().ok().map(|r| &r.platforms),"release_ready":false,"error":result.as_ref().err().map(ToString::to_string)}))?)?;
    result?;
    Ok(())
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    let args: Vec<_> = args
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("UTF-8 arguments required"))
        })
        .collect::<Result<_>>()?;
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [
            "bark-restore",
            root,
            platform,
            revision,
            attempt,
            artifacts,
            output,
        ] => bark::restore(
            Path::new(root),
            platform,
            revision,
            attempt,
            Path::new(artifacts),
            Path::new(output),
        ),
        ["bark-stage", root, platform, candidates, output] => bark::stage(
            Path::new(root),
            platform,
            Path::new(candidates),
            Path::new(output),
        ),
        ["bark-evidence", staged, run, output] => {
            bark::evidence(Path::new(staged), Path::new(run), Path::new(output))
        }
        ["audit"] => audit::run(),
        ["list"] => {
            println!("{}", recipe_list());
            Ok(())
        }
        ["prepare", root, work, name, platform] => prepare(
            &Path::new(root).canonicalize()?,
            Path::new(work),
            name,
            platform,
            None,
        ),
        ["prepare-copy", root, work, image, platform] => {
            let reference = registry::Reference::parse(image)?;
            prepare(
                &Path::new(root).canonicalize()?,
                Path::new(work),
                current_repository(reference.repository.rsplit('/').next().unwrap()),
                platform,
                Some(image),
            )
        }
        ["fields", work] => fields(Path::new(work)),
        ["inspect", work] => {
            println!("{}", inspect(Path::new(work))?);
            Ok(())
        }
        ["local", work] => local(Path::new(work)),
        ["authorize", work, namespace] => authorize(Path::new(work), namespace),
        ["uploaded", work] => {
            let work = Path::new(work);
            let mut receipt = load(work)?;
            ensure!(
                receipt.publication == Publication::UploadAttempted,
                "publication was not authorized"
            );
            receipt.publication = Publication::Uploaded;
            save(work, &receipt)
        }
        ["recheck", work] => {
            let work = Path::new(work);
            let mut receipt = load(work)?;
            ensure!(
                matches!(
                    receipt.publication,
                    Publication::Uploaded | Publication::Verified
                ),
                "no recorded upload"
            );
            receipt.publication = Publication::Uploaded;
            save(work, &receipt)
        }
        ["published", work] => published(Path::new(work)),
        ["verify", image, platform, output] => verify(image, platform, Path::new(output)),
        _ => bail!("invalid catalog-image command; use just catalog-image help"),
    }
}

#[cfg(test)]
mod tests;
