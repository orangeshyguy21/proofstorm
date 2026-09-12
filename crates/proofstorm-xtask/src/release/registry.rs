//! Shared anonymous image verification. No Docker/GitHub credentials are read.
use super::{sha256, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, process::Command};

const ACCEPT: &str = "Accept: application/vnd.oci.image.index.v1+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json";

pub(super) fn architecture(platform: &str) -> Result<&str> {
    match platform {
        "linux/amd64" => Ok("amd64"),
        "linux/arm64" => Ok("arm64"),
        _ => bail!("image platform must be linux/amd64 or linux/arm64"),
    }
}

pub(super) struct Reference {
    pub host: String,
    pub repository: String,
    pub digest: String,
}

impl Reference {
    pub fn parse(image: &str) -> Result<Self> {
        let (name, digest) = image
            .split_once('@')
            .context("image must be digest-pinned")?;
        ensure!(
            digest.strip_prefix("sha256:").is_some_and(sha256),
            "invalid registry digest"
        );
        let (host, repository) = name
            .split_once('/')
            .context("image needs an explicit registry")?;
        let local = host
            .strip_prefix("127.0.0.1:")
            .is_some_and(|port| port.parse::<u16>().is_ok_and(|p| p > 0));
        ensure!(
            host == "ghcr.io" || local,
            "source registry must be GHCR or an explicit 127.0.0.1 port"
        );
        ensure!(
            !repository.is_empty()
                && repository.len() <= 256
                && repository
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | "..")
                        && part.as_bytes()[0].is_ascii_alphanumeric()
                        && part.bytes().all(|b| b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || b"._-".contains(&b))),
            "unsafe registry repository"
        );
        Ok(Self {
            host: host.into(),
            repository: repository.into(),
            digest: digest.into(),
        })
    }

    fn url(&self, kind: &str, digest: &str) -> Result<String> {
        ensure!(
            matches!(kind, "manifests" | "blobs")
                && digest.strip_prefix("sha256:").is_some_and(sha256),
            "invalid registry path"
        );
        let scheme = if self.host == "ghcr.io" {
            "https"
        } else {
            "http"
        };
        Ok(format!(
            "{scheme}://{}/v2/{}/{kind}/{digest}",
            self.host, self.repository
        ))
    }
}

fn request(url: &str, token: Option<&str>, head: bool, local: bool) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "--fail",
        "--silent",
        "--show-error",
        "--max-time",
        "60",
        "-H",
        ACCEPT,
    ]);
    if local {
        // Do not follow a loopback registry redirect to some unrelated host.
        command.args(["--proto", "=http", "--max-redirs", "0"]);
    } else {
        command.args([
            "--location",
            "--max-redirs",
            "5",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ]);
    }
    if let Some(token) = token {
        command.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    if head {
        command.arg("--head");
    } else {
        command.args(["--max-filesize", "4194304"]);
    }
    let output = command.arg(url).output()?;
    ensure!(
        output.status.success(),
        "anonymous registry request failed; check public visibility, digest, and layer availability"
    );
    ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "registry response too large"
    );
    if head {
        check_head(&output.stdout)?;
    }
    Ok(output.stdout)
}

fn check_head(headers: &[u8]) -> Result<()> {
    let status = std::str::from_utf8(headers)?
        .lines()
        .filter(|line| line.starts_with("HTTP/"))
        .next_back()
        .and_then(|line| line.split_whitespace().nth(1));
    ensure!(status == Some("200"), "registry layer is not available");
    Ok(())
}

#[derive(Debug)]
pub(super) struct Inspection {
    pub platforms: BTreeSet<String>,
    identities: BTreeSet<String>,
    runnable: usize,
}

