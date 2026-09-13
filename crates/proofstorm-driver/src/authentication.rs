//! NUT-21/22 conformance and replay checks through native HTTP and Cashu crypto.
use crate::http;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cashu::{BlindSignature, BlindedMessage, Id, Keys, PublicKey, SecretKey, dhke, secret::Secret};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Config {
    pub mint: String,
    pub identity: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub source: Option<String>,
    pub spent_bat: Option<String>,
}
impl Config {
    /// Read secret inputs from environment, keeping them out of arguments/errors.
    /// # Errors
    /// Returns a fixed error for a missing required input.
    pub fn environment() -> Result<Self> {
        Ok(Self {
            mint: http::required("PROOFSTORM_MINT")?,
            identity: http::required("PROOFSTORM_IDENTITY_PROVIDER")?,
            url: http::required("PROOFSTORM_MINT_URL")?,
            username: http::required("OIDC_TEST_USERNAME")?,
            password: http::required("OIDC_TEST_PASSWORD")?,
            source: std::env::var("PROOFSTORM_SOURCE_OPERATION_ID").ok(),
            spent_bat: std::env::var("PROOFSTORM_SPENT_BAT").ok(),
        })
    }
}
struct Reply {
    status: StatusCode,
    value: Value,
}
impl Reply {
    fn ok(&self) -> bool {
        self.status.is_success()
    }
    fn code(&self) -> Option<i64> {
        self.value["code"].as_i64()
    }
}
async fn send(request: reqwest::RequestBuilder) -> Result<Reply> {
    let (status, value) = http::json(request).await?;
    Ok(Reply { status, value })
}
fn finding(result: &mut Value, stage: &str, reply: Option<&Reply>) -> Value {
    result["failure_stage"] = json!(stage);
    if let Some(reply) = reply {
        result["failure_status"] = json!(reply.status.as_u16());
        result["failure_protocol_code"] = json!(reply.code());
    }
    result.clone()
}
struct Session {
    client: Client,
    discovery: Value,
    cat: String,
}
impl Session {
    async fn discover(info: &Value) -> Result<Self> {
        let client = http::client(Duration::from_secs(30))?;
        let discovery_url = info["nuts"]["21"]["openid_discovery"]
            .as_str()
            .context("discovery missing")?;
        let response = send(client.get(discovery_url)).await?;
        ensure!(response.ok(), "discovery failed");
        let token = response.value["token_endpoint"]
            .as_str()
            .context("token endpoint missing")?;
        // Never send credentials to another origin supplied by discovery.
        ensure!(
            reqwest::Url::parse(discovery_url)?.origin() == reqwest::Url::parse(token)?.origin(),
            "token endpoint origin mismatch"
        );
        ensure!(
            info["nuts"]["21"]["client_id"] == "cashu-client",
            "client policy mismatch"
        );
        Ok(Self {
            client,
            discovery: response.value,
            cat: String::new(),
        })
    }
    async fn authenticate(&mut self, config: &Config) -> Result<()> {
        let login = self.login(config, &config.password).await?;
        ensure!(login.ok(), "login failed");
        self.cat = login.value["access_token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .context("access token missing")?
            .into();
        Ok(())
    }
    async fn login(&self, config: &Config, password: &str) -> Result<Reply> {
        send(
            self.client
                .post(
                    self.discovery["token_endpoint"]
                        .as_str()
                        .context("token endpoint missing")?,
                )
                .form(&[
                    ("grant_type", "password"),
                    ("client_id", "cashu-client"),
                    ("username", &config.username),
                    ("password", password),
                    ("scope", "openid"),
                ]),
        )
        .await
    }
    fn claims_match(&self) -> bool {
        // This checks advertised claims; the mint independently validates the CAT
        // signature when issuing cryptographically verified blind-auth proofs.
        let Some(payload) = self.cat.split('.').nth(1) else {
            return false;
        };
        let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')) else {
            return false;
        };
        let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        claims["sub"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
            && claims["iss"] == self.discovery["issuer"]
            && claims["azp"] == "cashu-client"
            && claims["iat"]
                .as_i64()
                .zip(claims["exp"].as_i64())
                .is_some_and(|(iat, exp)| exp.checked_sub(iat) == Some(600))
    }
    async fn mint(&self, config: &Config, outputs: &[Output]) -> Result<Reply> {
        send(
            self.client
                .post(format!("{}/v1/auth/blind/mint", config.url))
                .header("Clear-auth", &self.cat)
                .json(&json!({"outputs":outputs.iter().map(|o| &o.message).collect::<Vec<_>>()})),
        )
        .await
    }
}
struct AuthKeys {
    id: Id,
    keys: Keys,
    one: PublicKey,
}
impl AuthKeys {
    async fn load(client: &Client, config: &Config) -> Result<Self> {
        let response = send(client.get(format!("{}/v1/auth/blind/keys", config.url))).await?;
        ensure!(response.ok(), "auth keys unavailable");
        let keysets = response.value["keysets"]
            .as_array()
            .context("auth keysets unavailable")?;
        let active: Vec<_> = keysets
            .iter()
            .filter(|keyset| keyset["unit"] == "auth" && keyset["active"] == true)
            .collect();
        ensure!(active.len() == 1, "ambiguous auth keyset");
        let id = active[0]["id"]
            .as_str()
            .context("auth keyset ID missing")?
            .parse()?;
        let keys: Keys = serde_json::from_value(active[0]["keys"].clone())?;
        let one = keys
            .amount_key(1_u64.into())
            .context("auth denomination missing")?;
        Ok(Self { id, keys, one })
    }
}
struct Output {
    message: BlindedMessage,
    secret: Secret,
    r: SecretKey,
}
fn outputs(keys: &AuthKeys, count: usize) -> Result<Vec<Output>> {
    (0..count)
        .map(|_| {
            let secret = Secret::generate();
            let (point, r) = dhke::blind_message(secret.as_bytes(), None)?;
            Ok(Output {
                message: BlindedMessage::new(1_u64.into(), keys.id, point),
                secret,
                r,
            })
        })
        .collect()
}
fn tokens(keys: &AuthKeys, outputs: Vec<Output>, value: &Value) -> Result<Vec<String>> {
    let signatures: Vec<BlindSignature> = serde_json::from_value(value["signatures"].clone())?;
    ensure!(
        signatures.len() == outputs.len(),
        "auth signature count mismatch"
    );
    for (signature, output) in signatures.iter().zip(&outputs) {
        ensure!(
            signature.keyset_id == keys.id && signature.amount == 1_u64.into(),
            "auth signature identity mismatch"
        );
        signature.verify_dleq(keys.one, output.message.blinded_secret)?;
    }
    let (rs, secrets): (Vec<_>, Vec<_>) = outputs
        .into_iter()
        .map(|output| (output.r, output.secret))
        .unzip();
    let proofs = dhke::construct_proofs(signatures, rs, secrets, &keys.keys)?;
    proofs
        .into_iter()
        .map(|proof| {
            proof.verify_dleq(keys.one)?;
            // NUT-22 does not transmit DLEQ/blinding material to the mint on spend.
            let body = serde_json::to_vec(
                &json!({"id":proof.keyset_id,"secret":proof.secret,"C":proof.c}),
            )?;
            Ok(format!("authA{}", URL_SAFE_NO_PAD.encode(body)))
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "one ordered conformance transaction records each protocol boundary"
)]
async fn conformance(config: &Config) -> Result<Value> {
    let mut result = json!({"contract":"proofstorm/authentication-conformance/v1","mint":config.mint,"identity_provider":config.identity,
        "advertised_nut21":false,"advertised_nut22":false,"invalid_oidc_password_rejected":false,
        "missing_cat_rejected":false,"invalid_cat_code":null,"missing_bat_rejected":false,"invalid_bat_code":null,
        "oidc_login":false,"claims_match":false,"mint_accepted_cat":false,"bat_issued":false,"bat_dleq":false,
        "bat_max_code":null,"rate_limit_code":null,"conformant":false,"failure_stage":null,"failure_status":null,"failure_protocol_code":null});
    let client = http::client(Duration::from_secs(30))?;
    let info = send(client.get(format!("{}/v1/info", config.url))).await?;
    if !info.ok() {
        return Ok(finding(&mut result, "mint_info", Some(&info)));
    }
    result["advertised_nut21"] = json!(info.value["nuts"]["21"].is_object());
    result["advertised_nut22"] = json!(info.value["nuts"]["22"].is_object());
    if result["advertised_nut21"] != true || result["advertised_nut22"] != true {
        return Ok(finding(&mut result, "auth_advertisement", None));
    }
    if info.value["nuts"]["21"]["client_id"] != "cashu-client"
        || info.value["nuts"]["22"]["bat_max_mint"] != 3
    {
        return Ok(finding(&mut result, "auth_policy", None));
    }
    let Ok(mut session) = Session::discover(&info.value).await else {
        return Ok(finding(&mut result, "oidc_discovery", None));
    };
    let rejected = session.login(config, "not-the-generated-password").await?;
    result["invalid_oidc_password_rejected"] = json!(!rejected.ok());
    if rejected.ok() {
        return Ok(finding(&mut result, "invalid_oidc_password", None));
    }
    let quote = format!("{}/v1/mint/quote/bolt11", config.url);
    let auth = format!("{}/v1/auth/blind/mint", config.url);
    for (field, stage, url, payload, header, expected) in [
        (
            "missing_bat_rejected",
            "missing_bat",
            quote.as_str(),
            json!({"amount":1,"unit":"sat"}),
            None,
            None,
        ),
        (
            "invalid_bat_code",
            "invalid_bat",
            quote.as_str(),
            json!({"amount":1,"unit":"sat"}),
            Some(("Blind-auth", "authAinvalid")),
            Some(81002),
        ),
        (
            "missing_cat_rejected",
            "missing_cat",
            auth.as_str(),
            json!({"outputs":[]}),
            None,
            None,
        ),
        (
            "invalid_cat_code",
            "invalid_cat",
            auth.as_str(),
            json!({"outputs":[]}),
            Some(("Clear-auth", "not-a-jwt")),
            Some(80002),
        ),
    ] {
        let mut request = client.post(url).json(&payload);
        if let Some((key, value)) = header {
            request = request.header(key, value);
        }
        let reply = send(request).await?;
        result[field] = if expected.is_some() {
            json!(reply.code())
        } else {
            json!(!reply.ok())
        };
        if reply.ok() || expected.is_some_and(|code| reply.code() != Some(code)) {
            return Ok(finding(&mut result, stage, Some(&reply)));
        }
    }
    if session.authenticate(config).await.is_err() {
        return Ok(finding(&mut result, "oidc_login", None));
    }
    result["oidc_login"] = json!(true);
    result["claims_match"] = json!(session.claims_match());
    if result["claims_match"] != true {
        return Ok(finding(&mut result, "oidc_claims", None));
    }
    let keys = AuthKeys::load(&client, config).await?;
    let excessive = session.mint(config, &outputs(&keys, 4)?).await?;
    result["bat_max_code"] = json!(excessive.code());
    if excessive.ok() || excessive.code() != Some(81003) {
        return Ok(finding(&mut result, "bat_maximum", Some(&excessive)));
    }
    let pending = outputs(&keys, 1)?;
    let accepted = session.mint(config, &pending).await?;
    result["mint_accepted_cat"] =
        json!(!matches!(accepted.status.as_u16(), 401 | 403) && accepted.code() != Some(80002));
    if !accepted.ok() {
        return Ok(finding(&mut result, "bat_issuance", Some(&accepted)));
    }
    result["bat_issued"] = json!(
        accepted.value["signatures"]
            .as_array()
            .is_some_and(|signatures| signatures.len() == 1)
    );
    result["bat_dleq"] = json!(tokens(&keys, pending, &accepted.value).is_ok());
    if result["bat_issued"] != true || result["bat_dleq"] != true {
        return Ok(finding(&mut result, "bat_signature", None));
    }
    let limited = session.mint(config, &outputs(&keys, 1)?).await?;
    result["rate_limit_code"] = json!(limited.code());
    if limited.ok() || limited.code() != Some(81004) {
        return Ok(finding(&mut result, "cat_rate_limit", Some(&limited)));
    }
    result["conformant"] = json!(true);
    Ok(result)
}

async fn protected(config: &Config, is_replay: bool) -> Result<Value> {
    let mut result = if is_replay {
        json!({"contract":"proofstorm/authentication-replay/v1","mint":config.mint,"identity_provider":config.identity,
            "source_operation_id":config.source.as_ref().context("replay source missing")?,"spent_bat_replay_code":null,"fresh_bat_count":0,"fresh_bat_dleq":false})
    } else {
        json!({"contract":"proofstorm/authentication-protected-spend-private/v1","mint":config.mint,"identity_provider":config.identity,
            "bat_count":0,"bat_dleq":false,"spent_bat":null})
    };
    result.as_object_mut().context("invalid result")?.extend(json!({"protected_request":false,"conformant":false,"failure_stage":null,"failure_status":null,"failure_protocol_code":null}).as_object().context("invalid fields")?.clone());
    let client = http::client(Duration::from_secs(30))?;
    let quote = format!("{}/v1/mint/quote/bolt11", config.url);
    if is_replay {
        let reply = send(
            client
                .post(&quote)
                .json(&json!({"amount":1,"unit":"sat"}))
                .header(
                    "Blind-auth",
                    config.spent_bat.as_ref().context("spent BAT missing")?,
                ),
        )
        .await?;
        result["spent_bat_replay_code"] = json!(reply.code());
        if reply.ok() || reply.code() != Some(81002) {
            return Ok(finding(&mut result, "spent_bat_replay", Some(&reply)));
        }
    }
    let info = send(client.get(format!("{}/v1/info", config.url))).await?;
    if !info.ok() {
        return Ok(finding(&mut result, "mint_info", Some(&info)));
    }
    let Ok(mut session) = Session::discover(&info.value).await else {
        return Ok(finding(&mut result, "oidc_discovery", None));
    };
    if session.authenticate(config).await.is_err() {
        return Ok(finding(&mut result, "oidc_login", None));
    }
    let keys = AuthKeys::load(&client, config).await?;
    let pending = outputs(&keys, 3)?;
    let issued = session.mint(config, &pending).await?;
    let count = issued.value["signatures"].as_array().map_or(0, Vec::len);
    result[if is_replay {
        "fresh_bat_count"
    } else {
        "bat_count"
    }] = json!(count);
    if !issued.ok() || count != 3 {
        return Ok(finding(&mut result, "bat_issuance", Some(&issued)));
    }
    let Ok(proofs) = tokens(&keys, pending, &issued.value) else {
        return Ok(finding(&mut result, "bat_signature", None));
    };
    result[if is_replay {
        "fresh_bat_dleq"
    } else {
        "bat_dleq"
    }] = json!(true);
    let spent = &proofs[0];
    let reply = send(
        client
            .post(&quote)
            .json(&json!({"amount":1,"unit":"sat"}))
            .header("Blind-auth", spent),
    )
    .await?;
    result["protected_request"] = json!(
        reply.ok()
            && reply.value["quote"]
                .as_str()
                .is_some_and(|quote| !quote.is_empty())
    );
    if result["protected_request"] != true {
        return Ok(finding(&mut result, "protected_request", Some(&reply)));
    }
    if !is_replay {
        result["spent_bat"] = json!(spent);
    }
    result["conformant"] = json!(true);
    Ok(result)
}

/// Run one explicit authentication experiment. Protected-spend output is private.
/// # Errors
/// Returns transport/protocol errors for the caller to sanitize; never log secrets.
pub async fn run(mode: &str, config: &Config) -> Result<Value> {
    match mode {
        "conformance" => conformance(config).await,
        "protected-spend" => protected(config, false).await,
        "replay" => protected(config, true).await,
        _ => bail!("unsupported authentication mode"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn keyset() -> (AuthKeys, SecretKey) {
        let secret = SecretKey::generate();
        let keys = Keys::new(BTreeMap::from([(1_u64.into(), secret.public_key())]));
        (
            AuthKeys {
                id: "0011223344556677".parse().unwrap(),
                one: secret.public_key(),
                keys,
            },
            secret,
        )
    }
    fn issue(keys: &AuthKeys, secret: &SecretKey, outputs: &[Output]) -> Value {
        json!({"signatures":outputs.iter().map(|output| BlindSignature::new(1_u64.into(),dhke::sign_message(secret,&output.message.blinded_secret).unwrap(),keys.id,&output.message.blinded_secret,secret).unwrap()).collect::<Vec<_>>()})
    }
    #[test]
    fn blind_auth_verifies_crypto_and_never_transmits_dleq_or_blinding_material() {
        let (keys, secret) = keyset();
        let pending = outputs(&keys, 3).unwrap();
        let signatures = issue(&keys, &secret, &pending);
        let blinding: Vec<_> = pending
            .iter()
            .map(|output| format!("{:?}", output.r))
            .collect();
        let tokens = tokens(&keys, pending, &signatures).unwrap();
        assert_eq!(tokens.len(), 3);
        for token in tokens {
            let bytes = URL_SAFE_NO_PAD
                .decode(token.strip_prefix("authA").unwrap())
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 3);
            assert!(
                value.get("id").is_some()
                    && value.get("secret").is_some()
                    && value.get("C").is_some()
            );
            assert!(value.get("dleq").is_none());
            for private in &blinding {
                assert!(!value.to_string().contains(private));
            }
        }
    }
    #[test]
    fn auth_rejects_missing_dleq_wrong_identity_amount_and_reordered_signatures() {
        let (keys, secret) = keyset();
        for mode in ["dleq", "id", "amount", "order", "count"] {
            let pending = outputs(&keys, 3).unwrap();
            let mut signatures = issue(&keys, &secret, &pending);
            match mode {
                "dleq" => signatures["signatures"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("dleq"),
                "id" => {
                    signatures["signatures"][0]["id"] = json!("0088776655443322");
                    None
                }
                "amount" => {
                    signatures["signatures"][0]["amount"] = json!(2);
                    None
                }
                "order" => {
                    signatures["signatures"].as_array_mut().unwrap().swap(0, 1);
                    None
                }
                "count" => signatures["signatures"].as_array_mut().unwrap().pop(),
                _ => unreachable!(),
            };
            assert!(
                tokens(&keys, pending, &signatures).is_err(),
                "accepted {mode}"
            );
        }
    }
}
