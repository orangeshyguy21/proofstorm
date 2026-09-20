//! Explicit native read commands used by acceptance scenarios. These are not
//! controller actions: every read produces an ordinary native execution receipt.
use super::{json_content, quote, validate_retry};
use crate::{McpClient, cell, json as expect};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn wallet_request(implementation: &str, wallet: &str, mint: &str) -> Result<Value> {
    let url = format!("http://{mint}:3338");
    let script = match implementation {
        "cdk-cli-wallet" | "cocod-wallet" => {
            let database = if implementation == "cdk-cli-wallet" {
                "/wallet/cdk/cdk-cli.sqlite"
            } else {
                "/wallet/.cocod/coco.db"
            };
            format!(
                "exec env PROOFSTORM_DATABASE={} PROOFSTORM_WALLET={} PROOFSTORM_MINT={} PROOFSTORM_MINT_URL={} /opt/proofstorm/driver observe {}",
                quote(database),
                quote(wallet),
                quote(mint),
                quote(&url),
                quote(implementation)
            )
        }
        "nutshell-wallet" => format!(
            "exec /opt/proofstorm/driver holdings nutshell-wallet {}",
            quote(wallet)
        ),
        _ => bail!("unsupported acceptance observation: {implementation}"),
    };
    Ok(
        json!({"component":wallet,"script":script,"timeout_seconds":30,"output":{"mode":"public","fields":[]}}),
    )
}

pub fn observe_wallet(
    client: &mut McpClient,
    implementation: &str,
    scope: &Value,
) -> Result<Value> {
    let mut request = wallet_request(
        implementation,
        expect::string(scope, "/wallet")?,
        expect::string(scope, "/mint")?,
    )?;
    for field in ["name", "run_id", "request_id"] {
        if let Some(value) = scope.get(field) {
            request[field] = value.clone();
        }
    }
    let id = expect::string(&request, "/request_id")?.to_owned();
    let accepted = client.call("cell_exec", request.clone())?;
    let retried = client.call("cell_exec", request)?;
    validate_retry(&accepted, &retried)?;
    let receipt = cell::wait_succeeded(client, &id)?;
    let content = cell::artifact_content(&receipt)?;
    if implementation == "nutshell-wallet" {
        nutshell_balance(content, expect::string(scope, "/mint")?)
    } else {
        json_content(content)
    }
}

pub fn nutshell_balance(content: &Value, mint: &str) -> Result<Value> {
    let observed = json_content(content)?;
    let url = format!("http://{mint}:3338");
    let mut selected = None;
    for row in expect::array(&observed, "/mints")? {
        if expect::string(row, "/mint_url")? == url {
            anyhow::ensure!(selected.is_none(), "ambiguous mint in native holdings");
            selected = Some(row.clone());
        }
    }
    // An initialized database with no proofs has no holdings rows. A missing,
    // busy, or incompatible database fails the native command before this point.
    let balance = selected.unwrap_or_else(|| json!({"balance_sat":0,"reserved_sat":0}));
    expect::integer(&balance, "/balance_sat")?;
    expect::integer(&balance, "/reserved_sat")?;
    Ok(balance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holdings_select_exact_mint_and_distinguish_empty_from_unknown() {
        let receipt = |rows: Value| json!({"exit_code":0,"cleanup_verified":true,"streams_complete":true,"output_truncated":false,"stdout":json!({"mints":rows}).to_string()});
        let row = json!({"mint_url":"http://mint:3338","balance_sat":100,"reserved_sat":2});
        assert_eq!(
            nutshell_balance(&receipt(json!([row])), "mint").unwrap(),
            row
        );
        assert_eq!(
            nutshell_balance(&receipt(json!([row])), "other").unwrap()["balance_sat"],
            0
        );
        assert!(nutshell_balance(&receipt(json!([row, row])), "mint").is_err());
        assert!(nutshell_balance(&receipt(json!([{}])), "mint").is_err());
        assert!(nutshell_balance(&receipt(Value::Null), "mint").is_err());
        let mut failed = receipt(json!([]));
        failed["exit_code"] = json!(1);
        assert!(nutshell_balance(&failed, "mint").is_err());
    }
}
