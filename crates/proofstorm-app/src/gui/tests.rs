use super::*;

#[tokio::test]
async fn status_does_not_create_locks_or_remove_stale_records() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("home"), None, None).unwrap();
    let files = || {
        let mut names = std::fs::read_dir(&installation.home)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let before = files();
    assert_eq!(
        status(&installation.home).await.unwrap()["state"],
        "stopped"
    );
    assert_eq!(files(), before);
    let record = Record {
        format_version: 1,
        installation_id: installation.id.clone(),
        instance: "a".repeat(32),
        token: "b".repeat(64),
        executable: "/fixture/bin/proofstorm".into(),
        build_sha256: None,
        pid: 0,
        port: 0,
    };
    state::save(&installation.home.join(RECORD), &record).unwrap();
    let before = files();
    let result = status(&installation.home).await.unwrap();
    assert_eq!(result["state"], "unresponsive");
    assert!(!result.to_string().contains(&record.token));
    assert_eq!(files(), before);
    assert_eq!(
        state::record(&installation.home, &installation.id)
            .unwrap()
            .unwrap()
            .health(),
        record.health()
    );
}

#[test]
fn managed_environment_sets_operator_and_private_paths_explicitly() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("home"), None, None).unwrap();
    let environment = server::environment(&installation).unwrap();
    assert_eq!(environment.principal, "developer");
    assert_eq!(environment.workspace, crate::config::DEFAULT_WORKSPACE);
    assert_eq!(environment.mode, crate::config::Mode::Connected);
    assert_eq!(environment.database, installation.database());
    assert_eq!(environment.kubeconfig, Some(installation.kubeconfig()));
    assert_eq!(environment.context, installation.context());
}

#[test]
fn private_records_and_lifetime_leases_refuse_foreign_state() {
    let root = tempfile::tempdir().unwrap();
    let record = Record {
        format_version: 1,
        installation_id: "fixture".into(),
        instance: "a".repeat(32),
        token: "b".repeat(64),
        executable: "/fixture/bin/proofstorm".into(),
        build_sha256: None,
        pid: 1,
        port: 12345,
    };
    state::save(&root.path().join(RECORD), &record).unwrap();
    assert!(state::record(root.path(), "foreign").is_err());
    assert_eq!(
        state::record(root.path(), "fixture")
            .unwrap()
            .unwrap()
            .health(),
        record.health()
    );
    let held = state::lease(root.path(), "lease.sqlite3").unwrap();
    assert!(state::lease(root.path(), "lease.sqlite3").is_err());
    drop(held);
    assert!(state::lease(root.path(), "lease.sqlite3").is_ok());
    let link = root.path().join("linked.json");
    std::os::unix::fs::symlink(root.path().join(RECORD), &link).unwrap();
    assert!(state::read(&link).is_err());
    assert!(state::save(&link, &record).is_err());
    let mut other = record.clone();
    other.instance = "c".repeat(32);
    state::remove_owned(root.path(), &other).unwrap();
    assert!(root.path().join(RECORD).exists());
    state::remove_owned(root.path(), &record).unwrap();
    assert!(!root.path().join(RECORD).exists());
}

#[tokio::test]
async fn stop_confirms_owned_exit_when_the_last_http_response_is_lost() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(&root.path().join("home"), None, None).unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let record = Record {
        format_version: 1,
        installation_id: installation.id.clone(),
        instance: "a".repeat(32),
        token: "b".repeat(64),
        executable: "/fixture/bin/proofstorm".into(),
        build_sha256: None,
        pid: std::process::id(),
        port: listener.local_addr().unwrap().port(),
    };
    state::save(&installation.home.join(RECORD), &record).unwrap();
    let lifetime = state::lease(&installation.home, "gui-runtime-lock.sqlite3").unwrap();
    let task = tokio::spawn(async move {
        for path in ["/v1/gui/health", "/v1/gui/stop"] {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains(path));
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            if path.ends_with("health") {
                let body = record.health().to_string();
                reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            // A shutdown can close the socket without returning a response.
        }
        drop(lifetime);
    });
    let result = stop(&installation.home).await.unwrap();
    task.await.unwrap();
    assert_eq!(result["stopped"], true);
    assert_eq!(result["labs_stopped"], false);
    assert!(!installation.home.join(RECORD).exists());
}

#[test]
fn cookies_and_csrf_are_separate_from_cli_bearer_authority() {
    let mut headers = hyper::HeaderMap::new();
    assert_eq!(
        transport::authorized(&headers, "secret", "owned"),
        (false, false, false)
    );
    headers.insert("cookie", "unrelated=secret; owned=wrong".parse().unwrap());
    assert_eq!(
        transport::authorized(&headers, "secret", "owned"),
        (false, false, false)
    );
    headers.insert("cookie", "unrelated=other; owned=secret".parse().unwrap());
    assert_eq!(
        transport::authorized(&headers, "secret", "owned"),
        (false, true, false)
    );
    headers.insert("x-proofstorm-session", "secret".parse().unwrap());
    assert_eq!(
        transport::authorized(&headers, "secret", "owned"),
        (false, true, true)
    );
    headers.insert("authorization", "Bearer secret".parse().unwrap());
    assert_eq!(
        transport::authorized(&headers, "secret", "owned"),
        (true, true, true)
    );
}

