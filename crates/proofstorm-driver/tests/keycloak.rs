#![cfg(feature = "runtime")]
use proofstorm_driver::keycloak::configure;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

const LOGIN: &str = "/realms/master/protocol/openid-connect/token";
const REALM: &str = "/admin/realms/proofstorm";
const PASSWORD: &str = "private-password";

type Step = (&'static str, &'static str, u16, Value);

async fn server(steps: Vec<Step>) -> (u16, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut updates = Vec::new();
        for (method, path, status, body) in steps {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            let (headers, body_start, length) = loop {
                let mut bytes = [0; 1024];
                let count = stream.read(&mut bytes).await.unwrap();
                assert!(count > 0 && request.len() + count <= 8192);
                request.extend_from_slice(&bytes[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8(request[..end].to_vec()).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|n| n.parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break (headers, end + 4, length);
                }
            };
            while request.len() < body_start + length {
                let mut bytes = [0; 1024];
                let count = stream.read(&mut bytes).await.unwrap();
                assert!(count > 0 && request.len() + count <= 8192);
                request.extend_from_slice(&bytes[..count]);
            }
            assert_eq!(
                headers.lines().next().unwrap(),
                format!("{method} {path} HTTP/1.1")
            );
            if path == LOGIN {
                let form = String::from_utf8(request[body_start..].to_vec()).unwrap();
                for field in [
                    "grant_type=password",
                    "client_id=admin-cli",
                    "username=proofstorm-admin",
                    "password=private-password",
                ] {
                    assert!(form.split('&').any(|value| value == field));
                }
            } else {
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-token")
                );
            }
            if method == "PUT" {
                updates.push(serde_json::from_slice(&request[body_start..]).unwrap());
            }
            let body = if status == 204 {
                String::new()
            } else {
                body.to_string()
            };
            stream.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        updates
    });
    (port, task)
}

fn login() -> Step {
    ("POST", LOGIN, 200, json!({"access_token":"test-token"}))
}

fn policy(lifetime: u64) -> Step {
    (
        "GET",
        REALM,
        200,
        json!({"realm":"proofstorm", "accessTokenLifespan":lifetime,
        "users":[{"id":"preserve-user"}], "enabled":true}),
    )
}

#[tokio::test]
async fn lifetime_edits_update_only_policy_and_verify_the_result() {
    for (old, desired) in [(300, 600), (600, 60), (60, 3600)] {
        let (port, task) = server(vec![
            login(),
            policy(old),
            ("PUT", REALM, 204, Value::Null),
            policy(desired),
        ])
        .await;
        configure(port, desired, PASSWORD).await.unwrap();
        assert_eq!(
            task.await.unwrap(),
            vec![json!({"accessTokenLifespan":desired})]
        );
    }
}

#[tokio::test]
async fn matching_policy_and_retries_after_an_accepted_write_do_not_write_again() {
    let (port, task) = server(vec![login(), policy(600), login(), policy(600)]).await;
    configure(port, 600, PASSWORD).await.unwrap();
    configure(port, 600, PASSWORD).await.unwrap();
    assert!(task.await.unwrap().is_empty());
}

#[tokio::test]
async fn failed_or_unverified_configuration_never_reports_success() {
    let cases = vec![
        vec![("POST", LOGIN, 401, json!({"error":"private-response"}))],
        vec![("POST", LOGIN, 200, json!({}))],
        vec![login(), ("GET", REALM, 404, json!({}))],
        vec![
            login(),
            (
                "GET",
                REALM,
                200,
                json!({"realm":"other","accessTokenLifespan":600}),
            ),
        ],
        vec![login(), ("GET", REALM, 200, json!({"realm":"proofstorm"}))],
        vec![
            login(),
            policy(300),
            ("PUT", REALM, 403, json!({"error":"private-response"})),
        ],
        vec![
            login(),
            policy(300),
            ("PUT", REALM, 204, Value::Null),
            policy(300),
        ],
        vec![
            login(),
            policy(300),
            ("PUT", REALM, 204, Value::Null),
            ("GET", REALM, 503, json!({})),
        ],
    ];
    for steps in cases {
        let (port, task) = server(steps).await;
        let error = configure(port, 600, PASSWORD).await.unwrap_err();
        for secret in [PASSWORD, "test-token", "private-response"] {
            assert!(!format!("{error:#}").contains(secret));
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn invalid_policy_or_missing_credentials_is_rejected_before_io() {
    for (port, lifetime, password) in [
        (0, 600, PASSWORD),
        (1, 59, PASSWORD),
        (1, 3601, PASSWORD),
        (1, 600, ""),
    ] {
        assert!(configure(port, lifetime, password).await.is_err());
    }
}
