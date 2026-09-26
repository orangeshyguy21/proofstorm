//! Read-only CDK payment-processor handshake (protocol 4.0.0).
use anyhow::{Context, Result};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, str::FromStr, time::Duration};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

#[derive(Clone, PartialEq, prost::Message)]
pub struct Empty {}

#[derive(Clone, PartialEq, prost::Message, Serialize)]
pub struct Bolt11Settings {
    #[prost(bool, tag = "1")]
    pub mpp: bool,
    #[prost(bool, tag = "2")]
    pub amountless: bool,
    #[prost(bool, tag = "5")]
    pub invoice_description: bool,
}

#[derive(Clone, PartialEq, prost::Message, Serialize)]
pub struct Bolt12Settings {
    #[prost(bool, tag = "2")]
    pub amountless: bool,
    #[prost(bool, tag = "3")]
    pub invoice_description: bool,
}

#[derive(Clone, PartialEq, prost::Message, Serialize)]
pub struct OnchainSettings {
    #[prost(uint32, tag = "1")]
    pub confirmations: u32,
    #[prost(uint64, tag = "2")]
    pub min_receive_amount_sat: u64,
    #[prost(uint64, tag = "3")]
    pub min_send_amount_sat: u64,
}

#[derive(Clone, PartialEq, prost::Message, Serialize)]
pub struct Settings {
    #[prost(string, tag = "1")]
    pub unit: String,
    #[prost(message, optional, tag = "2")]
    pub bolt11: Option<Bolt11Settings>,
    #[prost(message, optional, tag = "3")]
    pub bolt12: Option<Bolt12Settings>,
    #[prost(btree_map = "string, string", tag = "4")]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, String>,
    #[prost(message, optional, tag = "5")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onchain: Option<OnchainSettings>,
}

/// Exact capability profiles, independent of mint-side sat conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    LdkServer,
    Bark,
}

impl FromStr for Profile {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "cdk-ldk-server-processor" => Ok(Self::LdkServer),
            "cdk-bark-processor" => Ok(Self::Bark),
            _ => anyhow::bail!("unknown payment processor profile"),
        }
    }
}

impl Profile {
    /// Check the complete advertised rail set before CDK registers it.
    /// # Errors
    /// Rejects missing, extra, or incompatible payment methods and units.
    pub fn validate(self, settings: &Settings) -> Result<()> {
        let (unit, bolt12) = match self {
            Self::LdkServer => ("msat", true),
            Self::Bark => ("sat", false),
        };
        anyhow::ensure!(
            settings.unit == unit
                && settings.bolt11.is_some()
                && settings.bolt12.is_some() == bolt12
                && settings.custom.is_empty()
                && settings.onchain.is_none(),
            "payment processor settings do not match {self:?}: expected {unit}, BOLT11{} and no other rails",
            if bolt12 { ", BOLT12" } else { "" }
        );
        Ok(())
    }
}

/// Perform a bounded, mutually authenticated `GetSettings` call without payment mutations.
/// # Errors
/// Rejects non-TLS endpoints, missing credentials, protocol mismatch and failed RPCs.
pub async fn settings(address: &str, tls: &Path) -> Result<Settings> {
    settings_for(address, tls, Profile::LdkServer).await
}

/// Perform the same authenticated handshake for an explicitly selected profile.
/// # Errors
/// Rejects non-TLS endpoints, invalid credentials, failed RPCs and wrong capabilities.
pub async fn settings_for(address: &str, tls: &Path, profile: Profile) -> Result<Settings> {
    anyhow::ensure!(
        address.starts_with("https://"),
        "processor readiness requires TLS"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(std::fs::read(tls.join("ca.pem"))?))
            .identity(Identity::from_pem(
                std::fs::read(tls.join("client.pem"))?,
                std::fs::read(tls.join("client.key"))?,
            ));
        let channel = Endpoint::from_shared(address.to_owned())?
            .tls_config(tls)?
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(1))
            .connect()
            .await?;
        let mut client = tonic::client::Grpc::new(channel).max_decoding_message_size(65_536);
        client.ready().await?;
        let mut request = tonic::Request::new(Empty {});
        request
            .metadata_mut()
            .insert("x-cdk-protocol-version", "4.0.0".parse()?);
        let response = client
            .unary(
                request,
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/cdk_payment_processor.CdkPaymentProcessor/GetSettings",
                ),
                tonic_prost::ProstCodec::<Empty, Settings>::default(),
            )
            .await?;
        let settings: Settings = response.into_inner();
        profile.validate(&settings)?;
        Ok(settings)
    })
    .await
    .context("payment processor readiness deadline")?
}

/// Replace this helper with the native processor after loading private node credentials.
/// The key is never placed in a public configuration document or process argument.
/// # Errors
/// Rejects invalid credential files and a failed native exec.
#[cfg(unix)]
pub fn exec_ldk_processor() -> Result<()> {
    use std::{fmt::Write as _, os::unix::process::CommandExt};
    let key = std::fs::read("/ldk-server/regtest/api_key").context("read private LDK API key")?;
    anyhow::ensure!(key.len() == 32, "invalid LDK API key length");
    let encoded = key
        .iter()
        .fold(String::with_capacity(key.len() * 2), |mut encoded, byte| {
            write!(encoded, "{byte:02x}").expect("write hexadecimal credential");
            encoded
        });
    let error = std::process::Command::new("cdk-payment-processor-ldk-server")
        .env("LDK_API_KEY", encoded)
        .exec();
    Err(error).context("execute native LDK payment processor")
}
