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
    claim_exists: bool,
    claim_race_secret: Option<Secret>,
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
                if request.uri().path()
                    == "/api/v1/namespaces/test/persistentvolumeclaims/data-backend-0"
                {
                    assert_eq!(method, http::Method::GET);
                    let mut state = cluster.lock().unwrap();
                    if let Some(winner) = state.claim_race_secret.take() {
                        state.secret = Some(winner);
                    }
                    let exists = state.claim_exists;
                    let (status, value) = if exists {
                        (
                            200,
                            json!({"apiVersion":"v1","kind":"PersistentVolumeClaim","metadata":{"name":"data-backend-0"}}),
                        )
                    } else {
                        (
                            404,
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"absent","code":404}),
                        )
                    };
                    return Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(Body::from(serde_json::to_vec(&value).unwrap()))
                            .unwrap(),
                    );
                }
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
        (json!({"POSTGRES_USER":"proofstorm", "POSTGRES_DB":"postgres"}), vec!["POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB"]),
        (json!({"PROOFSTORM_SECRET_KIND":"nutshell-mint"}), vec!["PROOFSTORM_SECRET_KIND", "MINT_PRIVATE_KEY"]),
        (json!({"PROOFSTORM_SECRET_KIND":"redis-cache"}), vec!["PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"]),
        (json!({"PROOFSTORM_SECRET_KIND":"keycloak-oidc", "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS":"300"}), vec!["PROOFSTORM_SECRET_KIND", "KEYCLOAK_ADMIN_PASSWORD", "OIDC_TEST_USERNAME", "OIDC_TEST_PASSWORD", "realm.json"]),
        (json!({"PROOFSTORM_SECRET_KIND":"cdk-mint", "bitcoin-rpc-password":"regtest"}), vec!["PROOFSTORM_SECRET_KIND", "mint-mnemonic", "ldk-mnemonic", "bdk-mnemonic", "bitcoin-rpc-password"]),
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
            // Linked components compose their own URLs; none is pre-built.
            assert!(!data.contains_key("DATABASE_URL"));
        }
        Some("nutshell-mint") => is_random_hex(&data["MINT_PRIVATE_KEY"]),
        Some("cdk-mint") => {
            for seed in ["mint-mnemonic", "ldk-mnemonic", "bdk-mnemonic"] {
                is_mnemonic(&data[seed]);
            }
            assert_ne!(data["mint-mnemonic"], data["ldk-mnemonic"]);
            assert_ne!(data["ldk-mnemonic"], data["bdk-mnemonic"]);
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
        for seed in ["mint-mnemonic", "ldk-mnemonic", "bdk-mnemonic"] {
            assert!(seen.insert(data[seed].clone()));
        }
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

async fn ensure_owned(cluster: &Arc<Mutex<Cluster>>, template: &Secret) -> Result<(), Error> {
    let secrets = api(cluster);
    let claims = Api::namespaced(secrets.into_client(), "test");
    ensure_owned_identity(&api(cluster), &claims, template).await
}

/// Each owned-storage identity kind, its error label and its generated keys.
const OWNED: &[(&str, &str, &[&str])] = &[
    ("bark-processor", "Bark", &["mnemonic"]),
    ("open-ssp", "Open SSP", &["mnemonic", "admin-token"]),
    ("spark-processor", "Spark processor", &["mnemonic"]),
];

fn owned_template(kind: &str) -> Secret {
    serde_json::from_value(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":"credentials","labels":{"proofstorm.dev/component":"backend"}},
        "stringData":{"PROOFSTORM_SECRET_KIND":kind}}))
    .unwrap()
}

