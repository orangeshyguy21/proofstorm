use super::*;
use hickory_resolver::{
    config::{NameServerConfig, ResolverConfig, ResolverOpts},
    proto::{
        op::{Message, MessageType, ResponseCode},
        rr::{RData, Record, RecordType, rdata::A},
        xfer::Protocol,
    },
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UdpSocket,
};

struct Fixture {
    worker: Arc<Worker>,
    queries: Arc<AtomicUsize>,
    dns: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dns.abort();
    }
}

async fn fixture() -> Fixture {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = socket.local_addr().unwrap();
    let queries = Arc::new(AtomicUsize::new(0));
    let count = queries.clone();
    let dns = tokio::spawn(async move {
        let mut bytes = [0_u8; 4096];
        loop {
            let (len, peer) = socket.recv_from(&mut bytes).await.unwrap();
            let request = Message::from_vec(&bytes[..len]).unwrap();
            count.fetch_add(1, Ordering::Relaxed);
            let mut response = Message::new();
            response
                .set_id(request.id())
                .set_message_type(MessageType::Response)
                .set_recursion_desired(true)
                .set_recursion_available(true);
            for query in request.queries() {
                response.add_query(query.clone());
                if query.name().to_string().starts_with("missing.") {
                    response.set_response_code(ResponseCode::NXDomain);
                } else if query.query_type() == RecordType::A {
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        30,
                        RData::A(A(Ipv4Addr::LOCALHOST)),
                    ));
                }
            }
            if request
                .queries()
                .iter()
                .any(|query| query.name().to_string().starts_with("silent"))
            {
                continue;
            }
            socket
                .send_to(&response.to_vec().unwrap(), peer)
                .await
                .unwrap();
        }
    });
    let mut config = ResolverConfig::new();
    config.add_name_server(NameServerConfig::new(address, Protocol::Udp));
    let mut options = ResolverOpts::default();
    options.cache_size = 0;
    options.attempts = 1;
    options.timeout = Duration::from_millis(CHECK_TIMEOUT_MILLIS);
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    let resolver = TokioResolver::builder_with_config(
        config,
        hickory_resolver::name_server::TokioConnectionProvider::default(),
    )
    .with_options(options)
    .build();
    Fixture {
        worker: Worker::new("itest", "proofstorm-itest.svc.example.test.", resolver),
        queries,
        dns,
    }
}

fn request(component: &str, port: u16, path: Option<&str>) -> Request {
    let mut request = crate::tests::request();
    request.targets[0].component = component.into();
    request.targets[0].port = port;
    request.targets[0].http_path = path.map(str::to_owned);
    request
}

fn observations(response: Response) -> Vec<Observation> {
    match response {
        Response::Complete { observations, .. } => observations,
        Response::Rejected { code } => panic!("unexpected rejection: {code}"),
    }
}

#[tokio::test]
async fn tcp_resolves_service_dns_afresh_and_distinguishes_refusal_and_dns_failure() {
    let fixture = fixture().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    for _ in 0..2 {
        assert_eq!(
            observations(fixture.worker.evaluate(request("chain", port, None)).await)[0].outcome,
            Outcome::Reachable
        );
    }
    assert_eq!(
        fixture.queries.load(Ordering::Relaxed),
        2,
        "no cached DNS success"
    );
    drop(listener);
    assert_eq!(
        observations(fixture.worker.evaluate(request("chain", port, None)).await)[0].outcome,
        Outcome::ConnectionRefused
    );
    assert_eq!(
        observations(
            fixture
                .worker
                .evaluate(request("missing", port, None))
                .await
        )[0]
        .outcome,
        Outcome::DnsFailed
    );
}

