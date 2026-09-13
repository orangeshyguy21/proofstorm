//! Native Coco HTTP integration. Secrets stay in private files or request bodies.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Coco {
    root: PathBuf,
    passphrase: PathBuf,
    base: String,
    client: reqwest::Client,
}
fn private_text(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(metadata.is_file(), "private input is not a file");
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(16_385).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= 16_384,
        "private input size invalid"
    );
    Ok(String::from_utf8(bytes)?)
}
impl Coco {
    /// # Errors
    /// Returns an error if the bounded direct HTTP client cannot be created.
    pub fn new(root: &Path, passphrase: &Path, base: &str) -> Result<Self> {
        let url = reqwest::Url::parse(base)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Coco endpoint must be local"
        );
        Ok(Self {
            root: root.into(),
            passphrase: passphrase.into(),
            base: base.trim_end_matches('/').into(),
            client: crate::http::client(Duration::from_secs(20))?,
        })
    }
    async fn api(&self, path: &str, payload: Option<Value>) -> Result<Value> {
        let key = private_text(&self.root.join("credentials/current/client"))?;
        let url = format!("{}{path}", self.base);
        let request = if let Some(payload) = payload {
            self.client.post(url).json(&payload)
        } else {
            self.client.get(url)
        };
        let (status, value) = crate::http::json(request.bearer_auth(key.trim())).await?;
        ensure!(status.is_success(), "Coco API request failed");
        ensure!(value.get("error").is_none(), "Coco API returned an error");
        Ok(value)
    }
    async fn healthy(&self) -> Result<()> {
        let (status, value) =
            crate::http::json(self.client.get(format!("{}/health", self.base))).await?;
        ensure!(
            status.is_success() && value["status"] == "ok",
            "Coco health failed"
        );
        Ok(())
    }
    async fn session(&self, expected: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let state = self.api("/v1/status", None).await?;
                if state["cocoSession"]["state"] == expected {
                    return Ok(());
                }
                ensure!(
                    state["cocoSession"]["state"] != "failed",
                    "native session failed"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await?
    }
    fn create_passphrase(&self) -> Result<String> {
        let passphrase = cashu::secret::Secret::generate().to_string();
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.passphrase)?;
        file.write_all(passphrase.as_bytes())?;
        file.sync_all()?;
        Ok(passphrase)
    }
    async fn initialize(&self, mint: &str) -> Result<()> {
        let passphrase = self.create_passphrase()?;
        let value = self
            .api(
                "/v1/admin/wallet/initialize",
                Some(json!({"passphrase":passphrase})),
            )
            .await?;
        ensure!(
            value["generatedMnemonic"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                && value["status"]["cocoSession"]["state"] == "stopped",
            "Coco initialization contract failed"
        );
        let path = self.root.join("config.json");
        let mut config: Value = serde_json::from_str(&private_text(&path)?)?;
        ensure!(config["encrypted"] == true, "Coco wallet is not encrypted");
        config
            .as_object_mut()
            .context("invalid Coco configuration")?
            .insert("mintUrl".into(), json!(mint));
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(&mut temporary, &config)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }
    async fn balance(&self, mint: &str, expected: u64, wait: bool) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let balance = self.api("/balance", None).await?;
                if balance["output"][mint]["sats"].as_u64() == Some(expected) {
                    return Ok(json!({"native_ready_total_sat":expected}));
                }
                ensure!(wait, "native balance differs");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await?
    }
    async fn uninitialized(&self) -> Result<Value> {
        self.healthy().await?;
        let state = self.api("/v1/status", None).await?;
        ensure!(
            state.get("wallet") == Some(&Value::Null) && state["cocoSession"]["state"] == "stopped",
            "Coco wallet already initialized"
        );
        let status = self
            .client
            .get(format!("{}/v1/status", self.base))
            .send()
            .await?
            .status();
        ensure!(
            status == reqwest::StatusCode::UNAUTHORIZED,
            "Coco accepted unauthenticated status"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                fs::metadata(self.root.join("credentials/current/client"))?
                    .permissions()
                    .mode()
                    & 0o777
                    == 0o600,
                "Coco credential permissions differ"
            );
        }
        Ok(json!({"healthy":true,"initialized":false,"unauthenticated_status":401}))
    }

    /// Execute an explicit integration check or requested wallet action.
    /// # Errors
    /// Fails on mismatched state, bad credentials, failed HTTP or a bounded deadline.
    pub async fn run(&self, mode: &str, args: &[String]) -> Result<Value> {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        match (mode, args.as_slice()) {
            ("uninitialized", []) => self.uninitialized().await,
            ("initialize", [mint]) => {
                self.initialize(mint).await?;
                Ok(json!({"initialized":true}))
            }
            ("start", []) => {
                self.api(
                    "/v1/admin/session/start",
                    Some(json!({"passphrase":private_text(&self.passphrase)?})),
                )
                .await?;
                self.session("running").await?;
                Ok(json!({"session":"running"}))
            }
            ("locked", []) => {
                let status = self.api("/v1/status", None).await?;
                ensure!(
                    status["seedAccess"]["state"] == "locked"
                        && status["cocoSession"]["state"] == "stopped",
                    "Coco session was not locked"
                );
                Ok(json!({"locked":true}))
            }
            ("session", [expected]) if matches!(*expected, "running" | "stopped") => {
                self.session(expected).await?;
                self.healthy().await?;
                Ok(json!({"session":expected,"healthy":true}))
            }
            ("identity", []) => {
                let value = self
                    .api(
                        "/v1/admin/wallet/recovery-material",
                        Some(json!({"passphrase":private_text(&self.passphrase)?})),
                    )
                    .await?;
                let mnemonic = value["mnemonic"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("missing recovery material")?;
                Ok(json!(format!("{:x}", Sha256::digest(mnemonic.as_bytes()))))
            }
            ("balance" | "wait-balance", [mint, expected]) => {
                self.balance(mint, expected.parse()?, mode == "wait-balance")
                    .await
            }
            ("receive", []) => {
                let mut token = String::new();
                std::io::stdin()
                    .take(1_048_577)
                    .read_to_string(&mut token)?;
                ensure!(
                    !token.is_empty() && token.len() <= 1_048_576,
                    "private token size invalid"
                );
                let result = self
                    .api("/receive/cashu", Some(json!({"token":token})))
                    .await?;
                ensure!(
                    result.get("output").is_some(),
                    "Coco receive result missing"
                );
                Ok(json!({"received":true}))
            }
            #[cfg(unix)]
            ("initialize-cli", []) => {
                use std::os::unix::process::CommandExt;
                let passphrase = self.create_passphrase()?;
                Err(std::process::Command::new("cocod")
                    .args(["wallet", "initialize", "--passphrase", &passphrase])
                    .exec()
                    .into())
            }
            _ => bail!("unsupported Coco action"),
        }
    }
}
