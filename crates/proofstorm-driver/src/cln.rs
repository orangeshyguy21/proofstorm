//! Core Lightning's native Unix JSON-RPC boundary.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{io::Write, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{sleep, timeout},
};

/// Read one bounded, terminated JSON-RPC response under an absolute deadline.
/// # Errors
/// Rejects unavailable sockets, timeout, oversize/partial frames and RPC errors.
pub async fn rpc(path: &Path, method: &str, params: Value) -> Result<Value> {
    timeout(Duration::from_secs(10), async {
        let stream = UnixStream::connect(path).await?;
        connected(stream, method, params).await
    })
    .await
    .context("RPC deadline")?
}

async fn connected(mut stream: UnixStream, method: &str, params: Value) -> Result<Value> {
    let mut request = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","id":"proofstorm-driver","method":method,"params":params}),
    )?;
    request.extend_from_slice(b"\n\n");
    stream.write_all(&request).await?;
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 8192];
        let count = stream.read(&mut chunk).await?;
        ensure!(count > 0, "incomplete RPC response");
        ensure!(
            count <= crate::http::MAX_BODY.saturating_sub(bytes.len()),
            "RPC response too large"
        );
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(2).position(|value| value == b"\n\n") {
            let value: Value = serde_json::from_slice(&bytes[..end])?;
            ensure!(
                value["id"] == "proofstorm-driver" && value.get("error").is_none(),
                "RPC rejected"
            );
            return value.get("result").cloned().context("RPC result missing");
        }
    }
}

/// Persist the restricted mint rune atomically; reuse it across ordinary restarts.
/// # Errors
/// Returns a sanitized-at-caller transport or private file error.
pub async fn mint_rune(socket: &Path, path: &Path) -> Result<()> {
    mint_rune_for_payment(socket, path, "pay").await
}

/// Create an exact payment-method credential in a separate persistent file.
/// The xpay contract never adopts or overwrites a legacy pay credential.
/// # Errors
/// Rejects unknown payment contracts and unavailable private state or RPC.
pub async fn mint_rune_for_payment(socket: &Path, legacy_path: &Path, payment: &str) -> Result<()> {
    ensure!(
        matches!(payment, "pay" | "xpay"),
        "unsupported payment contract"
    );
    let selected_path = if payment == "xpay" {
        legacy_path.with_file_name("cln-xpay.rune")
    } else {
        legacy_path.to_path_buf()
    };
    let path = selected_path.as_path();
    if std::fs::read_to_string(path).is_ok_and(|value| !value.trim().is_empty()) {
        return Ok(());
    }
    let directory = path.parent().context("rune directory missing")?;
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    // Retry connection establishment only: replaying createrune after losing its
    // response would create additional credentials with no way to recover them.
    let stream = timeout(Duration::from_secs(180), async {
        loop {
            match UnixStream::connect(socket).await {
                Ok(stream) => return stream,
                Err(_) => sleep(Duration::from_secs(1)).await,
            }
        }
    })
    .await
    .context("rune startup deadline")?;
    let result = timeout(Duration::from_secs(10), connected(stream, "createrune", json!({"restrictions":[[
        "method=listfunds","method=invoice",format!("method={payment}"),"method=listinvoices","method=listpays","method=waitanyinvoice"
    ]]}))).await.context("rune RPC deadline")??;
    let rune = result["rune"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("rune missing")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(rune.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
