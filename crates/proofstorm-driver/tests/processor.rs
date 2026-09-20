#![cfg(feature = "runtime")]
use proofstorm_driver::processor::{Bolt11Settings, Bolt12Settings, Empty, Settings, settings};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use std::{
    convert::Infallible,
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request, Response, Status,
    codegen::{Body, BoxFuture, Service, StdError, http},
    transport::{Certificate, Identity, Server, ServerTlsConfig},
};

struct Credentials {
    directory: TempDir,
}
impl Credentials {
    fn new(server_name: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
        ];
        let ca_key = KeyPair::generate().unwrap();
        let ca = params.self_signed(&ca_key).unwrap();
        fs::write(directory.path().join("ca.pem"), ca.pem()).unwrap();
        for (role, names, usage) in [
            (
                "server",
                vec![server_name.to_owned()],
                ExtendedKeyUsagePurpose::ServerAuth,
            ),
            ("client", vec![], ExtendedKeyUsagePurpose::ClientAuth),
        ] {
            let mut params = CertificateParams::new(names).unwrap();
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![usage];
            let key = KeyPair::generate().unwrap();
            let certificate = params.signed_by(&key, &ca, &ca_key).unwrap();
            fs::write(
                directory.path().join(format!("{role}.pem")),
                certificate.pem(),
            )
            .unwrap();
            fs::write(
                directory.path().join(format!("{role}.key")),
                key.serialize_pem(),
            )
            .unwrap();
        }
        Self { directory }
    }
    fn server(&self) -> ServerTlsConfig {
        let path = self.directory.path();
        ServerTlsConfig::new()
            .identity(Identity::from_pem(
                fs::read(path.join("server.pem")).unwrap(),
                fs::read(path.join("server.key")).unwrap(),
            ))
            .client_ca_root(Certificate::from_pem(
                fs::read(path.join("ca.pem")).unwrap(),
            ))
    }
}

#[derive(Clone)]
struct Mint {
    calls: Arc<AtomicUsize>,
    mode: &'static str,
}
impl tonic::server::NamedService for Mint {
    const NAME: &'static str = "cdk_payment_processor.CdkPaymentProcessor";
}
impl tonic::server::UnaryService<Empty> for Mint {
    type Response = Settings;
    type Future = BoxFuture<Response<Settings>, Status>;
    fn call(&mut self, request: Request<Empty>) -> Self::Future {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // The server must authenticate a client certificate before dispatching RPCs.
        assert!(!request.peer_certs().unwrap().is_empty());
        assert_eq!(
            request.metadata().get("x-cdk-protocol-version").unwrap(),
            "4.0.0"
        );
        let mode = self.mode;
        Box::pin(async move {
            if mode == "stall" {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            if mode == "reject" {
                return Err(Status::unavailable("backend unavailable"));
            }
            Ok(Response::new(Settings {
                unit: if mode == "oversize" {
                    "x".repeat(65_536)
                } else if mode == "wrong-unit" {
                    "usd".into()
                } else {
                    "msat".into()
                },
                bolt11: Some(Bolt11Settings {
                    mpp: false,
                    amountless: true,
                    invoice_description: true,
                }),
                bolt12: (mode != "missing-method").then_some(Bolt12Settings {
                    amountless: true,
                    invoice_description: true,
                }),
            }))
        })
    }
}
impl<B> Service<http::Request<B>> for Mint
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        assert_eq!(
            request.uri().path(),
            "/cdk_payment_processor.CdkPaymentProcessor/GetSettings"
        );
        let service = self.clone();
        Box::pin(async move {
            let codec = tonic_prost::ProstCodec::<Settings, Empty>::default();
            Ok(tonic::server::Grpc::new(codec)
                .unary(service, request)
                .await)
        })
    }
}

struct Running {
    address: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Running {
    async fn new(credentials: &Credentials, mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("https://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let mint = Mint {
            calls: calls.clone(),
            mode,
        };
        let (stop, stopped) = oneshot::channel();
        let builder = Server::builder().tls_config(credentials.server()).unwrap();
        let task = tokio::spawn(async move {
            builder
                .clone()
                .add_service(mint)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        Self {
            address,
            calls,
            task,
            stop: Some(stop),
        }
    }
    async fn finish(mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn processor_handshake_authenticates_both_peers_and_sends_the_protocol_version() {
    let credentials = Credentials::new("127.0.0.1");
    let server = Running::new(&credentials, "ok").await;
    let path = credentials.directory.path();
    let response = settings(&server.address, path).await.unwrap();
    assert_eq!(response.unit, "msat");
    assert!(response.bolt11.unwrap().amountless);
    assert!(response.bolt12.is_some());
    assert!(
        settings(&server.address.replacen("https://", "http://", 1), path)
            .await
            .is_err()
    );
    let unrelated = Credentials::new("127.0.0.1");
    fs::copy(
        path.join("ca.pem"),
        unrelated.directory.path().join("ca.pem"),
    )
    .unwrap();
    assert!(
        settings(&server.address, unrelated.directory.path())
            .await
            .is_err()
    );
    fs::copy(path.join("server.pem"), path.join("client.pem")).unwrap();
    fs::copy(path.join("server.key"), path.join("client.key")).unwrap();
    assert!(settings(&server.address, path).await.is_err());
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    server.finish().await;
}

#[tokio::test]
async fn processor_checks_server_identity_and_rpc_failures() {
    let credentials = Credentials::new("wrong.example");
    let server = Running::new(&credentials, "ok").await;
    assert!(
        settings(&server.address, credentials.directory.path())
            .await
            .is_err()
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    server.finish().await;
    let credentials = Credentials::new("127.0.0.1");
    for mode in [
        "reject",
        "oversize",
        "stall",
        "wrong-unit",
        "missing-method",
    ] {
        let server = Running::new(&credentials, mode).await;
        let start = Instant::now();
        assert!(
            settings(&server.address, credentials.directory.path())
                .await
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        server.finish().await;
    }
}