#[tokio::test]
async fn tcp_probes_leave_a_shared_http_request_quota_untouched() {
    let fixture = fixture().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // One application request is available across all connections and clients.
    // Every TCP probe and both HTTP controls reach this same listener.
    let server = tokio::spawn(async move {
        let mut requests = 0;
        let mut empty_connections = 0;
        for _ in 0..130 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let size = timeout(Duration::from_secs(2), socket.read(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            if size == 0 {
                empty_connections += 1;
                continue;
            }
            requests += 1;
            let status = if requests == 1 { 200 } else { 429 };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        (requests, empty_connections)
    });
    for _ in 0..128 {
        let result = observations(fixture.worker.evaluate(request("mint", port, None)).await);
        assert_eq!(result[0].outcome, Outcome::Reachable);
    }
    // The first application request must retain the entire quota. The second
    // is a negative control proving that the fixture's limiter is enforced.
    let first = observations(
        fixture
            .worker
            .evaluate(request("mint", port, Some("/v1/info")))
            .await,
    );
    assert_eq!(first[0].http_status, Some(200));
    let second = observations(
        fixture
            .worker
            .evaluate(request("mint", port, Some("/v1/info")))
            .await,
    );
    assert_eq!(second[0].http_status, Some(429));
    assert_eq!(second[0].outcome, Outcome::HttpError);
    assert_eq!(
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap(),
        (2, 128)
    );
}

#[tokio::test]
async fn http_checks_status_without_following_redirects_or_reading_large_bodies() {
    let fixture = fixture().await;
    for (status, expected) in [
        (200, Outcome::Reachable),
        (404, Outcome::HttpError),
        (302, Outcome::HttpError),
    ] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let size = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("GET //some-path HTTP/1.1\r\n"));
            assert!(!request.to_lowercase().contains("authorization:"));
            assert!(request.contains("chain.proofstorm-itest.svc.example.test."));
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nLocation: http://outside.invalid/\r\nContent-Length: 1000000000\r\n\r\n").as_bytes()).await.unwrap();
            // An HTTP probe succeeds on headers even if this huge body never arrives.
            std::future::pending::<()>().await;
        });
        let response = timeout(
            Duration::from_secs(1),
            fixture
                .worker
                .evaluate(request("chain", port, Some("//some-path"))),
        )
        .await
        .unwrap();
        let observation = &observations(response)[0];
        assert_eq!(observation.outcome, expected);
        assert_eq!(observation.http_status, Some(status));
        handler.abort();
        let _ = handler.await;
    }
    assert_eq!(fixture.queries.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn overload_and_invalid_requests_do_no_network_work() {
    let fixture = fixture().await;
    let mut invalid = request("chain", 80, None);
    invalid.targets.push(crate::Target {
        component: "chain.other.svc".into(),
        ..invalid.targets[0].clone()
    });
    assert_eq!(
        fixture.worker.evaluate(invalid).await,
        Response::Rejected {
            code: "invalid_probe_target".into()
        }
    );
    let permit = fixture
        .worker
        .checks
        .acquire_many(u32::try_from(MAX_WORKER_CHECKS).unwrap())
        .await
        .unwrap();
    assert_eq!(
        fixture.worker.evaluate(request("chain", 80, None)).await,
        Response::Rejected {
            code: "worker_busy".into()
        }
    );
    assert_eq!(fixture.queries.load(Ordering::Relaxed), 0);
    drop(permit);
}

#[tokio::test]
async fn timeout_is_shared_by_a_batch_and_early_results_keep_their_age() {
    let fixture = fixture().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handler = tokio::spawn(async move {
        let mut connections = Vec::new();
        loop {
            connections.push(listener.accept().await.unwrap());
        }
    });
    let mut batch = request("fast", port, None);
    for index in 1..MAX_BATCH_TARGETS {
        batch.targets.push(crate::Target {
            component: format!("slow-{index}"),
            http_path: Some("/".into()),
            ..batch.targets[0].clone()
        });
    }
    let started = Instant::now();
    let result = observations(fixture.worker.evaluate(batch).await);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "32 checks must not become 32 serial timeouts"
    );
    assert_eq!(result.len(), MAX_BATCH_TARGETS);
    assert_eq!(result[0].outcome, Outcome::Reachable);
    assert!(result[0].age_millis >= 1500);
    assert!(result[1..].iter().all(|o| o.outcome == Outcome::TimedOut));
    assert_eq!(fixture.worker.checks.available_permits(), MAX_WORKER_CHECKS);
    handler.abort();
    let _ = handler.await;
}

