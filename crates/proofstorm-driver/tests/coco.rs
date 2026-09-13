#![cfg(feature = "runtime")]
#![cfg(unix)]

use proofstorm_driver::coco::Coco;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Step {
    method: &'static str,
    path: &'static str,
    auth: bool,
    status: u16,
    reply: Value,
}
fn step(method: &'static str, path: &'static str, reply: Value) -> Step {
    Step {
        method,
        path,
        auth: true,
        status: 200,
        reply,
    }
}
struct Fixture {
    root: TempDir,
    coco: Coco,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(steps: Vec<Step>) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("credentials/current")).unwrap();
        let key = root.path().join("credentials/current/client");
        fs::write(&key, "private-credential").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            root.path().join("config.json"),
            r#"{"encrypted":true,"preserve":"configured-value"}"#,
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let path = root.path().to_owned();
        let server = tokio::spawn(async move {
            for expected in steps {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let (head, body) = loop {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert!(count > 0 && bytes.len() < 20_000);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let head = String::from_utf8(bytes[..end].to_vec())
                            .unwrap()
                            .to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .map_or(0, |v| v.parse::<usize>().unwrap());
                        if bytes.len() >= end + 4 + length {
                            break (head, bytes[end + 4..].to_vec());
                        }
                    }
                };
                assert!(head.starts_with(&format!(
                    "{} {} http/1.1",
                    expected.method.to_ascii_lowercase(),
                    expected.path
                )));
                assert_eq!(
                    head.contains("authorization: bearer private-credential"),
                    expected.auth
                );
                if expected.method == "POST" {
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(
                        body["passphrase"],
                        fs::read_to_string(path.join("passphrase")).unwrap()
                    );
                }
                let body = expected.reply.to_string();
                stream.write_all(format!("HTTP/1.1 {} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",expected.status,body.len()).as_bytes()).await.unwrap();
            }
        });
        let coco = Coco::new(root.path(), &root.path().join("passphrase"), &base).unwrap();
        Self { root, coco, server }
    }
    async fn finish(mut self) {
        (&mut self.server).await.unwrap();
    }
}

#[tokio::test]
async fn coco_initialization_start_and_identity_preserve_private_material() {
    let mnemonic = "private recovery words";
    let fixture = Fixture::new(vec![
        step(
            "POST",
            "/v1/admin/wallet/initialize",
            json!({"generatedMnemonic":mnemonic,"status":{"cocoSession":{"state":"stopped"}}}),
        ),
        step(
            "POST",
            "/v1/admin/session/start",
            json!({"cocoSession":{"state":"starting"}}),
        ),
        step(
            "GET",
            "/v1/status",
            json!({"cocoSession":{"state":"running"}}),
        ),
        step(
            "POST",
            "/v1/admin/wallet/recovery-material",
            json!({"mnemonic":mnemonic}),
        ),
    ])
    .await;
    let initialized = fixture
        .coco
        .run("initialize", &["http://mint:3338".into()])
        .await
        .unwrap();
    assert_eq!(initialized, json!({"initialized":true}));
    let passphrase = fs::read_to_string(fixture.root.path().join("passphrase")).unwrap();
    assert_eq!(passphrase.len(), 64);
    for name in ["passphrase", "config.json"] {
        assert_eq!(
            fs::metadata(fixture.root.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let config: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        json!({"encrypted":true,"preserve":"configured-value","mintUrl":"http://mint:3338"})
    );
    assert!(
        fixture
            .coco
            .run("initialize", &["http://different-mint:3338".into()])
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("passphrase")).unwrap(),
        passphrase
    );
    let start = fixture.coco.run("start", &[]).await.unwrap();
    assert_eq!(start["session"], "running");
    let identity = fixture.coco.run("identity", &[]).await.unwrap();
    assert_eq!(
        identity,
        format!("{:x}", Sha256::digest(mnemonic.as_bytes()))
    );
    for result in [initialized, start, identity] {
        for secret in [mnemonic, passphrase.as_str(), "private-credential"] {
            assert!(!result.to_string().contains(secret));
        }
    }
    fixture.finish().await;
}

#[tokio::test]
async fn coco_checks_the_unauthenticated_boundary_and_independent_balance() {
    let mut health = step("GET", "/health", json!({"status":"ok"}));
    health.auth = false;
    let mut unauthenticated = step("GET", "/v1/status", json!({"error":"unauthorized"}));
    unauthenticated.auth = false;
    unauthenticated.status = 401;
    let fixture = Fixture::new(vec![
        health,
        step(
            "GET",
            "/v1/status",
            json!({"wallet":null,"cocoSession":{"state":"stopped"}}),
        ),
        unauthenticated,
        step(
            "GET",
            "/v1/status",
            json!({"seedAccess":{"state":"locked"},"cocoSession":{"state":"stopped"}}),
        ),
        step(
            "GET",
            "/balance",
            json!({"output":{"http://mint:3338":{"sats":5000}}}),
        ),
        step(
            "GET",
            "/balance",
            json!({"output":{"http://mint:3338":{"sats":4999}}}),
        ),
    ])
    .await;
    assert_eq!(
        fixture.coco.run("uninitialized", &[]).await.unwrap()["unauthenticated_status"],
        401
    );
    assert_eq!(
        fixture.coco.run("locked", &[]).await.unwrap()["locked"],
        true
    );
    let args = ["http://mint:3338".into(), "5000".into()];
    assert_eq!(
        fixture.coco.run("balance", &args).await.unwrap()["native_ready_total_sat"],
        5000
    );
    assert!(fixture.coco.run("balance", &args).await.is_err());
    fixture.finish().await;
}

#[tokio::test]
async fn coco_rejects_api_errors_and_failed_sessions() {
    let fixture = Fixture::new(vec![
        step(
            "GET",
            "/balance",
            json!({"error":"private server details","output":{"http://mint:3338":{"sats":5000}}}),
        ),
        step(
            "GET",
            "/v1/status",
            json!({"cocoSession":{"state":"failed"}}),
        ),
    ])
    .await;
    let error = fixture
        .coco
        .run("balance", &["http://mint:3338".into(), "5000".into()])
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private server details"));
    assert!(
        fixture
            .coco
            .run("session", &["running".into()])
            .await
            .is_err()
    );
    fixture.finish().await;
}
