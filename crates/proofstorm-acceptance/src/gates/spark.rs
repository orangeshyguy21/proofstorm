//! Managed Spark qualification using the exact pinned native catalog images:
//! a CDK mint paid through the Spark processor, open-ssp, an operator
//! federation and the SSP's LDK Server node, against an independent peer.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

use super::stack::{
    self, Stack, melt_quote_id, melt_quotes, melt_remaining, quote_state, quote_states,
    transport::{Credentials, Target},
};
use crate::{GateContext, McpClient, cell, http, json as expect, native};
use proofstorm_core::processor_ids::{OPEN_SSP, SPARK_OPERATORS, SPARK_PROCESSOR};

mod funding;
mod payments;
mod recovery;
#[cfg(test)]
mod tests;

const INSTANCE: &str = "spark-instance";
const RUN: &str = "spark-payments";
const STACK: Stack = Stack {
    instance: INSTANCE,
    run: RUN,
    prefix: "spark",
    name: "Spark",
};
/// LDK Server 762bc47 finds its macaroon and certificate through this config.
const LDK: &str = "ldk-server-cli --config /config/config.toml --base-url 127.0.0.1:3536";
const WALLET: &str = "cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive";
const MINT: &str = "http://mint:3338";
/// open-ssp's loopback API, reached with `cell_exec` on the `ssp` component.
const SSP: &str = "http://127.0.0.1:5000";
/// The driver writes the admin bearer header to a private file, so the token
/// never appears in a command, its output or this gate's evidence.
const ADMIN: &str = "curl -sS --fail-with-body -H @/run/open-ssp/admin.header";
/// The only rail the fixed Spark profile advertises.
const METHODS: &str = "bolt11";
/// spark-operators' default federation size, one pod each.
const OPERATORS: usize = 3;
/// Every implementation that must run a digest-pinned image with provenance.
const PINNED: [&str; 4] = [SPARK_OPERATORS, OPEN_SSP, SPARK_PROCESSOR, "ldk-server"];

fn document() -> Result<Value> {
    let mut document: Value = serde_json::from_str(include_str!(
        "../../../proofstorm-core/tests/fixtures/spark-topology.json"
    ))?;
    document["name"] = json!("spark-managed-qualification");
    let components = document["components"]
        .as_array_mut()
        .context("components")?;
    // As in Bark, isolate the processor's payment fees. Native wallet receipts
    // include Cashu input fees, which are outside the backend's melt fee reserve.
    components
        .iter_mut()
        .find(|component| component["id"] == "mint")
        .context("mint component")?["config"]["input_fee_ppk"] = json!(0);
    // The peer and the channel-less island run the SSP node's exact LDK Server.
    let node = components
        .iter()
        .find(|component| component["id"] == "ssp-node")
        .context("SSP node component")?
        .clone();
    for id in ["peer", "island"] {
        let mut lightning = node.clone();
        lightning["id"] = json!(id);
        components.push(lightning);
    }
    components.push(json!({"id":"wallet","kind":"wallet","implementation":"cdk-cli-wallet","version": proofstorm_core::CDK_VERSION,"config_version":"cdk-cli-wallet/0.18/v1","control":"cell","config":{}}));
    document["links"].as_array_mut().context("links")?.extend([
        json!({"id":"peer-chain","kind":"chain_backend","from":"peer","to":"chain","binding":{"type":"chain","network":"regtest"}}),
        json!({"id":"island-chain","kind":"chain_backend","from":"island","to":"chain","binding":{"type":"chain","network":"regtest"}}),
    ]);
    document["policy"] = json!({"allow":["component.exec_live","component.control"],"limits":{"max_components":10,"max_links":16,"max_config_bytes":65536}});
    Ok(document)
}

pub(crate) fn images(catalog: &proofstorm_core::CatalogResponse) -> Result<Vec<String>> {
    let spec = serde_json::from_value(document()?)?;
    let lock = proofstorm_core::resolve_lock(&spec, catalog).map_err(anyhow::Error::msg)?;
    Ok(lock.entries.into_iter().map(|entry| entry.image).collect())
}

pub fn run(context: &GateContext) -> Result<()> {
    STACK.run(context, document()?, &PINNED, |client, namespace| {
        exercise(context, client, namespace)
    })?;
    println!(
        "Managed Spark: BOLT11 registration, processor mTLS and operator TLS, SSP and channel funding, unpaid and interrupted mint recovery across stack restarts, melts, unreachable melt refusal and cleanup passed"
    );
    Ok(())
}

