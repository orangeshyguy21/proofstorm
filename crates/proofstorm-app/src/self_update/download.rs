use super::{process::Cancellation, schema::Asset};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, time::Duration};

pub(super) trait Download {
    async fn metadata(&self, cancel: &mut Cancellation) -> Result<Vec<u8>>;
    async fn asset(&self, asset: &Asset, path: &Path, cancel: &mut Cancellation) -> Result<()>;
}

pub struct Http(reqwest::Client);
impl Http {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(180))
                .user_agent(concat!("proofstorm/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if allowed_redirect(attempt.url(), attempt.previous().len()) {
                        attempt.follow()
                    } else {
                        attempt.error("unexpected release download redirect")
                    }
                }))
                .build()?,
        ))
    }
    async fn get(&self, url: &str, seconds: u64) -> Result<reqwest::Response> {
        let mut last = None;
        for _ in 0..3 {
            match self
                .0
                .get(url)
                .timeout(Duration::from_secs(seconds))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) if !response.status().is_server_error() => {
                    anyhow::bail!("release request returned HTTP {}", response.status());
                }
                Ok(response) => last = Some(response.error_for_status().unwrap_err()),
                Err(error) => last = Some(error),
            }
        }
        Err(last.expect("three attempts").into())
    }
}
fn allowed_redirect(url: &reqwest::Url, previous: usize) -> bool {
    previous < 5
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none_or(|p| p == 443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "proofstorm.com"
            )
        )
}

impl Download for Http {
    async fn metadata(&self, cancel: &mut Cancellation) -> Result<Vec<u8>> {
        let fetch = async {
            let mut response = self.get(super::schema::ENDPOINT, 20).await?;
            ensure!(
                response.content_length().is_none_or(|n| n <= 65536),
                "release metadata exceeds size limit"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    bytes.len() + chunk.len() <= 65536,
                    "release metadata exceeds size limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        };
        tokio::select! { result = fetch => result, () = cancel.cancelled() => anyhow::bail!("update cancelled") }
    }
    async fn asset(&self, asset: &Asset, path: &Path, cancel: &mut Cancellation) -> Result<()> {
        let fetch = async {
            let mut response = self.get(&asset.url, 180).await?;
            ensure!(
                response.content_length().is_none_or(|n| n == asset.bytes),
                "asset content length differs from release metadata"
            );
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            let mut digest = Sha256::new();
            let mut bytes = 0_u64;
            while let Some(chunk) = response.chunk().await? {
                bytes += chunk.len() as u64;
                ensure!(bytes <= asset.bytes, "asset exceeds advertised size");
                digest.update(&chunk);
                file.write_all(&chunk)?;
            }
            ensure!(
                bytes == asset.bytes && format!("{:x}", digest.finalize()) == asset.sha256,
                "asset hash or size differs from selected release"
            );
            file.sync_all()?;
            Ok(())
        };
        tokio::select! { result = fetch => result, () = cancel.cancelled() => anyhow::bail!("update cancelled") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn redirect_policy_rejects_downgrades_untrusted_hosts_and_loops() {
        for url in [
            "http://github.com/asset",
            "https://github.com.evil.test/asset",
            "https://github.com@evil.test/asset",
            "https://user@github.com/asset",
            "https://github.com:444/asset",
        ] {
            assert!(!allowed_redirect(&url.parse().unwrap(), 1), "{url}");
        }
        for host in [
            "github.com",
            "release-assets.githubusercontent.com",
            "objects.githubusercontent.com",
            "proofstorm.com",
        ] {
            let url = format!("https://{host}/asset").parse().unwrap();
            assert!(allowed_redirect(&url, 4));
            assert!(!allowed_redirect(&url, 5));
        }
    }

    #[tokio::test]
    async fn downloaded_bytes_must_match_captured_size_and_digest() {
        for (status, body, declared, expected, digest_ok, success) in [
            ("200 OK", "payload", 7, 7, true, true),
            ("200 OK", "payload", 7, 7, false, false),
            ("200 OK", "payload", 7, 8, true, false),
            ("200 OK", "short", 7, 7, true, false),
            ("302 Found", "payload", 7, 7, true, false),
            ("404 Not Found", "payload", 7, 7, true, false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/asset", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                    assert!(request.len() < 4096);
                }
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            });
            let root = tempfile::tempdir().unwrap();
            let asset = Asset {
                name: "asset".into(),
                url,
                bytes: expected,
                sha256: format!(
                    "{:x}",
                    Sha256::digest(if digest_ok { b"payload" } else { b"changed" })
                ),
            };
            // Only the transport test uses loopback HTTP. Production selection
            // requires the exact trusted HTTPS URL before calling this method.
            let http = Http(reqwest::Client::builder().no_proxy().build().unwrap());
            let result = http
                .asset(
                    &asset,
                    &root.path().join("asset"),
                    &mut Cancellation::new().unwrap(),
                )
                .await;
            assert_eq!(result.is_ok(), success, "{status}: {result:?}");
            if success {
                assert_eq!(
                    std::fs::read(root.path().join("asset")).unwrap(),
                    b"payload"
                );
            }
            server.await.unwrap();
        }
    }
}
