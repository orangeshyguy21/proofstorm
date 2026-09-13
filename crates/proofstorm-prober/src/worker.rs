//! A persistent, credential-free network worker. Nothing is shelled out or queued unboundedly.
use std::{future::Future, io, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::{StreamExt, stream};
use hickory_resolver::{
    TokioResolver,
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
};
use http_body_util::Empty;
use hyper_util::rt::TokioIo;
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, timeout},
};

use crate::{
    CHECK_TIMEOUT_MILLIS, MAX_BATCH_TARGETS, MAX_WORKER_CHECKS, Observation, Outcome,
    PROTOCOL_VERSION, Request, Response, dns_label, transport,
};

const MAX_CONNECTIONS: usize = crate::MAX_BATCHES_PER_WORKER + 2;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// One worker for one cell, with a hard bound on checks across all connections.
pub struct Worker {
    instance_key: String,
    service_suffix: String,
    dns: TokioResolver,
    checks: Semaphore,
}

impl Worker {
    /// Read the Pod's DNS configuration once at startup, preserving custom cluster domains.
    /// Absolute Service names avoid multiplying queries through the DNS search list.
    ///
    /// # Errors
    /// Rejects invalid identity or missing namespace DNS configuration.
    pub fn from_system_config(instance_key: &str, namespace: &str) -> io::Result<Arc<Self>> {
        if !dns_label(instance_key)
            || !dns_label(namespace)
            || namespace != format!("proofstorm-{instance_key}")
        {
            return Err(io::Error::other("invalid prober identity"));
        }
        let (config, options) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|_| io::Error::other("cannot read prober DNS configuration"))?;
        Self::from_dns_config(instance_key, namespace, config, options)
    }

    /// Construct an embedded worker using explicit DNS configuration. The executable uses
    /// the Pod's configuration; this entry point also permits isolated resolver fault tests.
    ///
    /// # Errors
    /// Rejects mismatched identity or a missing namespace Service search domain.
    pub fn from_dns_config(
        instance_key: &str,
        namespace: &str,
        config: ResolverConfig,
        mut options: ResolverOpts,
    ) -> io::Result<Arc<Self>> {
        if !dns_label(instance_key)
            || !dns_label(namespace)
            || namespace != format!("proofstorm-{instance_key}")
        {
            return Err(io::Error::other("invalid prober identity"));
        }
        let prefix = format!("{namespace}.svc.");
        let service_suffix = config
            .search()
            .iter()
            .map(ToString::to_string)
            .find(|name| name.starts_with(&prefix))
            .ok_or_else(|| io::Error::other("namespace Service DNS search domain is missing"))?;
        options.cache_size = 0;
        options.attempts = 1;
        options.timeout = Duration::from_millis(CHECK_TIMEOUT_MILLIS);
        options.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        options.num_concurrent_reqs = 1;
        let resolver = TokioResolver::builder_with_config(
            config,
            hickory_resolver::name_server::TokioConnectionProvider::default(),
        )
        .with_options(options)
        .build();
        Ok(Self::new(instance_key, &service_suffix, resolver))
    }

    fn new(instance_key: &str, service_suffix: &str, resolver: TokioResolver) -> Arc<Self> {
        Arc::new(Self {
            instance_key: instance_key.into(),
            service_suffix: service_suffix.trim_end_matches('.').into(),
            dns: resolver,
            checks: Semaphore::new(MAX_WORKER_CHECKS),
        })
    }

    /// Serve bounded requests until shutdown, cancelling and joining every connection task.
    ///
    /// # Errors
    /// Returns listener failures. Malformed, disconnected or idle clients are closed individually.
    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                () = &mut shutdown => break Ok(()),
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => break Err(error),
                    };
                    let Ok(permit) = connections.clone().try_acquire_owned() else { continue; };
                    let worker = self.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let _ = worker.connection(stream).await;
                    });
                }
            }
        };
        tasks.shutdown().await;
        result
    }

    async fn connection(&self, socket: TcpStream) -> io::Result<()> {
        socket.set_nodelay(true)?;
        let (mut reader, mut writer) = socket.into_split();
        loop {
            let request: Request = timeout(IDLE_TIMEOUT, transport::read(&mut reader)).await??;
            let keep_alive = request.keep_alive;
            // One request at a time per connection. EOF or pipelined input cancels this
            // batch immediately, rather than retaining work after a controller disconnect.
            let mut unexpected = [0];
            let response = tokio::select! {
                response = self.evaluate(request) => response,
                _ = reader.read(&mut unexpected) => return Ok(()),
            };
            timeout(WRITE_TIMEOUT, transport::write(&mut writer, &response)).await??;
            if !keep_alive {
                return Ok(());
            }
        }
    }

    /// Execute one complete validated batch. Overload is explicit, never mistaken for failure.
    pub async fn evaluate(&self, request: Request) -> Response {
        if let Err(code) = request.validate(&self.instance_key) {
            return Response::Rejected { code: code.into() };
        }
        if request.targets.iter().any(|target| {
            target
                .http_path
                .as_ref()
                .is_some_and(|path| path.parse::<hyper::Uri>().is_err())
        }) {
            return Response::Rejected {
                code: "invalid_probe_target".into(),
            };
        }
        let Ok(count) = u32::try_from(request.targets.len()) else {
            return Response::Rejected {
                code: "invalid_batch_size".into(),
            };
        };
        let Ok(_permits) = self.checks.try_acquire_many(count) else {
            return Response::Rejected {
                code: "worker_busy".into(),
            };
        };
        let mut observations = stream::iter(request.targets)
            .map(|target| async move {
                let started = Instant::now();
                let host = format!("{}.{}.", target.component, self.service_suffix);
                let (outcome, http_status) = timeout(
                    Duration::from_millis(CHECK_TIMEOUT_MILLIS),
                    self.check(&host, target.port, target.http_path.as_deref()),
                )
                .await
                .unwrap_or((Outcome::TimedOut, None));
                let finished = Instant::now();
                (
                    Observation {
                        component: target.component,
                        rollout_digest: target.rollout_digest,
                        outcome,
                        elapsed_micros: bounded_u64(finished.duration_since(started).as_micros()),
                        age_millis: 0,
                        http_status,
                    },
                    finished,
                )
            })
            .buffer_unordered(MAX_BATCH_TARGETS)
            .collect::<Vec<_>>()
            .await;
        observations.sort_unstable_by(|a, b| a.0.component.cmp(&b.0.component));
        let now = Instant::now();
        Response::Complete {
            protocol_version: PROTOCOL_VERSION,
            instance_key: request.instance_key,
            revision_digest: request.revision_digest,
            batch_id: request.batch_id,
            observations: observations
                .into_iter()
                .map(|(mut observation, finished)| {
                    observation.age_millis = bounded_u64(now.duration_since(finished).as_millis());
                    observation
                })
                .collect(),
        }
    }

    async fn check(&self, host: &str, port: u16, path: Option<&str>) -> (Outcome, Option<u16>) {
        let Ok(addresses) = self.dns.lookup_ip(host).await else {
            return (Outcome::DnsFailed, None);
        };
        let mut outcome = Outcome::DnsFailed;
        for ip in addresses.iter() {
            match TcpStream::connect(SocketAddr::new(ip, port)).await {
                Ok(stream) => {
                    return if let Some(path) = path {
                        check_http(stream, host, port, path).await
                    } else {
                        (Outcome::Reachable, None)
                    };
                }
                Err(error) => {
                    outcome = match error.kind() {
                        io::ErrorKind::ConnectionRefused => Outcome::ConnectionRefused,
                        io::ErrorKind::TimedOut => Outcome::TimedOut,
                        _ => Outcome::TransportError,
                    }
                }
            }
        }
        (outcome, None)
    }
}