async fn transport_fixture() -> (
    tempfile::TempDir,
    std::sync::Arc<Session>,
    tokio::task::JoinHandle<()>,
) {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let record = Record {
        format_version: 1,
        installation_id: "fixture".into(),
        instance: "a".repeat(32),
        token: "b".repeat(64),
        executable: "/fixture/bin/proofstorm".into(),
        build_sha256: None,
        pid: std::process::id(),
        port: listener.local_addr().unwrap().port(),
    };
    let session = std::sync::Arc::new(Session::new(
        record,
        root.path().into(),
        root.path().join("bundle"),
        true,
    ));
    let state = session.clone();
    let task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let state = state.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |mut request| {
                    let state = state.clone();
                    async move {
                        Ok::<_, std::convert::Infallible>(
                            transport::route(&mut request, state)
                                .await
                                .unwrap_or_else(|| {
                                    crate::http::json(
                                        hyper::StatusCode::OK,
                                        &json!({"read_only":true}),
                                    )
                                }),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    (root, session, task)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exercise the full browser authentication boundary with real HTTP requests"
)]
async fn transport_blocks_unauthenticated_cross_origin_and_untyped_writes() {
    let (root, session, task) = transport_fixture().await;
    let client = client().unwrap();
    let base = session.record.url();
    let origin = base.clone();
    let token = &session.record.token;
    assert_eq!(
        client
            .get(format!("{base}/v1/environment"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let health = client
        .get(format!("{base}/v1/gui/health"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        health.json::<Value>().await.unwrap(),
        session.record.health()
    );
    let exchange = client
        .post(format!("{base}/v1/gui/session"))
        .header("Origin", &origin)
        .header("X-Proofstorm-Session", token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(exchange.status(), 200);
    let cookie = exchange.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let cookie = cookie.split(';').next().unwrap();
    assert_eq!(
        client
            .get(format!("{base}/v1/environment"))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let command = || {
        client
            .post(format!("{base}/v1/gui/open"))
            .header("Cookie", cookie)
            .json(&json!({"project":root.path()}))
    };
    assert_eq!(
        command()
            .header("Origin", &origin)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        command()
            .header("X-Proofstorm-Session", token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        command()
            .header("Origin", "https://evil.invalid")
            .header("X-Proofstorm-Session", token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        command()
            .header("Origin", &origin)
            .header("X-Proofstorm-Session", token)
            .header("Host", "localhost:1")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        command()
            .header("Origin", &origin)
            .header("X-Proofstorm-Session", token)
            .header("Sec-Fetch-Site", "cross-site")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let browser = |path: &str| {
        client
            .post(format!("{base}{path}"))
            .header("Cookie", cookie)
            .header("Origin", &origin)
            .header("X-Proofstorm-Session", token)
    };
    // Native UI is user-triggered and shares the exact authentication boundary.
    // Invalid inputs must fail before opening any OS window in this test.
    assert_eq!(
        client
            .post(format!("{base}/v1/gui/pick-folder"))
            .json(&json!({"project":root.path()}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        browser("/v1/gui/pick-folder")
            .json(&json!({"project":root.path(),"script":"anything"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        browser("/v1/gui/pick-folder")
            .json(&json!({"project":"relative"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        browser("/v1/gui/open")
            .json(&json!({"project":root.path(),"command":"anything"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        browser("/v1/gui/open")
            .json(&json!({"project":"x".repeat(9000)}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(browser("/v1/gui/stop").send().await.unwrap().status(), 404);
    assert_eq!(
        browser("/v1/gui/open")
            .json(&json!({"project":root.path(),"replace_connection":true}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        browser("/v1/gui/plan")
            .json(&json!({"project":root.path(),"harness":"arbitrary-command"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    for harness in ["opencode", "claude", "claude-code"] {
        assert_eq!(
            browser("/v1/gui/plan")
                .json(&json!({"project":root.path(),"harness":harness}))
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
    }
    assert_eq!(
        browser("/v1/gui/plan")
            .json(&json!({"project":root.path()}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert!(!root.path().join(".codex").exists() && !root.path().join("attachments").exists());
    let held = session.actions.clone().acquire_owned().await.unwrap();
    assert_eq!(
        client
            .post(format!("{base}/v1/gui/stop"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    drop(held);
    assert_eq!(
        client
            .post(format!("{base}/v1/gui/stop"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    tokio::time::timeout(Duration::from_millis(100), session.shutdown.notified())
        .await
        .unwrap();
    task.abort();
}

#[tokio::test]
async fn browser_activation_requires_a_fresh_acknowledgement() {
    let (_root, session, task) = transport_fixture().await;
    let client = client().unwrap();
    let base = session.record.url();
    let token = session.record.token.clone();
    let request = client
        .post(format!("{base}/v1/gui/activate"))
        .bearer_auth(&token)
        .json(&json!({"project":"/project with spaces"}));
    let activating =
        tokio::spawn(async move { request.send().await.unwrap().json::<Value>().await.unwrap() });
    for _ in 0..100 {
        if session.activation.lock().unwrap().generation > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let generation = session.activation.lock().unwrap().generation;
    assert!(generation > 0);
    let ack = client
        .post(format!("{base}/v1/gui/focus-ack"))
        .bearer_auth(token)
        .json(&json!({"generation":generation}))
        .send()
        .await
        .unwrap();
    assert_eq!(ack.status(), 200);
    assert_eq!(activating.await.unwrap()["focused"], true);
    assert_eq!(session.activate("/next project").await["focused"], false);
    task.abort();
}
#[test]
fn startup_progress_only_relays_complete_known_status_lines() {
    assert_eq!(
        super::startup_progress(b"proofstorm-gui-startup:Checking controller health\n"),
        Some("Checking controller health")
    );
    assert_eq!(
        super::startup_progress(b"proofstorm-gui-startup:Checking controller health"),
        None
    );
    assert_eq!(
        super::startup_progress(
            b"private error details\nproofstorm-gui-startup:untrusted message\n"
        ),
        None
    );
    assert_eq!(super::startup_progress(b"proofstorm-gui-startup:Verifying GUI server files\nproofstorm-gui-startup:Checking runtime ownership\n"),
        Some("Checking runtime ownership"));
}
