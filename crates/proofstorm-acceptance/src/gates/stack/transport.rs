//! Authenticated positive controls bracket each negative transport probe.
//! Credential bytes stay in memory and never enter evidence or process arguments.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, thread::sleep, time::Duration};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

use crate::{GateContext, http, json as expect};

#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

/// Key names inside one Secret: an authority and, for mutual TLS, a client identity.
#[derive(Clone, Copy)]
pub(crate) struct Credentials {
    pub(crate) secret: &'static str,
    pub(crate) ca: &'static str,
    pub(crate) identity: Option<(&'static str, &'static str)>,
}

impl Credentials {
    /// The layout of CDK payment-processor and CLN gRPC TLS Secrets.
    pub(crate) const fn mutual(secret: &'static str) -> Self {
        Self {
            secret,
            ca: "ca.pem",
            identity: Some(("client.pem", "client.key")),
        }
    }
}

/// One TLS endpoint, and another authority whose credentials must not reach it.
pub(crate) struct Target {
    pub(crate) role: &'static str,
    /// The `kubectl port-forward` target, such as `service/processor`.
    pub(crate) forward: String,
    /// Label selector of the one pod serving `forward`.
    pub(crate) pod: String,
    pub(crate) port: u16,
    /// A read-only unary RPC that accepts an empty request.
    pub(crate) path: &'static str,
    pub(crate) own: Credentials,
    pub(crate) foreign: Credentials,
}

impl Target {
    /// A component's Service, served by its single pod.
    pub(crate) fn service(
        role: &'static str,
        service: &str,
        port: u16,
        path: &'static str,
        own: Credentials,
        foreign: Credentials,
    ) -> Self {
        Self {
            role,
            forward: format!("service/{service}"),
            pod: format!("{}={service}", proofstorm_kube::COMPONENT_LABEL),
            port,
            path,
            own,
            foreign,
        }
    }
}

/// Mutual TLS must refuse plaintext, a missing client certificate and a client
/// from another authority. A server-authenticated endpoint takes no client
/// certificate: it must refuse plaintext and fail verification under another
/// authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Plaintext,
    MissingClient,
    WrongClient,
    WrongAuthority,
}

impl Case {
    fn all(mutual: bool) -> &'static [Self] {
        if mutual {
            &[Self::Plaintext, Self::MissingClient, Self::WrongClient]
        } else {
            &[Self::Plaintext, Self::WrongAuthority]
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::MissingClient => "missing-client",
            Self::WrongClient => "wrong-client",
            Self::WrongAuthority => "wrong-ca",
        }
    }
}

/// Probe each target, recording `{prefix}-tls-{role}-{case}.json` per refusal.
pub(crate) fn verify(
    context: &GateContext,
    namespace: &str,
    prefix: &str,
    targets: &[Target],
) -> Result<()> {
    // The acceptance worker may already be inside its CLI's Tokio runtime.
    // Keep these synchronous gate calls off that runtime's executor thread.
    std::thread::scope(|scope| {
        scope
            .spawn(|| verify_blocking(context, namespace, prefix, targets))
            .join()
            .map_err(|_| anyhow::anyhow!("transport probe worker panicked"))?
    })
}