#[tokio::test]
async fn owned_seeds_are_unique_preserved_and_never_recreated_over_storage() {
    for (kind, label, generated) in OWNED {
        let template = owned_template(kind);
        let mut seeds = BTreeSet::new();
        for _ in 0..3 {
            let cluster = Arc::new(Mutex::new(Cluster::default()));
            ensure_owned(&cluster, &template).await.unwrap();
            let original = cluster.lock().unwrap().secret.clone().unwrap();
            let data = original.data.as_ref().unwrap();
            let text = |key: &str| std::str::from_utf8(&data[key].0).unwrap().to_owned();
            assert_eq!(data.len(), generated.len() + 1);
            is_mnemonic(&text("mnemonic"));
            assert!(seeds.insert(text("mnemonic")));
            if generated.contains(&"admin-token") {
                is_random_hex(&text("admin-token"));
                assert!(seeds.insert(text("admin-token")));
            }
            cluster.lock().unwrap().claim_exists = true;
            ensure_owned(&cluster, &template).await.unwrap();
            assert_eq!(cluster.lock().unwrap().secret.as_ref(), Some(&original));
            assert_eq!(cluster.lock().unwrap().creates.len(), 1);
            cluster.lock().unwrap().secret = None;
            let error = ensure_owned(&cluster, &template).await.unwrap_err();
            assert!(
                matches!(error, Error::SecretContract(message) if message == format!(
                    "{label} identity Secret \"credentials\" is missing while owned storage exists; restore the identity instead of regenerating it"
                ))
            );
            assert_eq!(cluster.lock().unwrap().creates.len(), 1);
        }
    }
}

#[tokio::test]
async fn invalid_or_racing_owned_identity_is_never_overwritten() {
    for (kind, label, generated) in OWNED {
        let template = owned_template(kind);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        ensure_owned(&cluster, &template).await.unwrap();
        let original = cluster.lock().unwrap().secret.clone().unwrap();
        for missing in generated.iter().chain(&["PROOFSTORM_SECRET_KIND"]) {
            for invalid in [
                None,
                Some(b"".to_vec()),
                Some(b"private invalid words".to_vec()),
            ] {
                let mut stored = original.clone();
                if let Some(bytes) = invalid {
                    stored
                        .data
                        .as_mut()
                        .unwrap()
                        .insert((*missing).into(), ByteString(bytes));
                } else {
                    stored.data.as_mut().unwrap().remove(*missing);
                }
                for racing in [false, true] {
                    let state = if racing {
                        Cluster {
                            concurrent: Some(stored.clone()),
                            ..Cluster::default()
                        }
                    } else {
                        Cluster {
                            secret: Some(stored.clone()),
                            claim_exists: true,
                            ..Cluster::default()
                        }
                    };
                    let cluster = Arc::new(Mutex::new(state));
                    let error = ensure_owned(&cluster, &template).await.unwrap_err();
                    let message = format!("{error:#}");
                    assert!(!message.contains("private invalid words"));
                    assert!(
                        message.contains("is missing key")
                            || message.ends_with(&format!(
                                "{label} identity Secret has invalid data; refusing replacement"
                            ))
                    );
                    assert_eq!(cluster.lock().unwrap().secret.as_ref(), Some(&stored));
                    assert_eq!(cluster.lock().unwrap().creates.len(), usize::from(racing));
                }
            }
        }
        let cluster = Arc::new(Mutex::new(Cluster {
            concurrent: Some(original.clone()),
            ..Cluster::default()
        }));
        ensure_owned(&cluster, &template).await.unwrap();
        assert_eq!(cluster.lock().unwrap().secret.as_ref(), Some(&original));
    }
}

