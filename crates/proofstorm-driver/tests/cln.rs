#![cfg(feature = "runtime")]
#![cfg(unix)]
use proofstorm_driver::cln;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    time::timeout,
};

#[tokio::test]
async fn rune_creation_uses_restricted_rpc_and_reuses_private_state() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("rpc");
    let rune = directory.path().join("private/cln.rune");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 8192];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.ends_with(b"\n\n") {
                break;
            }
        }
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["method"], "createrune");
        assert_eq!(
            request["params"]["restrictions"],
            json!([[
                "method=listfunds",
                "method=invoice",
                "method=pay",
                "method=listinvoices",
                "method=listpays",
                "method=waitanyinvoice"
            ]])
        );
        stream.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"proofstorm-driver\",\"result\":{\"rune\":\"private-rune-canary\"}}\n\n").await.unwrap();
        listener
    });
    cln::mint_rune(&socket, &rune).await.unwrap();
    let listener = server.await.unwrap();
    assert_eq!(fs::read_to_string(&rune).unwrap(), "private-rune-canary");
    assert_eq!(
        fs::metadata(&rune).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(rune.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    cln::mint_rune(&socket, &rune).await.unwrap();
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rpc_rejects_wrong_identity_errors_partial_and_oversized_responses() {
    for response in [
        "{\"id\":\"other\",\"result\":{}}\n\n".into(),
        "{\"id\":\"proofstorm-driver\",\"error\":{\"message\":\"private-canary\"}}\n\n".into(),
        "{\"id\":\"proofstorm-driver\",\"result\":{}}".into(),
        "x".repeat(proofstorm_driver::http::MAX_BODY + 1),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("rpc");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            let _ = stream.write_all(response.as_bytes()).await;
        });
        let error = cln::rpc(&socket, "getinfo", json!({})).await.unwrap_err();
        assert!(!error.to_string().contains("private-canary"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn xpay_uses_separate_restricted_state_and_preserves_the_old_rune() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("rpc");
    let legacy = directory.path().join("cln.rune");
    fs::write(&legacy, "legacy-pay-rune").unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 8192];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.ends_with(b"\n\n") {
                break;
            }
        }
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["method"], "createrune");
        assert_eq!(
            request["params"]["restrictions"],
            json!([[
                "method=listfunds",
                "method=invoice",
                "method=xpay",
                "method=listinvoices",
                "method=listpays",
                "method=waitanyinvoice"
            ]])
        );
        stream
            .write_all(
                b"{\"id\":\"proofstorm-driver\",\"result\":{\"rune\":\"new-xpay-rune\"}}\n\n",
            )
            .await
            .unwrap();
        listener
    });
    cln::mint_rune_for_payment(&socket, &legacy, "xpay")
        .await
        .unwrap();
    let listener = server.await.unwrap();
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "legacy-pay-rune");
    let selected = directory.path().join("cln-xpay.rune");
    assert_eq!(fs::read_to_string(&selected).unwrap(), "new-xpay-rune");
    assert_eq!(
        fs::metadata(&selected).unwrap().permissions().mode() & 0o777,
        0o600
    );
    cln::mint_rune_for_payment(&socket, &legacy, "xpay")
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert!(
        cln::mint_rune_for_payment(&socket, &legacy, "withdraw")
            .await
            .is_err()
    );
}