fn verify_blocking(
    context: &GateContext,
    namespace: &str,
    prefix: &str,
    targets: &[Target],
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for target in targets {
        let role = target.role;
        let credentials = context.kubectl.secret_data(namespace, target.own.secret)?;
        let other = context
            .kubectl
            .secret_data(namespace, target.foreign.secret)?;
        ensure!(
            required(&credentials, target.own.ca)? != required(&other, target.foreign.ca)?,
            "native services share a TLS authority"
        );
        let wrong = identity(&other, &target.foreign)?;
        let foreign_ca = Certificate::from_pem(required(&other, target.foreign.ca)?);
        let ca = Certificate::from_pem(required(&credentials, target.own.ca)?);
        let valid = identity(&credentials, &target.own)?;
        let before = process_state(context, namespace, &target.pod)?;
        let mut forward =
            http::PortForward::open(&context.kubectl, namespace, &target.forward, target.port)?;
        // Port-forward startup is asynchronous. Retry only the read-only,
        // authenticated control before submitting each negative probe once.
        authenticated_control(&runtime, &mut forward, &ca, valid.as_ref(), target.path)
            .with_context(|| format!("{role} authenticated positive control failed"))?;
        for case in Case::all(valid.is_some()) {
            let (authority, identity) = match case {
                Case::Plaintext => (None, None),
                Case::MissingClient => (Some(ca.clone()), None),
                Case::WrongClient => (
                    Some(ca.clone()),
                    Some(wrong.clone().context("missing transport credential")?),
                ),
                Case::WrongAuthority => (Some(foreign_ca.clone()), None),
            };
            let case = case.name();
            let address = if authority.is_some() {
                forward.url("").replacen("http://", "https://", 1)
            } else {
                forward.url("")
            };
            ensure!(
                runtime
                    .block_on(probe(&address, authority, identity, target.path))
                    .is_err(),
                "{role} accepted {case}"
            );
            // An unavailable server or incorrect RPC is never sufficient to
            // pass refusal: the identical authenticated RPC must still work.
            // kubectl can terminate its whole forward on the remote reset
            // caused by a rejected TLS handshake. Use a new tunnel, while
            // independently proving that the server process did not restart.
            forward =
                http::PortForward::open(&context.kubectl, namespace, &target.forward, target.port)?;
            let attempts =
                authenticated_control(&runtime, &mut forward, &ca, valid.as_ref(), target.path)
                    .with_context(|| format!("{role} positive control failed after {case}"))?;
            ensure!(
                process_state(context, namespace, &target.pod)? == before,
                "{role} restarted during transport refusal checks"
            );
            context.record(
                &format!("{prefix}-tls-{role}-{case}.json"),
                &json!({"role":role,"case":case,"refused":true,"authenticated_control_after":true,"fresh_control_forward":true,"control_attempts":attempts,"unchanged_process":before}),
            )?;
        }
    }
    Ok(())
}

fn authenticated_control(
    runtime: &tokio::runtime::Runtime,
    forward: &mut http::PortForward,
    ca: &Certificate,
    identity: Option<&Identity>,
    path: &'static str,
) -> Result<u32> {
    let address = forward.url("").replacen("http://", "https://", 1);
    let mut last = String::new();
    for attempt in 1..=30 {
        ensure!(forward.running(), "transport port-forward stopped");
        match runtime.block_on(probe(&address, Some(ca.clone()), identity.cloned(), path)) {
            Ok(()) => return Ok(attempt),
            Err(error) => last = format!("{error:#}"),
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("authenticated transport control did not recover: {last}")
}

fn process_state(context: &GateContext, namespace: &str, selector: &str) -> Result<Value> {
    let pods = context
        .kubectl
        .get_json(&["get", "pods", "-n", namespace, "-l", selector])?;
    let pods = expect::array(&pods, "/items")?;
    ensure!(
        pods.len() == 1 && pods[0]["metadata"]["deletionTimestamp"].is_null(),
        "transport service has no stable pod"
    );
    let restarts = expect::array(&pods[0], "/status/containerStatuses")?
        .iter()
        .map(|status| {
            Ok((
                expect::string(status, "/name")?,
                expect::integer(status, "/restartCount")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    ensure!(!restarts.is_empty(), "transport container state is missing");
    Ok(json!({"pod_uid":expect::string(&pods[0],"/metadata/uid")?,"restarts":restarts}))
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

fn required<'a>(credentials: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    credentials
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .context("missing transport credential")
}

/// The client identity `names` declares, if any.
fn identity(
    credentials: &BTreeMap<String, String>,
    names: &Credentials,
) -> Result<Option<Identity>> {
    names
        .identity
        .map(|(certificate, key)| {
            Ok(Identity::from_pem(
                required(credentials, certificate)?,
                required(credentials, key)?,
            ))
        })
        .transpose()
}

async fn probe(
    address: &str,
    ca: Option<Certificate>,
    identity: Option<Identity>,
    path: &'static str,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut endpoint = Endpoint::from_shared(address.to_owned())?
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2));
        if let Some(ca) = ca {
            let mut tls = ClientTlsConfig::new().ca_certificate(ca);
            if let Some(identity) = identity {
                tls = tls.identity(identity);
            }
            endpoint = endpoint.tls_config(tls)?;
        }
        let mut client =
            tonic::client::Grpc::new(endpoint.connect().await?).max_decoding_message_size(65_536);
        client.ready().await?;
        let mut request = tonic::Request::new(Empty {});
        request
            .metadata_mut()
            .insert("x-cdk-protocol-version", "4.0.0".parse()?);
        client
            .unary(
                request,
                tonic::codegen::http::uri::PathAndQuery::from_static(path),
                tonic_prost::ProstCodec::<Empty, Empty>::default(),
            )
            .await?;
        Ok(())
    })
    .await
    .context("transport probe deadline")?
}
