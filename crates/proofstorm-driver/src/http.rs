//! Bounded native HTTP access. Responses never become public error messages.
use anyhow::{Context, Result, ensure};
use reqwest::{Client, RequestBuilder, StatusCode, Url, redirect::Policy};
use serde_json::Value;
use std::time::Duration;

pub const MAX_BODY: usize = 1_048_576;

/// Construct a client with a total deadline and no ambient proxy or redirects.
/// # Errors
/// Returns a client initialization error.
pub fn client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(timeout)
        .build()?)
}

/// Parse a response under a fixed memory bound, preserving protocol error codes.
/// # Errors
/// Rejects transport failures, oversized bodies and malformed JSON.
pub async fn json(request: RequestBuilder) -> Result<(StatusCode, Value)> {
    let mut response = request.send().await?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= MAX_BODY.saturating_sub(bytes.len()),
            "response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, serde_json::from_slice(&bytes)?))
}

/// Restrict readiness to the mint's actual loopback info endpoint.
/// # Errors
/// Rejects remote hosts, credentials, redirects and transactional paths.
pub fn local_info_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path() == "/v1/info"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "readiness requires local mint info"
    );
    Ok(url)
}

/// Check only response headers. A large or stalled body cannot delay readiness.
/// # Errors
/// Rejects invalid readiness URLs, timeouts and non-success status codes.
pub async fn mint_ready(value: &str) -> Result<()> {
    let url = local_info_url(value)?;
    let response = client(Duration::from_secs(1))?.get(url).send().await?;
    ensure!(response.status().is_success(), "mint readiness failed");
    Ok(())
}

/// Probe Coco's pinned local health contract without initializing a session.
/// # Errors
/// Rejects timeouts, malformed responses and incompatible interface versions.
pub async fn coco_ready() -> Result<()> {
    let (status, value) =
        json(client(Duration::from_secs(2))?.get("http://127.0.0.1:62626/health")).await?;
    ensure!(
        status.is_success() && value["status"] == "ok" && value["interfaceVersion"] == "1",
        "coco readiness failed"
    );
    Ok(())
}

/// Read a required non-empty environment value without including it in errors.
/// # Errors
/// Returns a fixed error for missing or empty values.
pub fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .context("required driver input unavailable")
}
