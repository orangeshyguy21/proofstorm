//! Authenticated positive controls bracket each negative transport probe.
//! Credential bytes stay in memory and never enter evidence or process arguments.
use super::{Context, Duration, GateContext, Result, Value, ensure, expect, http, json, sleep};
use std::collections::BTreeMap;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

pub(super) fn verify(context: &GateContext, namespace: &str) -> Result<()> {
    // The acceptance worker may already be inside its CLI's Tokio runtime.
    // Keep these synchronous gate calls off that runtime's executor thread.
    std::thread::scope(|scope| {
        scope
            .spawn(|| verify_blocking(context, namespace))
            .join()
            .map_err(|_| anyhow::anyhow!("transport probe worker panicked"))?
    })
}

fn verify_blocking(context: &GateContext, namespace: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for (role, service, port, secret, other_secret, path) in [
        (
            "processor",
            "processor",
            50051,
            "processor-payment-tls",
            "cln-cln-tls",
            "/cdk_payment_processor.CdkPaymentProcessor/GetSettings",
        ),
        (
            "cln",
            "cln",
            9988,
            "cln-cln-tls",
            "cln-hold-tls",
            "/cln.Node/Getinfo",
        ),
        (
            "hold",
            "cln",
            9292,
            "cln-hold-tls",
            "cln-cln-tls",
            "/hold.Hold/GetInfo",
        ),
    ] {
        let credentials = context.kubectl.secret_data(namespace, secret)?;
        let other = context.kubectl.secret_data(namespace, other_secret)?;
        ensure!(
            required(&credentials, "ca.pem")? != required(&other, "ca.pem")?,
            "native services share a TLS authority"
        );
        let wrong = Identity::from_pem(
            required(&other, "client.pem")?,
            required(&other, "client.key")?,
        );
        let ca = Certificate::from_pem(required(&credentials, "ca.pem")?);
        let valid = Identity::from_pem(
            required(&credentials, "client.pem")?,
            required(&credentials, "client.key")?,
        );
        let before = process_state(context, namespace, service)?;
        let mut forward = http::PortForward::open(
            &context.kubectl,
            namespace,
            &format!("service/{service}"),
            port,
        )?;
        // Port-forward startup is asynchronous. Retry only the read-only,
        // authenticated control before submitting each negative probe once.
        authenticated_control(&runtime, &mut forward, &ca, &valid, path)
            .with_context(|| format!("{role} authenticated positive control failed"))?;
        for (case, secure, identity) in [
            ("plaintext", false, None),
            ("missing-client", true, None),
            ("wrong-client", true, Some(wrong.clone())),
        ] {
            let address = if secure {
                forward.url("").replacen("http://", "https://", 1)
            } else {
                forward.url("")
            };
            ensure!(
                runtime
                    .block_on(probe(&address, secure.then(|| ca.clone()), identity, path))
                    .is_err(),
                "{role} accepted {case}"
            );
            // An unavailable server or incorrect RPC is never sufficient to
            // pass refusal: the identical authenticated RPC must still work.
            // kubectl can terminate its whole forward on the remote reset
            // caused by a rejected TLS handshake. Use a new tunnel, while
            // independently proving that the server process did not restart.
            forward = http::PortForward::open(
                &context.kubectl,
                namespace,
                &format!("service/{service}"),
                port,
            )?;
            let attempts = authenticated_control(
                &runtime,
                &mut forward,
                &Certificate::from_pem(required(&credentials, "ca.pem")?),
                &valid,
                path,
            )
            .with_context(|| format!("{role} positive control failed after {case}"))?;
            ensure!(
                process_state(context, namespace, service)? == before,
                "{role} restarted during transport refusal checks"
            );
            context.record(
                &format!("bark-tls-{role}-{case}.json"),
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
    identity: &Identity,
    path: &'static str,
) -> Result<u32> {
    let address = forward.url("").replacen("http://", "https://", 1);
    let mut last = String::new();
    for attempt in 1..=30 {
        ensure!(forward.running(), "transport port-forward stopped");
        match runtime.block_on(probe(
            &address,
            Some(ca.clone()),
            Some(identity.clone()),
            path,
        )) {
            Ok(()) => return Ok(attempt),
            Err(error) => last = format!("{error:#}"),
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("authenticated transport control did not recover: {last}")
}

fn process_state(context: &GateContext, namespace: &str, component: &str) -> Result<Value> {
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        &format!("{}={component}", proofstorm_kube::COMPONENT_LABEL),
    ])?;
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
