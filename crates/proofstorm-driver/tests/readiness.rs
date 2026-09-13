#![cfg(feature = "runtime")]
use proofstorm_driver::http;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

#[test]
fn readiness_rejects_remote_transactional_and_credentialed_urls_before_io() {
    for url in [
        "http://mint:3338/v1/info",
        "https://127.0.0.1:3338/v1/info",
        "http://user:password@127.0.0.1:3338/v1/info",
        "http://127.0.0.1:3338/v1/mint/quote/bolt11",
        "http://127.0.0.1:3338/v1/info?redirect=elsewhere",
        "http://127.0.0.1:3338/v1/info#fragment",
    ] {
        assert!(http::local_info_url(url).is_err(), "accepted {url}");
    }
    assert!(http::local_info_url("http://127.0.0.1:3338/v1/info").is_ok());
}

#[tokio::test]
async fn readiness_ignores_ambient_proxy_and_does_not_read_the_body() {
    let mint = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/info", mint.local_addr().unwrap());
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_proofstorm-driver"))
            .args(["ready", "http", &url])
            .envs([
                ("http_proxy", &proxy_url),
                ("HTTP_PROXY", &proxy_url),
                ("all_proxy", &proxy_url),
                ("ALL_PROXY", &proxy_url),
            ])
            .env("no_proxy", "")
            .env("NO_PROXY", "")
            .output()
            .unwrap()
    });
    // First execution of a freshly linked binary can be delayed by host loader
    // checks. Start measuring the HTTP/body contract after it connects.
    let (mut stream, peer) = timeout(Duration::from_secs(120), mint.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(peer.ip().is_loopback());
    let mut request = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let count = timeout(Duration::from_secs(1), stream.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0 && request.len() + count < 8192);
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|value| value == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /v1/info http/1.1\r\n"));
    for header in [
        "authorization:",
        "blind-auth:",
        "clear-auth:",
        "x-forwarded-for:",
        "cf-connecting-ip:",
    ] {
        assert!(!request.contains(header));
    }
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\n\r\n")
        .await
        .unwrap();
    // Keep the stream open without sending a body. Readiness must finish now.
    let output = timeout(Duration::from_millis(750), output)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(
        timeout(Duration::from_millis(30), proxy.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn readiness_rejects_redirects_without_following_them() {
    let mint = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let transaction = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/info", mint.local_addr().unwrap());
    let location = format!(
        "http://{}/v1/mint/quote/bolt11",
        transaction.local_addr().unwrap()
    );
    let response = tokio::spawn(async move {
        let (mut stream, _) = mint.accept().await.unwrap();
        let mut request = [0; 8192];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
    });
    assert!(http::mint_ready(&url).await.is_err());
    response.await.unwrap();
    assert!(
        timeout(Duration::from_millis(30), transaction.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_http_rejects_oversized_json() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        let body = format!("\"{}\"", "x".repeat(http::MAX_BODY));
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
    });
    assert!(
        http::json(http::client(Duration::from_secs(1)).unwrap().get(url))
            .await
            .is_err()
    );
    server.await.unwrap();
}
