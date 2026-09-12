//! Catalog image maintenance, separate from controller/release promotion and runtime setup.
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
    "cdk-mint-management",
    "cdk-ldk-mint-management",
    "nutshell-mint-management",
    "cdk-cli-wallet",
    "cocod-wallet",
];

fn recipe(name: &str) -> Result<&'static str> {
    Ok(match name {
        "bitcoin-core" => "docker/bitcoin/Dockerfile",
        "cdk-mint-management" | "cdk-ldk-mint-management" => "docker/mint/Dockerfile.kube-cdk",
        "nutshell-mint-management" => "docker/mint/Dockerfile.kube-nutshell",
        "cdk-cli-wallet" => "docker/wallet/Dockerfile.kube-cdk",
        "cocod-wallet" => "docker/wallet/Dockerfile.kube-cocod",
        _ => bail!("unknown catalog image; controller builds use release-controller-build"),
    })
}

fn probe(name: &str) -> Result<&'static str> {
    Ok(match name {
        "bitcoin-core" => "bitcoind --version",
        "cdk-mint-management" | "cdk-ldk-mint-management" => {
            "cdk-mint-cli --version && cdk-mintd --version"
        }
        "nutshell-mint-management" => "mint-cli --help",
        "cdk-cli-wallet" => "cdk-cli --version",
        "cocod-wallet" => "cocod --version",
        _ => bail!("unknown catalog probe"),
    })
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Input {
    Build {
        source: Value,
        recipe_sha256: String,
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
    fn validate(&self) -> Result<()> {
        recipe(&self.repository)?;
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
            Input::Copy { image } => ensure!(
                registry::Reference::parse(image)?
                    .repository
                    .rsplit('/')
                    .next()
                    == Some(self.repository.as_str())
                    && self.local_image_id.is_none()
                    && !self.local_verified,
                "copy source repository changed"
            ),
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
    } = &receipt.input
    {
        build::verify_snapshot(&work.join("source"), source)?;
        ensure!(
            format!(
                "{:x}",
                Sha256::digest(fs::read(
                    work.join("source").join(recipe(&receipt.repository)?)
                )?)
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
    recipe(name)?;
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
            reference.repository.rsplit('/').next() == Some(name),
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
            Sha256::digest(fs::read(staging.path().join("source").join(recipe(name)?))?)
        );
        if name == "cocod-wallet" {
            prepare_cocod(staging.path(), &recipe_sha256)?;
        }
        Input::Build {
            source,
            recipe_sha256,
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
        "bitcoin-core" | "cdk-cli-wallet" | "cocod-wallet"
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

fn valid_probe(repository: &str, output: &str) -> bool {
    match repository {
        "bitcoin-core" => matches!(
            output.lines().next(),
            Some("Bitcoin Core version v31.1" | "Bitcoin Core version v31.1.0")
        ),
        "cdk-cli-wallet" => output.trim() == "cdk-cli 0.18.0",
        "cocod-wallet" => output.trim() == "0.0.17",
        "cdk-mint-management" | "cdk-ldk-mint-management" => {
            let lines: Vec<_> = output.lines().filter(|line| !line.is_empty()).collect();
            lines == ["cdk-mint-rpc 0.18.0", "cdk-mintd 0.18.0"]
        }
        "nutshell-mint-management" => output.contains("Usage:") && output.contains("--help"),
        _ => false,
    }
}

fn local(work: &Path) -> Result<()> {
    let id = inspect(work)?;
    let mut receipt = load(work)?;
    crate::development::regular(&work.join("probe.stdout"))?;
    let output = fs::read_to_string(work.join("probe.stdout"))?;
    ensure!(
        valid_probe(&receipt.repository, &output),
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
    let (kind, source) = match &receipt.input {
        Input::Build { .. } => ("build", receipt.local_image_id.clone().unwrap_or_default()),
        Input::Copy { image } => ("copy", image.clone()),
    };
    let mut mint_image = String::new();
    if kind == "build" && receipt.repository == "cdk-ldk-mint-management" {
        let value =
            bundle::read_json(&work.join("source/docker/mint/cdk-ldk-management-provenance.json"))?;
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
            .join(recipe(&receipt.repository)?)
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
        ["list"] => {
            println!(
                "Catalog recipes (linux/amd64 or linux/arm64):\n{}",
                RECIPES.join("\n")
            );
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
                reference.repository.rsplit('/').next().unwrap(),
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