#[tokio::test]
async fn owned_identities_refuse_each_others_secrets() {
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    ensure_owned(&cluster, &owned_template("open-ssp"))
        .await
        .unwrap();
    for kind in ["bark-processor", "spark-processor"] {
        let error = ensure_owned(&cluster, &owned_template(kind))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("has invalid data"));
    }
    assert_eq!(cluster.lock().unwrap().creates.len(), 1);
    let error = ensure_owned(&cluster, &owned_template("cdk-mint"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not an owned-storage identity"));
}

#[tokio::test]
async fn owned_storage_guard_rechecks_concurrent_creation_and_cannot_be_bypassed() {
    for (kind, label, _) in OWNED {
        let template = owned_template(kind);
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        let error = ensure(&api(&cluster), &template).await.unwrap_err();
        assert!(
            matches!(error, Error::SecretContract(message) if message == format!(
                "{label} identity requires the owned-storage provisioning guard"
            ))
        );
        assert!(cluster.lock().unwrap().creates.is_empty());
        ensure_owned(&cluster, &template).await.unwrap();
        let original = cluster.lock().unwrap().secret.clone().unwrap();
        let race = Arc::new(Mutex::new(Cluster {
            claim_exists: true,
            claim_race_secret: Some(original.clone()),
            ..Cluster::default()
        }));
        ensure_owned(&race, &template).await.unwrap();
        let state = race.lock().unwrap();
        assert!(state.creates.is_empty());
        assert_eq!(state.secret.as_ref(), Some(&original));
    }
}

async fn ensure_bark_tls(cluster: &Arc<Mutex<Cluster>>, template: &Secret) -> Result<(), Error> {
    let claims = Api::namespaced(api(cluster).into_client(), "test");
    crate::management_tls::ensure_bark(&api(cluster), &claims, template).await
}

#[tokio::test]
async fn bark_tls_preserves_distinct_roles_and_refuses_lost_or_incomplete_identity() {
    let mut authorities = BTreeSet::new();
    for role in ["cln", "hold"] {
        let mut template = owned_template("bark-processor");
        template.string_data = Some(BTreeMap::from([
            ("PROOFSTORM_SECRET_KIND".into(), format!("bark-{role}-tls")),
            ("PROOFSTORM_TLS_SERVER_NAME".into(), "backend".into()),
        ]));
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        assert!(
            crate::management_tls::ensure(&api(&cluster), &template)
                .await
                .is_err()
        );
        ensure_bark_tls(&cluster, &template).await.unwrap();
        let original = cluster.lock().unwrap().secret.clone().unwrap();
        assert!(authorities.insert(original.data.as_ref().unwrap()["ca.pem"].0.clone()));
        cluster.lock().unwrap().claim_exists = true;
        cluster.lock().unwrap().reads = 0;
        ensure_bark_tls(&cluster, &template).await.unwrap();
        // Once an identity was observed, never enter the creation path again:
        // a deletion between reads must not permit regeneration over its PVC.
        assert_eq!(cluster.lock().unwrap().reads, 1);
        assert_eq!(cluster.lock().unwrap().secret.as_ref(), Some(&original));
        for key in [
            "ca.key",
            "client.key",
            "server.pem",
            "PROOFSTORM_TLS_SERVER_NAME",
        ] {
            let mut damaged = original.clone();
            damaged.data.as_mut().unwrap().remove(key);
            cluster.lock().unwrap().secret = Some(damaged.clone());
            assert!(ensure_bark_tls(&cluster, &template).await.is_err());
            assert_eq!(cluster.lock().unwrap().secret.as_ref(), Some(&damaged));
        }
        cluster.lock().unwrap().secret = None;
        assert!(
            ensure_bark_tls(&cluster, &template)
                .await
                .unwrap_err()
                .to_string()
                .contains("owned storage exists")
        );
        assert_eq!(cluster.lock().unwrap().creates.len(), 1);
        let race = Arc::new(Mutex::new(Cluster {
            concurrent: Some(original.clone()),
            ..Cluster::default()
        }));
        ensure_bark_tls(&race, &template).await.unwrap();
        assert_eq!(race.lock().unwrap().secret.as_ref(), Some(&original));
        let race = Arc::new(Mutex::new(Cluster {
            claim_exists: true,
            claim_race_secret: Some(original.clone()),
            ..Cluster::default()
        }));
        ensure_bark_tls(&race, &template).await.unwrap();
        assert_eq!(race.lock().unwrap().secret.as_ref(), Some(&original));
    }
}

fn operators_template(operators: &str, threshold: &str, service: &str) -> Secret {
    serde_json::from_value(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
    "metadata":{"name":"credentials","labels":{"proofstorm.dev/component":service}},
    "stringData":{
        "PROOFSTORM_SECRET_KIND":"spark-operators",
        "PROOFSTORM_SPARK_OPERATORS":operators,
        "PROOFSTORM_SPARK_THRESHOLD":threshold,
        "PROOFSTORM_SPARK_SERVICE":service
    }}))
    .unwrap()
}

const SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];
const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const AUTHORITY_KEY_ID: &[u8] = &[0x55, 0x1d, 0x23];
const EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const CLIENT_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];
const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// Splits one DER element off `input`: tag, whole encoding, contents, rest.
fn der(input: &[u8]) -> (u8, &[u8], &[u8], &[u8]) {
    let (length, header) = if input[1] < 0x80 {
        (usize::from(input[1]), 2)
    } else {
        let count = usize::from(input[1] & 0x7f);
        let length = input[2..2 + count]
            .iter()
            .fold(0, |length, byte| length << 8 | usize::from(*byte));
        (length, 2 + count)
    };
    let end = header + length;
    (input[0], &input[..end], &input[header..end], &input[end..])
}

fn der_children(mut input: &[u8]) -> Vec<(u8, &[u8], &[u8])> {
    let mut children = Vec::new();
    while !input.is_empty() {
        let (tag, whole, contents, rest) = der(input);
        children.push((tag, whole, contents));
        input = rest;
    }
    children
}

/// The X.509 fields these tests check, read without a parser dependency.
struct Certificate {
    issuer: Vec<u8>,
    subject: Vec<u8>,
    public_key: Vec<u8>,
    /// Extension OID contents to the DER inside its `extnValue`.
    extensions: BTreeMap<Vec<u8>, Vec<u8>>,
}

fn certificate(pem: &str) -> Certificate {
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    // `ByteString` decodes base64 for Kubernetes, and so for PEM bodies here.
    let bytes = serde_json::from_value::<ByteString>(json!(body)).unwrap().0;
    let (_, _, certificate, _) = der(&bytes);
    let (_, _, tbs, _) = der(certificate);
    let fields = der_children(tbs);
    assert_eq!(fields[0].0, 0xa0, "X.509 v3");
    let (_, _, extensions) = fields.iter().find(|(tag, ..)| *tag == 0xa3).unwrap();
    let (_, _, extensions, _) = der(extensions);
    Certificate {
        issuer: fields[3].1.to_vec(),
        subject: fields[5].1.to_vec(),
        public_key: fields[6].1.to_vec(),
        extensions: der_children(extensions)
            .into_iter()
            .map(|(_, _, extension)| {
                let parts = der_children(extension);
                (parts[0].2.to_vec(), parts[parts.len() - 1].2.to_vec())
            })
            .collect(),
    }
}

impl Certificate {
    /// The first key-usage byte; every later usage bit must be clear.
    fn key_usage(&self) -> u8 {
        let (tag, _, bits, _) = der(&self.extensions[KEY_USAGE]);
        assert_eq!(tag, 0x03);
        assert!(bits[2..].iter().all(|byte| *byte == 0));
        bits[1]
    }

    fn uses_p256(&self) -> bool {
        self.public_key
            .windows(P256.len())
            .any(|window| window == P256)
    }
}

fn assert_server_certificate(
    data: &BTreeMap<String, String>,
    index: usize,
    authority: &Certificate,
) {
    let server = certificate(&data[&format!("server-{index}.crt")]);
    // Chains to `ca.crt`: issued under its name and its key identifier.
    assert_eq!(server.issuer, authority.subject);
    let (_, _, authority_id, _) = der(&authority.extensions[SUBJECT_KEY_ID]);
    let (_, _, identifiers, _) = der(&server.extensions[AUTHORITY_KEY_ID]);
    assert_eq!(
        der_children(identifiers),
        vec![(0x80, identifiers, authority_id)]
    );
    let key = rcgen::KeyPair::from_pem(&data[&format!("server-{index}.key")]).unwrap();
    assert_eq!(key.public_key_der(), server.public_key);
    assert!(server.uses_p256());
    let (_, _, names, _) = der(&server.extensions[SUBJECT_ALT_NAME]);
    let names: Vec<(u8, Vec<u8>)> = der_children(names)
        .into_iter()
        .map(|(tag, _, name)| (tag, name.to_vec()))
        .collect();
    assert_eq!(
        names,
        [
            (0x82, format!("backend-{index}.backend").into_bytes()),
            (0x82, format!("backend-{index}").into_bytes()),
            (0x82, b"localhost".to_vec()),
            (0x87, vec![127, 0, 0, 1]),
        ]
    );
    let (_, _, usages, _) = der(&server.extensions[EXTENDED_KEY_USAGE]);
    let usages: Vec<(u8, &[u8])> = der_children(usages)
        .into_iter()
        .map(|(tag, _, usage)| (tag, usage))
        .collect();
    assert_eq!(usages, [(0x06, SERVER_AUTH), (0x06, CLIENT_AUTH)]);
    assert_eq!(server.key_usage(), 0x80, "digitalSignature only");
    assert_eq!(
        server.extensions[BASIC_CONSTRAINTS],
        [0x30, 0x03, 0x01, 0x01, 0x00]
    );
}

