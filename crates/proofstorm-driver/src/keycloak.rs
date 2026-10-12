//! Reconcile mutable realm policy without reimporting users or rotating credentials.
use anyhow::{Context, Result, ensure};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

/// Apply the authored lifetime to the local, already initialized Keycloak realm.
/// Kubernetes retries this bounded, idempotent startup step until it succeeds;
/// readiness remains disabled until the policy is verified.
/// # Errors
/// Rejects invalid policy, unavailable credentials, failed requests or unapplied policy.
pub async fn configure(port: u16, lifetime: u64, password: &str) -> Result<()> {
    ensure!(
        port != 0 && (60..=3_600).contains(&lifetime),
        "invalid OIDC policy"
    );
    ensure!(
        !password.is_empty(),
        "OIDC administrator credential unavailable"
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        let client = crate::http::client(Duration::from_secs(2))?;
        let origin = format!("http://127.0.0.1:{port}");
        let (status, token) = crate::http::json(
            client
                .post(format!(
                    "{origin}/realms/master/protocol/openid-connect/token"
                ))
                .form(&[
                    ("grant_type", "password"),
                    ("client_id", "admin-cli"),
                    ("username", "proofstorm-admin"),
                    ("password", password),
                ]),
        )
        .await?;
        ensure!(status.is_success(), "OIDC administrator login failed");
        let token = token["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .context("OIDC administrator token unavailable")?;
        let realm = format!("{origin}/admin/realms/proofstorm");
        if lifespan(&client, &realm, token).await? != lifetime {
            // Update only the authored scalar. Do not replace the realm, clients,
            // users, sessions or signing keys with the bootstrap import document.
            let response = client
                .put(&realm)
                .bearer_auth(token)
                .json(&json!({"accessTokenLifespan":lifetime}))
                .send()
                .await?;
            ensure!(response.status().is_success(), "OIDC policy update failed");
            ensure!(
                lifespan(&client, &realm, token).await? == lifetime,
                "OIDC policy update was not applied"
            );
        }
        Ok(())
    })
    .await
    .context("OIDC policy configuration deadline")?
}

async fn lifespan(client: &Client, realm: &str, token: &str) -> Result<u64> {
    let (status, value) = crate::http::json(client.get(realm).bearer_auth(token)).await?;
    ensure!(status.is_success(), "OIDC realm policy unavailable");
    ensure!(value["realm"] == "proofstorm", "unexpected OIDC realm");
    value["accessTokenLifespan"]
        .as_u64()
        .context("OIDC token lifetime unavailable")
}