#[tokio::test]
async fn completed_batches_release_connections_even_when_the_client_keeps_its_socket() {
    let fixture = fixture().await;
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = target.local_addr().unwrap().port();
    let accepting = tokio::spawn(async move {
        loop {
            let (connection, _) = target.accept().await.unwrap();
            drop(connection);
        }
    });
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receive) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(fixture.worker.clone().serve(listener, async {
        let _ = receive.await;
    }));
    let mut retained = Vec::new();
    for _ in 0..(super::MAX_CONNECTIONS * 3) {
        let mut connection = TcpStream::connect(address).await.unwrap();
        let batch = request("chain", port, None);
        assert!(!batch.keep_alive);
        transport::write(&mut connection, &batch).await.unwrap();
        let response: Response = timeout(Duration::from_secs(1), transport::read(&mut connection))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observations(response)[0].outcome, Outcome::Reachable);
        let mut byte = [0];
        assert_eq!(
            timeout(Duration::from_secs(1), connection.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        // Model a tunnel retaining its downstream socket after its user is done.
        retained.push(connection);
    }
    let mut persistent = TcpStream::connect(address).await.unwrap();
    let mut batch = request("chain", port, None);
    batch.keep_alive = true;
    for index in 0..3 {
        batch.keep_alive = index < 2;
        transport::write(&mut persistent, &batch).await.unwrap();
        let response: Response = timeout(Duration::from_secs(1), transport::read(&mut persistent))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observations(response)[0].outcome, Outcome::Reachable);
    }
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(1), persistent.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    shutdown.send(()).unwrap();
    serving.await.unwrap().unwrap();
    accepting.abort();
    assert!(accepting.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn shutdown_cancels_inflight_dns_checks_and_returns_every_permit() {
    let fixture = fixture().await;
    let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receive) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(fixture.worker.clone().serve(listener, async {
        let _ = receive.await;
    }));
    let mut connection = TcpStream::connect(address).await.unwrap();
    transport::write(&mut connection, &request("silent", 80, None))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), async {
        while fixture.queries.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    shutdown.send(()).unwrap();
    timeout(Duration::from_millis(500), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fixture.worker.checks.available_permits(), MAX_WORKER_CHECKS);
    let mut byte = [0];
    assert_eq!(connection.read(&mut byte).await.unwrap(), 0);
}

#[tokio::test]
async fn independent_batches_share_one_worker_limit_and_cancellation_releases_it() {
    let fixture = fixture().await;
    let mut tasks = JoinSet::new();
    for batch in 0..crate::MAX_BATCHES_PER_WORKER {
        let worker = fixture.worker.clone();
        let mut request = request("silent", 80, None);
        request.targets = (0..MAX_BATCH_TARGETS)
            .map(|index| crate::Target {
                component: format!("silent-{batch}-{index}"),
                ..request.targets[0].clone()
            })
            .collect();
        tasks.spawn(async move { worker.evaluate(request).await });
    }
    timeout(Duration::from_secs(1), async {
        while fixture.worker.checks.available_permits() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture.worker.evaluate(request("chain", 80, None)).await,
        Response::Rejected {
            code: "worker_busy".into()
        }
    );
    tasks.shutdown().await;
    assert_eq!(fixture.worker.checks.available_permits(), MAX_WORKER_CHECKS);
}

#[tokio::test]
async fn controller_disconnect_cancels_its_unresponsive_check_promptly() {
    let fixture = fixture().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receive) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(fixture.worker.clone().serve(listener, async {
        let _ = receive.await;
    }));
    let mut connection = TcpStream::connect(address).await.unwrap();
    transport::write(&mut connection, &request("silent", 80, None))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), async {
        while fixture.queries.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture.worker.checks.available_permits(),
        MAX_WORKER_CHECKS - 1
    );
    drop(connection);
    timeout(Duration::from_millis(500), async {
        while fixture.worker.checks.available_permits() != MAX_WORKER_CHECKS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    shutdown.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn http_connection_close_is_successful_and_oversized_headers_are_bounded() {
    let fixture = fixture().await;
    for oversized in [false, true] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let response = if oversized {
                format!("HTTP/1.1 200 OK\r\nX-Large: {}", "x".repeat(32 * 1024))
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            };
            let _ = socket.write_all(response.as_bytes()).await;
            if oversized {
                std::future::pending::<()>().await;
            }
        });
        let result = timeout(
            Duration::from_secs(1),
            fixture.worker.evaluate(request("chain", port, Some("/"))),
        )
        .await
        .unwrap();
        assert_eq!(
            observations(result)[0].outcome,
            if oversized {
                Outcome::TransportError
            } else {
                Outcome::Reachable
            }
        );
        handler.abort();
        let _ = handler.await;
    }
}
