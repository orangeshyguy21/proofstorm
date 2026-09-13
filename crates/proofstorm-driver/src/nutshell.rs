//! Public protocol observations and allowlisted configuration evidence.
//! Non-public settings are read from the explicitly rendered environment, never an SDK.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

fn required<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    env.get(name)
        .map(String::as_str)
        .context("required mint configuration missing")
}
fn number(env: &BTreeMap<String, String>, name: &str) -> Result<u64> {
    Ok(required(env, name)?.parse()?)
}
fn boolean(env: &BTreeMap<String, String>, name: &str) -> Result<bool> {
    let value = required(env, name)?;
    ensure!(
        value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false"),
        "invalid mint boolean"
    );
    Ok(value.eq_ignore_ascii_case("true"))
}
/// Corroborate public settings against the live mint; project no credential values.
/// # Errors
/// Rejects unavailable/mismatched mint responses and missing explicit configuration.
pub async fn settings(mode: &str, env: &BTreeMap<String, String>) -> Result<Value> {
    let client = crate::http::client(Duration::from_secs(3))?;
    let (status, info) = crate::http::json(client.get("http://127.0.0.1:3338/v1/info")).await?;
    ensure!(status.is_success(), "mint info unavailable");
    let version = info["version"].as_str().context("mint version missing")?;
    let version = version.strip_prefix("Nutshell/").unwrap_or(version);
    ensure!(version == "0.20.3", "unsupported mint version");
    ensure!(
        info["name"] == required(env, "MINT_INFO_NAME")?,
        "public mint name differs"
    );
    match mode {
        "postgres-settings" => {
            let url = reqwest::Url::parse(required(env, "MINT_DATABASE")?)?;
            ensure!(
                matches!(url.scheme(), "postgres" | "postgresql"),
                "not PostgreSQL configuration"
            );
            Ok(
                json!({"version":version,"name":info["name"],"database_host":url.host_str().context("missing database host")?,
                "database_name":url.path().trim_start_matches('/'),"private_key_length":required(env,"MINT_PRIVATE_KEY")?.len()}),
            )
        }
        "redis-settings" => {
            let url = reqwest::Url::parse(required(env, "MINT_REDIS_CACHE_URL")?)?;
            ensure!(
                matches!(url.scheme(), "redis" | "rediss"),
                "not Redis configuration"
            );
            Ok(
                json!({"enabled":boolean(env,"MINT_REDIS_CACHE_ENABLED")?,"host":url.host_str().context("missing cache host")?,
                "password_length":url.password().context("missing cache password")?.len(),"ttl":number(env,"MINT_REDIS_CACHE_TTL")?,"cluster":boolean(env,"MINT_REDIS_CACHE_CLUSTER")?}),
            )
        }
        "settings" => {
            let (status, keysets) =
                crate::http::json(client.get("http://127.0.0.1:3338/v1/keysets")).await?;
            ensure!(status.is_success(), "mint keysets unavailable");
            let active: Vec<_> = keysets["keysets"]
                .as_array()
                .context("missing mint keysets")?
                .iter()
                .filter(|k| k["active"] == true && k["unit"] == "sat")
                .collect();
            let input_fee = number(env, "MINT_INPUT_FEE_PPK")?;
            ensure!(
                !active.is_empty()
                    && active
                        .iter()
                        .all(|k| k["input_fee_ppk"].as_u64() == Some(input_fee)),
                "public input fee differs"
            );
            ensure!(
                info["description"] == required(env, "MINT_INFO_DESCRIPTION")?,
                "public mint description differs"
            );
            for (nut, setting) in [
                ("4", "MINT_MAX_MINT_BOLT11_SAT"),
                ("5", "MINT_MAX_MELT_BOLT11_SAT"),
            ] {
                let methods = info["nuts"][nut]["methods"]
                    .as_array()
                    .context("mint method limits missing")?;
                let matching: Vec<_> = methods
                    .iter()
                    .filter(|m| m["method"] == "bolt11" && m["unit"] == "sat")
                    .collect();
                ensure!(
                    matching.len() == 1
                        && matching[0]["max_amount"].as_u64() == Some(number(env, setting)?),
                    "public mint amount limit differs"
                );
            }
            let percent: f64 = required(env, "LIGHTNING_FEE_PERCENT")?.parse()?;
            ensure!(percent.is_finite(), "invalid fee percentage");
            Ok(
                json!({"version":version,"name":info["name"],"description":info["description"],"input_fee_ppk":input_fee,
                "mint_quote_ttl":number(env,"MINT_QUOTE_TTL")?,"melt_quote_ttl":number(env,"MELT_QUOTE_TTL")?,
                "max_mint_sat":number(env,"MINT_MAX_MINT_BOLT11_SAT")?,"max_melt_sat":number(env,"MINT_MAX_MELT_BOLT11_SAT")?,
                "max_balance_sat":number(env,"MINT_MAX_BALANCE")?,"global_rate_limit":number(env,"MINT_GLOBAL_RATE_LIMIT_PER_MINUTE")?,
                "transaction_rate_limit":number(env,"MINT_TRANSACTION_RATE_LIMIT_PER_MINUTE")?,"lightning_fee_percent":percent,
                "lightning_reserve_fee_min":number(env,"LIGHTNING_RESERVE_FEE_MIN")?,"backend":required(env,"MINT_BACKEND_BOLT11_SAT")?,
                "lnd_endpoint":required(env,"MINT_LND_REST_ENDPOINT")?,"database":required(env,"MINT_DATABASE")?,"private_key_length":required(env,"MINT_PRIVATE_KEY")?.len()}),
            )
        }
        _ => anyhow::bail!("unsupported mint settings observation"),
    }
}

/// Check the persisted rune's allowed and forbidden CLN HTTP methods.
/// # Errors
/// Returns errors for missing credentials, transport failures or oversized input.
#[cfg(unix)]
pub async fn rune_probe() -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let path = std::path::Path::new("/app/data/.proofstorm/cln.rune");
    let mut bytes = String::new();
    std::fs::File::open(path)?
        .take(16_385)
        .read_to_string(&mut bytes)?;
    let rune = bytes.trim();
    ensure!(
        !rune.is_empty() && rune.len() <= 16_384,
        "invalid rune size"
    );
    let client = crate::http::client(Duration::from_secs(10))?;
    let allowed = client
        .post("http://mint-cln:3010/v1/listfunds")
        .header("rune", rune)
        .header("accept", "application/json")
        .send()
        .await?
        .status();
    let forbidden = client
        .post("http://mint-cln:3010/v1/withdraw")
        .header("rune", rune)
        .header("accept", "application/json")
        .form(&[("destination", "x"), ("satoshi", "all")])
        .send()
        .await?
        .status();
    Ok(
        json!({"length":rune.len(),"mode":format!("0o{:o}",std::fs::metadata(path)?.permissions().mode()&0o777),
        "digest":format!("{:x}",Sha256::digest(rune.as_bytes())),"allowed":allowed.as_u16(),"forbidden":forbidden.as_u16()}),
    )
}