// Drive the HTTP connection in the check's own future: cancellation drops the socket,
// and no detached HTTP connection task or idle pool survives a finished check.
async fn check_http(
    stream: TcpStream,
    host: &str,
    port: u16,
    path: &str,
) -> (Outcome, Option<u16>) {
    let result: Result<(Outcome, Option<u16>), ()> = async {
        let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
            .max_buf_size(16 * 1024)
            .max_headers(64)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|_| ())?;
        let request = hyper::Request::builder()
            .uri(path)
            .header(hyper::header::HOST, format!("{host}:{port}"))
            .body(Empty::<Bytes>::new())
            .map_err(|_| ())?;
        tokio::pin!(connection);
        let response = sender.send_request(request);
        tokio::pin!(response);
        let response = tokio::select! {
            response = &mut response => response.map_err(|_| ())?,
            // A Connection: close response can finish the driver in the same poll that
            // delivers valid headers. Collect its queued response before deciding failure.
            _ = &mut connection => response.await.map_err(|_| ())?,
        };
        let status = response.status();
        Ok((
            if status.is_success() {
                Outcome::Reachable
            } else {
                Outcome::HttpError
            },
            Some(status.as_u16()),
        ))
    }
    .await;
    result.unwrap_or((Outcome::TransportError, None))
}

fn bounded_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
