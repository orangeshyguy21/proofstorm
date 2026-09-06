//! Distinct MCP principals exercise recipient scope and real native imports.
use super::{
    cocod_wallet::operation,
    private_transfer::{capture, reserve, transfer},
};
use crate::{GateContext, McpClient, lab};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{fs, path::Path};

const INSTANCE: &str = "cocod-wallet-instance";
const EXPERIMENT: &str = "cocod-wallet-experiment";
const PARENT: &str = "cocod-wallet-session";
const RECIPIENT_CAPABILITIES: &[&str] = &[
    "catalog.read",
    "component.exec_live",
    "wallet.control",
    "experiment.read",
    "artifact.read",
    "action.cancel",
];

fn save(directory: &Path, id: &str, value: &Value) -> Result<()> {
    fs::write(
        directory.join(format!("{id}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}
fn scoped(session: &str, id: &str, mut parameters: Value) -> Value {
    parameters.as_object_mut().expect("parameters").extend(
        json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,
        "session_id":session,"operation_id":id,"idempotency_key":id})
        .as_object()
        .unwrap()
        .clone(),
    );
    parameters
}
fn child_operation(
    client: &mut McpClient,
    directory: &Path,
    session: &str,
    id: &str,
    tool: &str,
    parameters: Value,
) -> Result<Value> {
    client.call(tool, scoped(session, id, parameters))?;
    let receipt = lab::wait_operation(client, id, 60)?;
    save(directory, id, &receipt)?;
    Ok(lab::artifact_content(&receipt)?.clone())
}
fn refused(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    expected_code: &str,
    tool: &str,
    request: Value,
) -> Result<()> {
    let response = client.call_response(tool, request)?;
    save(directory, id, &response)?;
    if response.pointer("/error/data/code") != Some(&json!(expected_code)) {
        bail!("request {id} did not receive the expected {expected_code} refusal");
    }
    Ok(())
}

fn delegate(
    client: &mut McpClient,
    directory: &Path,
    principal: &str,
    session: &str,
    wallet: &str,
    reference: &str,
    receive: &Value,
) -> Result<()> {
    let value = client.call(
        "proofstorm_private_access_issue",
        json!({"instance_id":INSTANCE,"recipient_principal_id":principal,
        "recipient_grant_id":session,"component":wallet,"mint":"mint","reference":reference,
        "receive":receive,"idempotency_key":session}),
    )?;
    if value["principal_id"] != principal || value["scope"]["reference"] != reference {
        bail!("recipient session binding differs");
    }
    save(directory, session, &value)
}

#[allow(
    clippy::too_many_lines,
    reason = "two native directions share an explicit scope and evidence sequence"
)]
pub fn exercise(context: &GateContext, parent: &mut McpClient, directory: &Path) -> Result<()> {
    let workspace = format!("cocod-wallet-{}", context.run_id);
    let mut cdk = context.session(&workspace, "recipient-cdk", RECIPIENT_CAPABILITIES)?;
    let mut coco = context.session(&workspace, "recipient-coco", RECIPIENT_CAPABILITIES)?;
    let cdk_prefix = json!([
        "cdk-cli",
        "--work-dir",
        "/wallet/cdk",
        "--unit",
        "sat",
        "--non-interactive",
        "receive",
        "--allow-untrusted",
        "@proofstorm-private-input"
    ]);
    let coco_receive = "import sys,json,urllib.request\nfrom pathlib import Path\ntoken=sys.stdin.read();key=Path('/wallet/.cocod/credentials/current/client').read_text().strip()\nr=urllib.request.Request('http://127.0.0.1:62626/receive/cashu',data=json.dumps({'token':token}).encode(),headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})\nwith urllib.request.urlopen(r,timeout=40) as response: result=json.load(response)\nassert 'error' not in result and 'output' in result";
    for (
        tag,
        principal,
        session,
        source,
        destination,
        amount,
        expected,
        send,
        receive,
        input,
        recipient,
    ) in [
        (
            "out",
            "recipient-cdk",
            "handoff-cdk",
            "wallet-a",
            "wallet-b",
            200,
            600,
            json!([
                "cocod",
                "send",
                "cashu",
                "200",
                "--mint-url",
                "http://mint:3338"
            ]),
            cdk_prefix,
            json!({"kind":"argv","index":8}),
            &mut cdk,
        ),
        (
            "back",
            "recipient-coco",
            "handoff-coco",
            "wallet-b",
            "wallet-a",
            100,
            4500,
            json!([
                "cdk-cli",
                "--work-dir",
                "/wallet/cdk",
                "--unit",
                "sat",
                "--non-interactive",
                "send",
                "--mint-url",
                "http://mint:3338",
                "--amount",
                "100"
            ]),
            json!(["python3", "-c", coco_receive]),
            json!({"kind":"stdin"}),
            &mut coco,
        ),
    ] {
        let id = |step: &str| format!("handoff-{tag}-{step}");
        let reference = reserve(
            parent,
            directory,
            &id("reserve"),
            source,
            destination,
            65536,
        )?;
        capture(
            parent,
            directory,
            &id("send"),
            source,
            &reference,
            send,
            "cashu_token",
        )?;
        delegate(
            parent,
            directory,
            principal,
            session,
            destination,
            &reference,
            &json!({"argv":receive,"timeout_seconds":60,"input":input}),
        )?;
        // A child cannot clear the parent's runtime session before ownership is checked.
        refused(
            recipient,
            directory,
            &id("parent-release-denied"),
            "validation_failed",
            "proofstorm_session_finish",
            json!({"session_id":PARENT,"idempotency_key":id("parent-release-denied")}),
        )?;
        refused(
            recipient,
            directory,
            &id("sender-balance-denied"),
            "access_denied",
            "proofstorm_wallet_balance",
            scoped(
                session,
                &id("sender-balance-denied"),
                json!({"wallet":source,"mint":"mint"}),
            ),
        )?;
        refused(
            recipient,
            directory,
            &id("unbound-exec-denied"),
            "access_denied",
            "proofstorm_component_exec_live",
            scoped(
                session,
                &id("unbound-exec-denied"),
                json!({"component":destination,"argv":["true"],"timeout_seconds":10}),
            ),
        )?;
        let handed = transfer(
            parent,
            directory,
            &id("bind"),
            json!({"transferMethod":"handoff","component":source,"reference":reference,"recipientGrantId":session}),
        )?;
        if handed["recipient"]["principal"] != principal
            || handed["recipient"]["session"] != session
        {
            bail!("custody recipient binding differs");
        }
        let mut substituted = receive.clone();
        substituted[0] = json!("unapproved-receive");
        refused(
            recipient,
            directory,
            &id("command-denied"),
            "access_denied",
            "proofstorm_component_exec_live",
            scoped(
                session,
                &id("command-denied"),
                json!({
            "component":destination,"argv":substituted,"timeout_seconds":60,"output":{"mode":"private"},
            "private_payload":{"kind":"consume","reference":reference,"input":input}}),
            ),
        )?;
        let read = recipient.call(
            "proofstorm_private_access_read",
            json!({"grant_id":session}),
        )?;
        save(directory, &id("recipient-scope"), &read)?;
        let delivered = child_operation(
            recipient,
            directory,
            session,
            &id("deliver"),
            "proofstorm_private_transfer",
            json!({"transfer":{"transferMethod":"deliver","component":destination,"reference":reference}}),
        )?;
        if delivered["transfer"]["delivered"] != true {
            bail!("delegated inbox not delivered");
        }
        let native = child_operation(
            recipient,
            directory,
            session,
            &id("receive"),
            "proofstorm_component_exec_live",
            json!({"component":destination,"argv":receive,"timeout_seconds":60,"output":{"mode":"private"},
            "private_payload":{"kind":"consume","reference":reference,"input":input}}),
        )?;
        if native["exit_code"] != 0
            || native["cleanup_verified"] != true
            || native["streams_complete"] != true
            || native["output_truncated"] != false
            || native["stdout"] != ""
            || native["stderr"] != ""
            || native["private_files_retired"] != true
        {
            bail!("delegated native receive contract failed");
        }
        let balance = child_operation(
            recipient,
            directory,
            session,
            &id("balance"),
            "proofstorm_wallet_balance",
            json!({"wallet":destination,"mint":"mint"}),
        )?;
        if balance["balance_sat"] != expected {
            bail!("delegated {amount} sat receipt did not reach expected balance");
        }
        let released = parent.call(
            "proofstorm_private_access_revoke",
            json!({"grant_id":session}),
        )?;
        save(directory, &id("revoke"), &released)?;
        refused(
            recipient,
            directory,
            &id("revoked-balance-denied"),
            "access_denied",
            "proofstorm_wallet_balance",
            scoped(
                session,
                &id("revoked-balance-denied"),
                json!({"wallet":destination,"mint":"mint"}),
            ),
        )?;
        transfer(
            parent,
            directory,
            &id("release"),
            json!({"transferMethod":"release","component":source,"reference":reference}),
        )?;
    }
    revoked_before_receive(context, parent, &mut coco, directory)?;
    super::cocod_wallet::private(
        parent,
        directory,
        "handoff-reconcile",
        "wallet-b",
        json!({"argv":["cdk-cli","--work-dir","/wallet/cdk","--unit","sat","--non-interactive","check-pending"]}),
        0,
    )?;
    let final_cdk = operation(
        parent,
        directory,
        "proofstorm_wallet_balance",
        "handoff-cdk-final",
        json!({"wallet":"wallet-b","mint":"mint"}),
    )?;
    let final_coco = operation(
        parent,
        directory,
        "proofstorm_wallet_balance",
        "handoff-coco-final",
        json!({"wallet":"wallet-a","mint":"mint"}),
    )?;
    if final_cdk["balance_sat"] != 500
        || final_coco["balance_sat"] != 4500
        || final_cdk["pending_sat"] != 0
        || final_cdk["reserved_sat"] != 0
        || final_cdk["pending_spent_sat"] != 0
        || final_coco["reserved_sat"] != 0
        || final_coco["inflight_sat"] != 0
    {
        bail!("cross-principal conservation/reconciliation failed");
    }
    save(
        directory,
        "handoff-summary",
        &json!({"principals":["experiment-agent","recipient-cdk","recipient-coco"],"directions":[200,100],"final_balances":[4500,500],"transport":"infrastructure_relay","scope":"one transfer and wallet per recipient session"}),
    )
}