#[tokio::test]
async fn spark_operators_generate_every_identity_in_its_contract_format() {
    let template = operators_template("3", "2", "backend");
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    ensure(&api(&cluster), &template).await.unwrap();
    let data = cluster.lock().unwrap().creates[0]
        .string_data
        .clone()
        .unwrap();
    let mut keys: BTreeSet<String> = template.string_data.clone().unwrap().into_keys().collect();
    keys.extend(["ca.crt", "operators.json", "threshold"].map(String::from));
    let authority = certificate(&data["ca.crt"]);
    assert_eq!(authority.issuer, authority.subject);
    assert!(authority.uses_p256());
    assert_eq!(
        authority.extensions[BASIC_CONSTRAINTS],
        [0x30, 0x06, 0x01, 0x01, 0xff, 0x02, 0x01, 0x00]
    );
    assert_eq!(authority.key_usage(), 0x04, "keyCertSign only");
    let secp = secp256k1::Secp256k1::new();
    let mut entries = Vec::new();
    let mut distinct = BTreeSet::new();
    for index in 0..3 {
        let key = data[&format!("operator-{index}.key")]
            .strip_suffix('\n')
            .unwrap();
        let public = data[&format!("operator-{index}.pub")]
            .strip_suffix('\n')
            .unwrap();
        is_random_hex(key);
        assert_eq!(public.len(), 66);
        assert!(public.starts_with("02") || public.starts_with("03"));
        let secret: secp256k1::SecretKey = key.parse().unwrap();
        assert_eq!(secret.public_key(&secp).to_string(), public);
        assert_server_certificate(&data, index, &authority);
        for value in [key, public, data[&format!("server-{index}.key")].as_str()] {
            assert!(distinct.insert(value.to_owned()));
        }
        entries.push(format!(
            r#"{{"id":{index},"address":"backend-{index}.backend:8535","external_address":"backend-{index}.backend:8535","identity_public_key":"{public}","cert_path":"/spark-operators/ca.crt"}}"#
        ));
        keys.extend([
            format!("operator-{index}.key"),
            format!("operator-{index}.pub"),
            format!("server-{index}.crt"),
            format!("server-{index}.key"),
        ]);
    }
    assert_eq!(data["operators.json"], format!("[{}]", entries.join(",")));
    assert_eq!(data["threshold"], "2\n");
    // The CA signing key is discarded; nothing else is stored.
    assert_eq!(data.keys().cloned().collect::<BTreeSet<_>>(), keys);
    let persisted = cluster.lock().unwrap().secret.clone();
    ensure(&api(&cluster), &template).await.unwrap();
    let state = cluster.lock().unwrap();
    assert_eq!(state.secret, persisted);
    assert_eq!((state.creates.len(), state.reads), (1, 2));
}

#[tokio::test]
async fn spark_operator_federations_of_every_size_are_independent() {
    let mut authorities = BTreeSet::new();
    for (operators, threshold) in [("1", "1"), ("5", "3"), ("5", "5")] {
        let template = operators_template(operators, threshold, "backend");
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        ensure(&api(&cluster), &template).await.unwrap();
        let data = cluster.lock().unwrap().creates[0]
            .string_data
            .clone()
            .unwrap();
        let operators: usize = operators.parse().unwrap();
        assert_eq!(data.len(), 7 + 4 * operators);
        assert_eq!(data["threshold"], format!("{threshold}\n"));
        let entries: Vec<Value> = serde_json::from_str(&data["operators.json"]).unwrap();
        assert_eq!(entries.len(), operators);
        let authority = certificate(&data["ca.crt"]);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry["id"], index);
            assert_server_certificate(&data, index, &authority);
        }
        assert!(authorities.insert(data["ca.crt"].clone()));
    }
}