fn inspect_with(
    digest: &str,
    depth: usize,
    expected: Option<&str>,
    fetch: &mut impl FnMut(&str, &str, bool) -> Result<Vec<u8>>,
    result: &mut Inspection,
) -> Result<()> {
    fn metadata(
        kind: &str,
        digest: &str,
        fetch: &mut impl FnMut(&str, &str, bool) -> Result<Vec<u8>>,
    ) -> Result<Value> {
        ensure!(
            digest.strip_prefix("sha256:").is_some_and(sha256),
            "invalid registry digest"
        );
        let bytes = fetch(kind, digest, false)?;
        ensure!(
            bytes.len() <= 4 * 1024 * 1024
                && format!("sha256:{:x}", Sha256::digest(&bytes)) == digest,
            "registry content digest mismatch"
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
    ensure!(depth <= 3, "registry index nesting exceeds limit");
    let value = metadata("manifests", digest, fetch)?;
    ensure!(
        value["schemaVersion"] == 2,
        "unsupported registry manifest schema"
    );
    result.identities.insert(digest.into());
    if let Some(children) = value.get("manifests") {
        let children = children.as_array().context("invalid registry index")?;
        ensure!(
            !children.is_empty() && children.len() <= 100,
            "invalid registry child count"
        );
        for child in children {
            if child["platform"]["os"] == "unknown" {
                continue;
            }
            let platform = child
                .get("platform")
                .map(|p| -> Result<String> {
                    let platform = format!("{}/{}", text(p, "os")?, text(p, "architecture")?);
                    architecture(&platform)?;
                    if let Some(expected) = expected {
                        ensure!(platform == expected, "nested index platform mismatch");
                    }
                    Ok(platform)
                })
                .transpose()?;
            inspect_with(
                text(child, "digest")?,
                depth + 1,
                platform.as_deref().or(expected),
                fetch,
                result,
            )?;
        }
        return Ok(());
    }
    let config_digest = text(&value["config"], "digest")?;
    let config = metadata("blobs", config_digest, fetch)?;
    let platform = format!(
        "{}/{}",
        text(&config, "os")?,
        text(&config, "architecture")?
    );
    architecture(&platform)?;
    if let Some(expected) = expected {
        ensure!(
            platform == expected,
            "image config/descriptor platform mismatch"
        );
    }
    let layers = value["layers"].as_array().context("missing image layers")?;
    ensure!(
        !layers.is_empty() && layers.len() <= 100,
        "invalid image layer count"
    );
    for layer in layers {
        let digest = text(layer, "digest")?;
        ensure!(
            digest.strip_prefix("sha256:").is_some_and(sha256),
            "invalid layer digest"
        );
        fetch("blobs", digest, true)?;
    }
    result.identities.insert(config_digest.into());
    result.platforms.insert(platform);
    result.runnable += 1;
    ensure!(result.runnable <= 100, "too many runnable image manifests");
    Ok(())
}

fn check(
    digest: &str,
    identity: Option<&str>,
    platform: &str,
    exact: bool,
    fetch: &mut impl FnMut(&str, &str, bool) -> Result<Vec<u8>>,
) -> Result<Inspection> {
    architecture(platform)?;
    if let Some(identity) = identity {
        ensure!(
            identity.strip_prefix("sha256:").is_some_and(sha256),
            "invalid local identity"
        );
    }
    let mut result = Inspection {
        platforms: BTreeSet::new(),
        identities: BTreeSet::new(),
        runnable: 0,
    };
    inspect_with(digest, 0, None, fetch, &mut result)?;
    ensure!(
        result.platforms.contains(platform),
        "image lacks required platform {platform}"
    );
    if exact {
        ensure!(
            result.runnable == 1 && result.platforms.len() == 1,
            "expected exactly one runnable image"
        );
    }
    if let Some(identity) = identity {
        ensure!(
            result.identities.contains(identity),
            "published image differs from verified local identity"
        );
    }
    Ok(result)
}

pub(super) fn verify(
    image: &str,
    identity: Option<&str>,
    platform: &str,
    exact: bool,
) -> Result<Inspection> {
    architecture(platform)?;
    let reference = Reference::parse(image)?;
    let local = reference.host != "ghcr.io";
    let token = if local {
        None
    } else {
        let response = request(
            &format!(
                "https://ghcr.io/token?service=ghcr.io&scope=repository:{}:pull",
                reference.repository
            ),
            None,
            false,
            false,
        )?;
        let response: Value = serde_json::from_slice(&response)?;
        let token = text(&response, "token")?;
        ensure!(
            !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic()),
            "invalid anonymous pull token"
        );
        Some(token.to_owned())
    };
    check(
        &reference.digest,
        identity,
        platform,
        exact,
        &mut |kind, digest, head| {
            request(&reference.url(kind, digest)?, token.as_deref(), head, local)
        },
    )
}

#[cfg(test)]
pub(super) fn test_url(digest: &str) -> Result<String> {
    Reference::parse(&format!(
        "ghcr.io/orangeshyguy21/proofstorm/proofstormd@{digest}"
    ))?
    .url("manifests", digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn put(graph: &mut BTreeMap<String, Vec<u8>>, value: &Value) -> String {
        let bytes = serde_json::to_vec(value).unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        graph.insert(digest.clone(), bytes);
        digest
    }

    #[test]
    fn layer_heads_require_a_successful_final_response() {
        for headers in [
            "HTTP/1.1 200 OK\r\n\r\n",
            "HTTP/1.1 307 Temporary Redirect\r\n\r\nHTTP/2 200\r\n\r\n",
        ] {
            check_head(headers.as_bytes()).unwrap();
        }
        for headers in [
            "",
            "HTTP/1.1 301 Moved Permanently\r\n\r\n",
            "HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 404\r\n\r\n",
        ] {
            assert!(check_head(headers.as_bytes()).is_err());
        }
    }

    #[test]
    fn multi_platform_copies_verify_every_runnable_manifest() {
        let mut graph = BTreeMap::new();
        let mut manifests = Vec::new();
        for arch in ["amd64", "arm64"] {
            let config = put(&mut graph, &json!({"os":"linux","architecture":arch}));
            let manifest = put(
                &mut graph,
                &json!({"schemaVersion":2,"config":{"digest":config},"layers":[{"digest":format!("sha256:{}", "a".repeat(64))}]}),
            );
            manifests
                .push(json!({"digest":manifest,"platform":{"os":"linux","architecture":arch}}));
        }
        let index = put(
            &mut graph,
            &json!({"schemaVersion":2,"manifests":manifests}),
        );
        let mut heads = 0;
        let mut fetch = |_: &str, digest: &str, head| {
            Ok(if head {
                heads += 1;
                vec![]
            } else {
                graph[digest].clone()
            })
        };
        let result = check(&index, None, "linux/amd64", false, &mut fetch).unwrap();
        assert_eq!(
            result.platforms,
            BTreeSet::from(["linux/amd64".into(), "linux/arm64".into()])
        );
        assert!(check(&index, None, "linux/arm64", true, &mut fetch).is_err());
        assert_eq!(heads, 4);
    }

    #[test]
    fn both_architectures_check_bytes_layers_descriptors_and_local_identities() {
        for arch in ["amd64", "arm64"] {
            let mut graph = BTreeMap::new();
            let config = put(&mut graph, &json!({"os":"linux","architecture":arch}));
            let layer = format!("sha256:{}", "a".repeat(64));
            let manifest = put(
                &mut graph,
                &json!({"schemaVersion":2,"config":{"digest":config},"layers":[{"digest":layer}]}),
            );
            let index = put(
                &mut graph,
                &json!({"schemaVersion":2,"manifests":[{"digest":manifest,"platform":{"os":"linux","architecture":arch}}]}),
            );
            let platform = format!("linux/{arch}");
            for identity in [&config, &manifest, &index] {
                let mut heads = 0;
                check(
                    &index,
                    Some(identity),
                    &platform,
                    true,
                    &mut |_, digest, head| {
                        if head {
                            assert_eq!(digest, layer);
                            heads += 1;
                            Ok(vec![])
                        } else {
                            Ok(graph[digest].clone())
                        }
                    },
                )
                .unwrap();
                assert_eq!(heads, 1);
            }
            for (wrong_platform, wrong_identity, missing_layer, corrupt) in [
                (true, false, false, false),
                (false, true, false, false),
                (false, false, true, false),
                (false, false, false, true),
            ] {
                assert!(
                    check(
                        &index,
                        Some(if wrong_identity { &layer } else { &config }),
                        if wrong_platform {
                            "linux/386"
                        } else {
                            &platform
                        },
                        true,
                        &mut |_, digest, head| {
                            if head && missing_layer {
                                bail!("missing layer");
                            }
                            Ok(if head {
                                vec![]
                            } else if corrupt {
                                b"changed".to_vec()
                            } else {
                                graph[digest].clone()
                            })
                        }
                    )
                    .is_err()
                );
            }
            let wrong = put(
                &mut graph,
                &json!({"schemaVersion":2,"manifests":[{"digest":manifest,"platform":{"os":"linux","architecture":if arch == "arm64" {"amd64"} else {"arm64"}}}]}),
            );
            assert!(
                check(&wrong, None, &platform, false, &mut |_, digest, head| Ok(
                    if head { vec![] } else { graph[digest].clone() }
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn references_refuse_mutable_tags_credentials_and_implicit_local_registry() {
        let sha = "a".repeat(64);
        for name in [
            "ghcr.io/x/y:latest",
            "http://127.0.0.1:5000/x",
            "localhost:5111/x",
            "ghcr.io/x/../y",
            "ghcr.io/user:password@x/y",
            "127.0.0.1:0/x",
        ] {
            assert!(
                Reference::parse(&format!("{name}@sha256:{sha}")).is_err(),
                "{name}"
            );
        }
        assert!(Reference::parse(&format!("127.0.0.1:54321/bitcoin-core@sha256:{sha}")).is_ok());
    }
}