fn revoked_before_receive(
    context: &GateContext,
    parent: &mut McpClient,
    recipient: &mut McpClient,
    directory: &Path,
) -> Result<()> {
    let reference = reserve(
        parent,
        directory,
        "handoff-revoke-reserve",
        "wallet-b",
        "wallet-a",
        65536,
    )?;
    let captured = capture(
        parent,
        directory,
        "handoff-revoke-capture",
        "wallet-b",
        &reference,
        json!([
            "python3",
            "-c",
            "import secrets,sys; sys.stdout.buffer.write(secrets.token_bytes(4096))"
        ]),
        "bytes",
    )?;
    delegate(
        parent,
        directory,
        "recipient-coco",
        "handoff-revoked",
        "wallet-a",
        &reference,
        &json!({"argv":["python3","-c","import sys; sys.stdin.buffer.read()"],"timeout_seconds":60,"input":{"kind":"stdin"}}),
    )?;
    transfer(
        parent,
        directory,
        "handoff-revoke-bind",
        json!({"transferMethod":"handoff","component":"wallet-b","reference":reference,"recipientGrantId":"handoff-revoked"}),
    )?;
    let before = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
    ])?;
    context.kubectl.run(&[
        "rollout",
        "restart",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
    ])?;
    context.kubectl.run(&[
        "rollout",
        "status",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--timeout=120s",
    ])?;
    let after = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
    ])?;
    if before["items"][0]["metadata"]["uid"] == after["items"][0]["metadata"]["uid"] {
        bail!("handoff controller was not replaced");
    }
    save(
        directory,
        "handoff-controller-restart",
        &json!({"before":before["items"][0]["metadata"]["uid"],"after":after["items"][0]["metadata"]["uid"]}),
    )?;
    let restored = child_operation(
        recipient,
        directory,
        "handoff-revoked",
        "handoff-after-restart",
        "proofstorm_private_transfer",
        json!({"transfer":{"transferMethod":"status","component":"wallet-a","reference":reference}}),
    )?;
    if restored["transfer"]["sha256"] != captured["transfer"]["sha256"]
        || restored["transfer"]["recipient"]["principal"] != "recipient-coco"
    {
        bail!("recipient custody did not survive controller replacement");
    }
    parent.call(
        "proofstorm_private_access_revoke",
        json!({"grant_id":"handoff-revoked"}),
    )?;
    refused(
        recipient,
        directory,
        "handoff-revoked-deliver",
        "access_denied",
        "proofstorm_private_transfer",
        scoped(
            "handoff-revoked",
            "handoff-revoked-deliver",
            json!({"transfer":{"transferMethod":"deliver","component":"wallet-a","reference":reference}}),
        ),
    )?;
    let retired = transfer(
        parent,
        directory,
        "handoff-revoked-release",
        json!({"transferMethod":"release","component":"wallet-b","reference":reference}),
    )?;
    if !retired["receiver"]["operation_id"].is_null() || retired["capture"] != "released" {
        bail!("revoked transfer admitted a consumer or failed retirement");
    }
    Ok(())
}