fn exercise(context: &GateContext, client: &mut McpClient, namespace: &str) -> Result<()> {
    context.qualification_stage("spark-settings")?;
    let settings = native::json_output(
        client,
        INSTANCE,
        RUN,
        "processor",
        "spark-settings",
        &format!(
            "/opt/proofstorm/driver processor-settings https://127.0.0.1:50051 /processor-client/tls {SPARK_PROCESSOR}"
        ),
    )?;
    context.record("spark-settings.json", &settings)?;
    check_settings(&settings)?;
    STACK.registered(context, namespace, METHODS)?;
    context.qualification_stage("spark-transport")?;
    // The operators serve server-authenticated TLS. Their own readiness probe
    // verifies the local operator against the federation CA as `localhost`.
    let operator = native::execute(
        client,
        INSTANCE,
        RUN,
        "operators",
        "spark-operator-tls",
        "/opt/proofstorm/driver spark-operator-ready",
    )?;
    context.record("spark-operator-tls.json", &operator)?;
    stack::transport::verify(context, namespace, STACK.prefix, &transport_targets())?;
    context.qualification_stage("spark-funding")?;
    let nodes = funding::run(context, client)?;
    native::execute(
        client,
        INSTANCE,
        RUN,
        "wallet",
        "spark-wallet-init",
        &format!("{WALLET} balance >/dev/null"),
    )?;
    balance(context, client, "spark-wallet-empty", 0)?;
    context.qualification_stage("spark-unpaid-restart")?;
    recovery::unpaid(context, client, namespace, &nodes)?;
    context.qualification_stage("spark-interrupted-mint")?;
    let minted = payments::mint(context, client, namespace, &nodes)?;
    context.qualification_stage("spark-melt")?;
    let first = payments::melt(context, client, namespace, "first", 30_000, 100_000)?;
    context.qualification_stage("spark-completed-restart")?;
    recovery::restart_stack(context, client, namespace, &nodes, "paid")?;
    let issued = quote_state(context, namespace, "mint", &minted, "ISSUED")?;
    context.record("spark-issued-after-restart.json", &issued)?;
    balance(context, client, "spark-wallet-recovered", first.remaining)?;
    let recovered = payments::melt(
        context,
        client,
        namespace,
        "recovered",
        10_000,
        first.remaining,
    )?;
    context.qualification_stage("spark-unreachable-melt")?;
    payments::unreachable(context, client, namespace, recovered.remaining)?;
    context.qualification_stage("spark-preimage")?;
    // docker/payment/SPARK.md documents melts without a payment preimage.
    // Record what each settled melt carried; absence never fails the gate.
    context.record(
        "spark-preimages.json",
        &json!({"melts":{"first":first.preimage,"recovered":recovered.preimage},"documented_limitation":"melts carry no payment_preimage"}),
    )
}

/// The processor's mTLS refuses foreign clients; a real client certificate from
/// the operators' authority stands in for another service. Operator 0 serves
/// TLS without client certificates, so it must refuse plaintext and fail
/// verification under the processor's authority.
fn transport_targets() -> [Target; 2] {
    let operators = Credentials {
        secret: "operators-identity",
        ca: "ca.crt",
        identity: None,
    };
    [
        Target::service(
            "processor",
            "processor",
            50051,
            "/cdk_payment_processor.CdkPaymentProcessor/GetSettings",
            Credentials::mutual("processor-payment-tls"),
            Credentials {
                identity: Some(("server-0.crt", "server-0.key")),
                ..operators
            },
        ),
        Target {
            role: "operators",
            forward: "pod/operators-0".into(),
            pod: "statefulset.kubernetes.io/pod-name=operators-0".into(),
            port: 8535,
            // The empty service name is the operator's liveness check.
            path: "/grpc.health.v1.Health/Check",
            own: operators,
            foreign: Credentials::mutual("processor-payment-tls"),
        },
    ]
}

/// The processor advertises exactly its fixed bolt11 profile in sat.
fn check_settings(settings: &Value) -> Result<()> {
    ensure!(
        settings["unit"] == "sat"
            && settings["bolt11"].is_object()
            && settings["bolt12"].is_null()
            && settings["onchain"].is_null()
            && settings["custom"]
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
        "Spark capabilities differ from its {METHODS} sat profile"
    );
    Ok(())
}

/// An open-ssp admin request. Only the header file carries the token.
fn admin(arguments: &str, path: &str) -> String {
    format!("{ADMIN} {arguments}{SSP}{path}")
}

fn ssp_status() -> String {
    admin("", "/status")
}

fn ssp_identity() -> String {
    format!("curl -sS --fail-with-body {SSP}/identity")
}

/// Fails until the operators finish key generation, so it is retried.
fn deposit_address() -> String {
    retryable(
        &admin("-X POST ", "/admin/spark/deposit-address"),
        r#"{"address":null}"#,
    )
}

/// Retried until the operators have seen the deposit's confirmations.
fn claim_deposit(hex: &str, vout: u64) -> String {
    let body = json!({"transaction_hex":hex,"vout":vout}).to_string();
    retryable(
        &admin(
            &format!(
                "-H 'Content-Type: application/json' -d {} ",
                native::quote(&body)
            ),
            "/admin/spark/claim-deposit",
        ),
        r#"{"ok":false}"#,
    )
}

/// Print a recipe step's JSON when it succeeds, otherwise a fixed pending
/// marker, so a poll can retry it. The failure body is discarded.
fn retryable(command: &str, pending: &str) -> String {
    format!(
        "if body=$({command}); then printf '%s\\n' \"$body\"; else printf '%s\\n' {}; fi",
        native::quote(pending)
    )
}

/// The SSP's spendable Spark liquidity; none while its wallet reports an error.
fn available_sats(status: &Value) -> Result<u64> {
    if status["spark"].is_null() {
        return Ok(0);
    }
    expect::integer(status, "/spark/available_sats")
}

fn balance(context: &GateContext, client: &mut McpClient, id: &str, expected: u64) -> Result<()> {
    STACK.balance(context, client, id, expected)
}

fn control(client: &mut McpClient, tool: &str, component: &str, id: &str) -> Result<()> {
    STACK.control(client, tool, component, id)
}
