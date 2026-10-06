use super::*;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context as TaskContext, Poll},
};
use tonic::{
    Request, Response, Status,
    codegen::{Body, BoxFuture, Service, StdError, http},
    transport::{Server, ServerTlsConfig},
};

#[derive(Clone)]
struct ReadOnly(Arc<AtomicUsize>);
impl tonic::server::NamedService for ReadOnly {
    const NAME: &'static str = "hold.Hold";
}
impl tonic::server::UnaryService<Empty> for ReadOnly {
    type Response = Empty;
    type Future = BoxFuture<Response<Empty>, Status>;
    fn call(&mut self, request: Request<Empty>) -> Self::Future {
        assert!(!request.peer_certs().unwrap().is_empty());
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Response::new(Empty {})) })
    }
}
impl<B> Service<http::Request<B>> for ReadOnly
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            Ok(
                tonic::server::Grpc::new(tonic_prost::ProstCodec::<Empty, Empty>::default())
                    .unary(service, request)
                    .await,
            )
        })
    }
}

#[tokio::test]
async fn real_mtls_probe_rejects_plaintext_missing_and_foreign_clients() {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let identity = |names: Vec<String>, usage| {
        let mut params = CertificateParams::new(names).unwrap();
        params.extended_key_usages = vec![usage];
        let key = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &ca, &ca_key).unwrap();
        Identity::from_pem(cert.pem(), key.serialize_pem())
    };
    let tls = ServerTlsConfig::new()
        .identity(identity(
            vec!["127.0.0.1".into()],
            ExtendedKeyUsagePurpose::ServerAuth,
        ))
        .client_ca_root(Certificate::from_pem(ca.pem()));
    let valid = identity(vec![], ExtendedKeyUsagePurpose::ClientAuth);
    let foreign = rcgen::generate_simple_self_signed(vec!["foreign-client".into()]).unwrap();
    let foreign = Identity::from_pem(foreign.cert.pem(), foreign.key_pair.serialize_pem());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = Server::builder()
        .tls_config(tls)
        .unwrap()
        .add_service(ReadOnly(calls.clone()));
    let task = tokio::spawn(server.serve_with_incoming_shutdown(
        tokio_stream::wrappers::TcpListenerStream::new(listener),
        async {
            let _ = stopped.await;
        },
    ));
    let secure = format!("https://{address}");
    let plain = format!("http://{address}");
    let ca = Certificate::from_pem(ca.pem());
    let path = "/hold.Hold/GetInfo";
    probe(&secure, Some(ca.clone()), Some(valid.clone()), path)
        .await
        .unwrap();
    for (address, authority, identity) in [
        (plain.as_str(), None, None),
        (secure.as_str(), Some(ca.clone()), None),
        (secure.as_str(), Some(ca.clone()), Some(foreign)),
    ] {
        assert!(probe(address, authority, identity, path).await.is_err());
        probe(&secure, Some(ca.clone()), Some(valid.clone()), path)
            .await
            .unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "only authenticated controls reach the RPC"
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
