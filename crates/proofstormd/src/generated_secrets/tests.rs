use super::*;
use http::{Request, Response};
use k8s_openapi::ByteString;
use kube::{Client, client::Body};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    convert::Infallible,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Cluster {
    secret: Option<Secret>,
    creates: Vec<Secret>,
    reads: usize,
    read_error: Option<u16>,
    create_error: Option<u16>,
    /// Another reconciler creates this Secret between our read and create.
    concurrent: Option<Secret>,
}

fn api(cluster: &Arc<Mutex<Cluster>>) -> Api<Secret> {
    let cluster = cluster.clone();
    let client = Client::new(
        tower::service_fn(move |request: Request<Body>| {
            let cluster = cluster.clone();
            async move {
                let method = request.method().clone();
                assert_eq!(
                    request.uri().path(),
                    if method == http::Method::POST {
                        "/api/v1/namespaces/test/secrets"
                    } else {
                        "/api/v1/namespaces/test/secrets/credentials"
                    }
                );
                let bytes = request.into_body().collect_bytes().await.unwrap();
                let mut state = cluster.lock().unwrap();
                let result = match method {
                    http::Method::GET => {
                        state.reads += 1;
                        state
                            .read_error
                            .map_or_else(|| state.secret.clone().ok_or(404), Err)
                    }
                    http::Method::POST => {
                        let mut secret: Secret = serde_json::from_slice(&bytes).unwrap();
                        state.creates.push(secret.clone());
                        if let Some(code) = state.create_error {
                            Err(code)
                        } else if let Some(winner) = state.concurrent.take() {
                            state.secret = Some(winner);
                            Err(409)
                        } else {
                            for (key, value) in secret.string_data.take().unwrap() {
                                secret
                                    .data
                                    .get_or_insert_default()
                                    .insert(key, ByteString(value.into_bytes()));
                            }
                            state.secret = Some(secret.clone());
                            Ok(secret)
                        }
                    }
                    _ => panic!("unexpected method {method}"),
                };
                let (status, body) = match result {
                    Ok(secret) => (200, serde_json::to_value(secret).unwrap()),
                    Err(code) => (
                        code,
                        json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":if code == 404 { "NotFound" } else { "Injected" }, "message":"injected API error", "code":code}),
                    ),
                };
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        }),
        "test",
    );
    Api::namespaced(client, "test")
}

fn fixtures() -> Vec<(Secret, Vec<&'static str>)> {
    [
        (json!({"POSTGRES_USER":"proofstorm", "POSTGRES_DB":"mint"}), vec!["POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "DATABASE_URL", "database.toml"]),
        (json!({"PROOFSTORM_SECRET_KIND":"nutshell-mint"}), vec!["PROOFSTORM_SECRET_KIND", "MINT_PRIVATE_KEY"]),
        (json!({"PROOFSTORM_SECRET_KIND":"redis-cache"}), vec!["PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"]),
        (json!({"PROOFSTORM_SECRET_KIND":"keycloak-oidc", "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS":"300"}), vec!["PROOFSTORM_SECRET_KIND", "KEYCLOAK_ADMIN_PASSWORD", "OIDC_TEST_USERNAME", "OIDC_TEST_PASSWORD", "realm.json"]),
        (json!({"PROOFSTORM_SECRET_KIND":"cdk-mint", "bitcoin-rpc-password":"regtest"}), vec!["PROOFSTORM_SECRET_KIND", "mint-mnemonic", "wallet-mnemonic", "bitcoin-rpc-password"]),
    ].into_iter().map(|(data, required)| {
        let mut template: Secret = serde_json::from_value(json!({
            "apiVersion":"v1", "kind":"Secret", "type":"Opaque",
            "metadata":{"name":"credentials", "labels":{"proofstorm.dev/component":"backend"}, "annotations":{"keep":"yes"}},
            "stringData":data
        })).unwrap();
        template.string_data.as_mut().unwrap().insert("EXTRA".into(), "preserved".into());
        (template, required)
    }).collect()
}

fn is_random_hex(value: &str) {
    assert_eq!(value.len(), 64);
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

fn is_mnemonic(value: &str) {
    let mnemonic: bip39::Mnemonic = value.parse().unwrap();
    assert_eq!(mnemonic.word_count(), 12);
}

fn assert_generated_contract(data: &BTreeMap<String, String>) {
    match data.get("PROOFSTORM_SECRET_KIND").map(String::as_str) {
        None => {
            is_random_hex(&data["POSTGRES_PASSWORD"]);
            assert_eq!(
                data["DATABASE_URL"],
                format!(
                    "postgresql://proofstorm:{}@backend:5432/mint",
                    data["POSTGRES_PASSWORD"]
                )
            );
            assert_eq!(
                data["database.toml"],
                format!(
                    "\n[database]\nengine = \"postgres\"\n\n[database.postgres]\nurl = {:?}\ntls_mode = \"disable\"\nmax_connections = 20\nconnection_timeout_seconds = 10\n",
                    data["DATABASE_URL"]
                )
            );
        }
        Some("nutshell-mint") => is_random_hex(&data["MINT_PRIVATE_KEY"]),
        Some("cdk-mint") => {
            is_mnemonic(&data["mint-mnemonic"]);
            is_mnemonic(&data["wallet-mnemonic"]);
            assert_ne!(data["mint-mnemonic"], data["wallet-mnemonic"]);
        }
        Some("redis-cache") => {
            is_random_hex(&data["REDIS_PASSWORD"]);
            assert_eq!(
                data["REDIS_URL"],
                format!("redis://:{}@backend:6379/0", data["REDIS_PASSWORD"])
            );
        }
        Some("keycloak-oidc") => {
            is_random_hex(&data["KEYCLOAK_ADMIN_PASSWORD"]);
            is_random_hex(&data["OIDC_TEST_PASSWORD"]);
            assert_ne!(data["KEYCLOAK_ADMIN_PASSWORD"], data["OIDC_TEST_PASSWORD"]);
            assert_eq!(data["OIDC_TEST_USERNAME"], "proofstorm-user");
            let realm: Value = serde_json::from_str(&data["realm.json"]).unwrap();
            assert_eq!(realm["realm"], "proofstorm");
            assert_eq!(realm["accessTokenLifespan"], 300);
            assert_eq!(realm["clients"][0]["clientId"], "cashu-client");
            assert_eq!(realm["clients"][0]["publicClient"], true);
            assert_eq!(realm["clients"][0]["directAccessGrantsEnabled"], true);
            assert_eq!(realm["users"][0]["username"], data["OIDC_TEST_USERNAME"]);
            assert_eq!(
                realm["users"][0]["credentials"][0]["value"],
                data["OIDC_TEST_PASSWORD"]
            );
            assert_eq!(realm["users"][0]["credentials"][0]["temporary"], false);
        }
        kind => panic!("unexpected fixture kind {kind:?}"),
    }
}

#[tokio::test]
async fn generated_shapes_preserve_templates_and_reconnects_never_rotate_credentials() {
    for (mut template, required) in fixtures() {
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        ensure(&api(&cluster), &template).await.unwrap();
        let persisted = {
            let state = cluster.lock().unwrap();
            let written = &state.creates[0];
            assert_eq!(written.metadata, template.metadata);
            assert_eq!(written.type_, template.type_);
            let data = written.string_data.as_ref().unwrap();
            for key in required {
                assert!(data.contains_key(key), "{key}");
            }
            for (key, value) in template.string_data.as_ref().unwrap() {
                assert_eq!(&data[key], value);
            }
            assert_generated_contract(data);
            state.secret.clone().unwrap()
        };
        // A fresh client, with changed or invalid template settings, still keeps stored data.
        template.metadata.labels = None;
        let data = template.string_data.as_mut().unwrap();
        data.remove("POSTGRES_USER");
        data.insert(
            "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS".into(),
            "invalid".into(),
        );
        ensure(&api(&cluster), &template).await.unwrap();
        let state = cluster.lock().unwrap();
        assert_eq!(state.secret.as_ref(), Some(&persisted));
        assert_eq!(state.creates.len(), 1);
        assert_eq!(state.reads, 2);
    }
}

#[tokio::test]
async fn incomplete_existing_data_fails_without_writes_for_every_required_key() {
    for (template, required) in fixtures() {
        for missing in std::iter::once(None).chain(required.iter().copied().map(Some)) {
            let mut existing = template.clone();
            existing.string_data = None;
            existing.data = missing.map(|missing| {
                required
                    .iter()
                    .filter(|key| **key != missing)
                    .map(|key| ((*key).into(), ByteString(b"keep".to_vec())))
                    .collect()
            });
            let cluster = Arc::new(Mutex::new(Cluster {
                secret: Some(existing.clone()),
                ..Cluster::default()
            }));
            let error = ensure(&api(&cluster), &template).await.unwrap_err();
            let expected = missing.map_or_else(
                || "Secret \"credentials\" has no generated data".into(),
                |key| format!("Secret \"credentials\" is missing key {key:?}"),
            );
            assert!(matches!(error, Error::SecretContract(message) if message == expected));
            let state = cluster.lock().unwrap();
            assert_eq!(state.secret.as_ref(), Some(&existing));
            assert!(state.creates.is_empty());
        }
    }
}

#[tokio::test]
async fn existing_data_and_read_errors_never_invoke_the_generator() {
    for read_error in [None, Some(403), Some(500)] {
        let (template, _) = fixtures().remove(0);
        let existing = Secret {
            data: Some(BTreeMap::from([("key".into(), ByteString(vec![]))])),
            ..template.clone()
        };
        let cluster = Arc::new(Mutex::new(Cluster {
            secret: Some(existing),
            read_error,
            ..Cluster::default()
        }));
        let result = ensure_generated_secret(&api(&cluster), &template, &["key"], || {
            panic!("must not regenerate existing credentials")
        })
        .await;
        if let Some(expected) = read_error {
            assert!(
                matches!(result, Err(Error::Kube(kube::Error::Api(error))) if error.code == expected)
            );
        } else {
            result.unwrap();
        }
        assert!(cluster.lock().unwrap().creates.is_empty());
    }
}

#[tokio::test]
async fn generation_and_apply_errors_are_returned_without_retry_or_partial_writes() {
    let (template, _) = fixtures().remove(0);
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    let error = ensure_generated_secret(&api(&cluster), &template, &["key"], || {
        Err(Error::SecretContract("generation failed".into()))
    })
    .await
    .unwrap_err();
    assert!(matches!(error, Error::SecretContract(message) if message == "generation failed"));
    assert!(cluster.lock().unwrap().creates.is_empty());
    for code in [403, 500] {
        let cluster = Arc::new(Mutex::new(Cluster {
            create_error: Some(code),
            ..Cluster::default()
        }));
        let error = ensure(&api(&cluster), &template).await.unwrap_err();
        assert!(matches!(error, Error::Kube(kube::Error::Api(error)) if error.code == code));
        let state = cluster.lock().unwrap();
        assert_eq!(state.creates.len(), 1);
        assert_eq!(state.reads, 1);
        assert!(state.secret.is_none());
    }
}

#[tokio::test]
async fn invalid_templates_fail_before_writing() {
    for (index, field, message) in [
        (0, "POSTGRES_USER", "has no POSTGRES_USER"),
        (0, "POSTGRES_DB", "has no POSTGRES_DB"),
        (
            3,
            "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS",
            "has no OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS",
        ),
    ] {
        let (mut template, _) = fixtures().remove(index);
        template.string_data.as_mut().unwrap().remove(field);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        assert!(
            ensure(&api(&cluster), &template)
                .await
                .unwrap_err()
                .to_string()
                .contains(message)
        );
        assert!(cluster.lock().unwrap().creates.is_empty());
    }
    for index in [0, 2] {
        let (mut template, _) = fixtures().remove(index);
        template.metadata.labels = None;
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        assert!(
            ensure(&api(&cluster), &template)
                .await
                .unwrap_err()
                .to_string()
                .contains("has no component identity")
        );
        assert!(cluster.lock().unwrap().creates.is_empty());
    }
    let (mut template, _) = fixtures().remove(3);
    template.string_data.as_mut().unwrap().insert(
        "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS".into(),
        "invalid".into(),
    );
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    assert!(
        ensure(&api(&cluster), &template)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid OIDC token lifespan")
    );
    assert!(cluster.lock().unwrap().creates.is_empty());
}

#[tokio::test]
async fn every_cdk_mint_receives_its_own_seeds() {
    let (template, _) = fixtures().pop().unwrap();
    let mut seen = BTreeSet::new();
    for _ in 0..8 {
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        ensure(&api(&cluster), &template).await.unwrap();
        let state = cluster.lock().unwrap();
        let data = state.creates[0].string_data.as_ref().unwrap();
        assert!(seen.insert(data["mint-mnemonic"].clone()));
        assert!(seen.insert(data["wallet-mnemonic"].clone()));
    }
}

#[tokio::test]
async fn a_lost_create_race_keeps_the_winning_credentials() {
    for (template, required) in fixtures() {
        let winner = Secret {
            string_data: None,
            data: Some(
                required
                    .iter()
                    .map(|key| ((*key).into(), ByteString(b"winner".to_vec())))
                    .collect(),
            ),
            ..template.clone()
        };
        let cluster = Arc::new(Mutex::new(Cluster {
            concurrent: Some(winner.clone()),
            ..Cluster::default()
        }));
        ensure(&api(&cluster), &template).await.unwrap();
        let state = cluster.lock().unwrap();
        assert_eq!(state.secret.as_ref(), Some(&winner));
        assert_eq!(state.creates.len(), 1);
        assert_eq!(state.reads, 2);
    }
    let (template, _) = fixtures().pop().unwrap();
    let cluster = Arc::new(Mutex::new(Cluster {
        concurrent: Some(Secret {
            string_data: None,
            data: Some(BTreeMap::from([(
                "PROOFSTORM_SECRET_KIND".into(),
                ByteString(b"cdk-mint".to_vec()),
            )])),
            ..template.clone()
        }),
        ..Cluster::default()
    }));
    let error = ensure(&api(&cluster), &template).await.unwrap_err();
    assert!(matches!(error, Error::SecretContract(message) if message.contains("missing key")));
}
