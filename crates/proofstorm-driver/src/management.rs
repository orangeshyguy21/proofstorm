//! Nutshell's public management protobuf contract, without a Python SDK.
//! Wire fields follow cashubtc/nutshell 0.20.3, package `cashu`, service `Mint`.
use anyhow::{Context, Result};
use serde::Serialize;
use std::{path::Path, time::Duration};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

#[derive(Clone, PartialEq, prost::Message)]
pub struct Empty {}

// Unknown protobuf fields are intentionally ignored; never project quote or key data.
#[derive(Clone, PartialEq, prost::Message, Serialize)]
pub struct MintInfo {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub version: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub description: Option<String>,
}

/// Authenticate both peers, check the certificate's address, and bound the whole call.
/// # Errors
/// Rejects missing credentials, TLS/transport errors and failed management RPCs.
pub async fn nutshell_info(address: &str, tls: &Path) -> Result<MintInfo> {
    inspect(address, tls, "client").await
}

/// Exercise the same management wire contract with explicit TLS identities.
/// Non-client modes exist for acceptance checks that must prove server rejection.
/// # Errors
/// Rejects unknown modes, incomplete credentials, transport failures and RPC errors.
pub async fn inspect(address: &str, tls: &Path, identity: &str) -> Result<MintInfo> {
    tokio::time::timeout(Duration::from_secs(1), async {
        anyhow::ensure!(
            matches!(identity, "client" | "server" | "missing" | "plaintext"),
            "unknown identity mode"
        );
        let mut endpoint = Endpoint::from_shared(address.to_owned())?;
        if identity == "plaintext" {
            anyhow::ensure!(address.starts_with("http://"), "plaintext requires HTTP");
        } else {
            anyhow::ensure!(address.starts_with("https://"), "TLS requires HTTPS");
            let mut config = ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(std::fs::read(tls.join("ca.pem"))?));
            if identity != "missing" {
                config = config.identity(Identity::from_pem(
                    std::fs::read(tls.join(format!("{identity}.pem")))?,
                    std::fs::read(tls.join(format!("{identity}.key")))?,
                ));
            }
            endpoint = endpoint.tls_config(config)?;
        }
        // Tonic connects directly to this endpoint; it never consults proxy env vars.
        let channel = endpoint
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(1))
            .connect()
            .await?;
        let mut client = tonic::client::Grpc::new(channel).max_decoding_message_size(131_072);
        client.ready().await?;
        let response = client
            .unary(
                tonic::Request::new(Empty {}),
                tonic::codegen::http::uri::PathAndQuery::from_static("/cashu.Mint/GetInfo"),
                tonic_prost::ProstCodec::<Empty, MintInfo>::default(),
            )
            .await?;
        Ok(response.into_inner())
    })
    .await
    .context("management readiness deadline")?
}

/// Readiness consumes neither an auth token nor a transactional HTTP request.
/// # Errors
/// Rejects nonlocal info URLs before loading credentials or opening sockets.
pub async fn nutshell_ready(url: &str) -> Result<()> {
    crate::http::local_info_url(url)?;
    nutshell_info(
        "https://127.0.0.1:8086",
        Path::new("/management-client/tls"),
    )
    .await?;
    crate::http::mint_ready(url).await
}