#[tokio::test]
async fn spark_operator_federation_is_fixed_at_creation() {
    let template = operators_template("3", "2", "backend");
    let cluster = Arc::new(Mutex::new(Cluster::default()));
    ensure(&api(&cluster), &template).await.unwrap();
    let original = cluster.lock().unwrap().secret.clone().unwrap();
    let refuses = async |template: &Secret, stored: &Secret, racing: bool, expected: &str| {
        let cluster = Arc::new(Mutex::new(Cluster {
            secret: (!racing).then(|| stored.clone()),
            concurrent: racing.then(|| stored.clone()),
            ..Cluster::default()
        }));
        let error = ensure(&api(&cluster), template).await.unwrap_err();
        assert!(
            matches!(&error, Error::SecretContract(message)
            if message.contains(expected)
                && message.contains("the federation's size, threshold and service are fixed at creation")),
            "{error}"
        );
        let state = cluster.lock().unwrap();
        assert_eq!(state.secret.as_ref(), Some(stored));
        assert_eq!(state.creates.len(), usize::from(racing));
    };
    for racing in [false, true] {
        for changed in [
            operators_template("4", "2", "backend"),
            operators_template("3", "3", "backend"),
            operators_template("3", "2", "other"),
        ] {
            refuses(&changed, &original, racing, "differs from its template").await;
        }
        for key in original.data.as_ref().unwrap().keys() {
            let setting = key.starts_with("PROOFSTORM_");
            let expected = if setting {
                "differs from its template".into()
            } else {
                format!("is missing key {key:?}")
            };
            for value in [None, Some(ByteString(vec![]))] {
                let mut stored = original.clone();
                let data = stored.data.as_mut().unwrap();
                match value {
                    Some(value) => data.insert(key.clone(), value),
                    None => data.remove(key),
                };
                refuses(&template, &stored, racing, &expected).await;
            }
        }
    }
    let race = Arc::new(Mutex::new(Cluster {
        concurrent: Some(original.clone()),
        ..Cluster::default()
    }));
    ensure(&api(&race), &template).await.unwrap();
    assert_eq!(race.lock().unwrap().secret.as_ref(), Some(&original));
}

#[tokio::test]
async fn invalid_spark_operator_templates_fail_before_any_api_call() {
    let mut unnamed = operators_template("3", "2", "backend");
    unnamed
        .string_data
        .as_mut()
        .unwrap()
        .remove("PROOFSTORM_SPARK_SERVICE");
    let mut relabelled = operators_template("3", "2", "backend");
    relabelled.metadata.labels = Some(BTreeMap::from([(
        "proofstorm.dev/component".into(),
        "other".into(),
    )]));
    for (template, expected) in [
        (
            operators_template("0", "1", "backend"),
            "needs 1 to 5 Spark operators",
        ),
        (
            operators_template("6", "1", "backend"),
            "needs 1 to 5 Spark operators",
        ),
        (
            operators_template("three", "1", "backend"),
            "needs 1 to 5 Spark operators",
        ),
        (
            operators_template("3", "0", "backend"),
            "needs a Spark threshold from 1 to 3",
        ),
        (
            operators_template("3", "4", "backend"),
            "needs a Spark threshold from 1 to 3",
        ),
        (
            operators_template("3", "-1", "backend"),
            "needs a Spark threshold from 1 to 3",
        ),
        (unnamed, "has no PROOFSTORM_SPARK_SERVICE"),
        (relabelled, "names a Spark service other than its component"),
    ] {
        let cluster = Arc::new(Mutex::new(Cluster::default()));
        let error = ensure(&api(&cluster), &template).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        let state = cluster.lock().unwrap();
        assert_eq!((state.reads, state.creates.len()), (0, 0));
    }
}
