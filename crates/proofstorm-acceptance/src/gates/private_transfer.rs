//! Metadata-only MCP custody, controller restart, and pinned native Cashu flows.
#![allow(
    clippy::needless_pass_by_value,
    reason = "acceptance helpers own constructed JSON request fixtures"
)]
use super::cocod_wallet::{
    balance, native, operation, private, python, relay_invoice, restart, start_session,
};
use crate::{GateContext, McpClient, lab};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn transfer(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    parameters: Value,
) -> Result<Value> {
    let artifact = operation(
        client,
        directory,
        "proofstorm_private_transfer",
        id,
        json!({"transfer":parameters}),
    )?;
    Ok(artifact["transfer"].clone())
}
pub(super) fn reserve(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    source: &str,
    destination: &str,
    maximum: u32,
) -> Result<String> {
    let t = transfer(
        client,
        directory,
        id,
        json!({"transferMethod":"prepare","component":source,"destinationComponent":destination,"maximumBytes":maximum}),
    )?;
    Ok(crate::json::string(&t, "/id")?.into())
}
fn deliver(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    destination: &str,
    reference: &str,
) -> Result<()> {
    let t = transfer(
        client,
        directory,
        id,
        json!({"transferMethod":"deliver","component":destination,"reference":reference}),
    )?;
    if t["delivered"] != true {
        bail!("private inbox was not delivered");
    }
    Ok(())
}
pub(super) fn capture(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    source: &str,
    reference: &str,
    argv: Value,
    format: &str,
) -> Result<Value> {
    let receipt = private(
        client,
        directory,
        id,
        source,
        json!({"argv":argv,"private_payload":{"kind":"capture","reference":reference,"format":format}}),
        0,
    )?;
    if receipt["transfer"]["capture"] != "ready" || receipt["private_files_retired"] != true {
        bail!("private capture or source retirement failed");
    }
    Ok(receipt)
}
fn consume(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    destination: &str,
    reference: &str,
    argv: Value,
    input: Value,
) -> Result<()> {
    let receipt = private(
        client,
        directory,
        id,
        destination,
        json!({"argv":argv,"private_payload":{"kind":"consume","reference":reference,"input":input}}),
        0,
    )?;
    if receipt["transfer"]["receiver"]["receipt"]["exit_code"] != 0
        || receipt["private_files_retired"] != true
    {
        bail!("private native receive evidence incomplete");
    }
    Ok(())
}

pub fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &Path,
    namespace: &str,
) -> Result<()> {
    let synthetic = reserve(
        client,
        directory,
        "synthetic-reserve",
        "wallet-b",
        "wallet-a",
        600_000,
    )?;
    let produced = capture(
        client,
        directory,
        "synthetic-capture",
        "wallet-b",
        &synthetic,
        json!([
            "python3",
            "-c",
            "import sys;sys.stdout.write('private-transfer-canary-'*24000)"
        ]),
        "bytes",
    )?;
    if produced["transfer"]["bytes"] != (b"private-transfer-canary-".len() * 24000) as u64 {
        bail!("large source length differs");
    }
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
        bail!("controller was not replaced");
    }
    std::fs::write(
        directory.join("controller-restart.json"),
        serde_json::to_vec_pretty(
            &json!({"before":before["items"][0]["metadata"]["uid"],"after":after["items"][0]["metadata"]["uid"]}),
        )?,
    )?;
    let reopened = transfer(
        client,
        directory,
        "synthetic-after-restart",
        json!({"transferMethod":"status","component":"wallet-b","reference":synthetic}),
    )?;
    if reopened["capture"] != "ready" || reopened["sha256"] != produced["transfer"]["sha256"] {
        bail!("private custody did not survive controller restart");
    }
    deliver(
        client,
        directory,
        "synthetic-deliver",
        "wallet-a",
        &synthetic,
    )?;
    let digest = crate::json::string(&reopened, "/sha256")?;
    consume(
        client,
        directory,
        "synthetic-consume",
        "wallet-a",
        &synthetic,
        json!([
            "python3",
            "-c",
            format!(
                "import sys,hashlib; b=sys.stdin.buffer.read(); assert len(b)==576000 and hashlib.sha256(b).hexdigest()=='{digest}'"
            )
        ]),
        json!({"kind":"stdin"}),
    )?;
    let replay = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "synthetic-replay-refused",
        json!({"component":"wallet-a","argv":["python3","-c","raise SystemExit(99)"],"timeout_seconds":10,"private_payload":{"kind":"consume","reference":synthetic,"input":{"kind":"stdin"}}}),
    );
    if replay.is_ok() {
        bail!("duplicate private consumer was admitted");
    }
    let refused: Value = serde_json::from_slice(&std::fs::read(
        directory.join("synthetic-replay-refused.json"),
    )?)?;
    if refused["phase"] != "failed" || refused["artifact"]["content"].get("exit_code").is_some() {
        bail!("duplicate refusal did not precede native execution");
    }
    transfer(
        client,
        directory,
        "synthetic-release",
        json!({"transferMethod":"release","component":"wallet-b","reference":synthetic}),
    )?;

    // Native protected cocod initialization and mint configuration, as in its
    // existing deterministic lifecycle gate. No token/proof data is projected.
    private(
        client,
        directory,
        "initialize-wallet-a",
        "wallet-a",
        json!({"script":python("p=Path('/wallet/session.passphrase'); p.write_text(secrets.token_urlsafe(32)); p.chmod(0o600)\nr=api('/v1/admin/wallet/initialize',{'passphrase':p.read_text()}); assert r['generatedMnemonic']\nconfig=root/'config.json'; settings=json.loads(config.read_text()); settings['mintUrl']='http://mint:3338'; config.write_text(json.dumps(settings)); config.chmod(0o600)")}),
        0,
    )?;
    restart(
        context,
        client,
        directory,
        namespace,
        "wallet-a",
        "configured-wallet-a",
    )?;
    start_session(client, directory, "wallet-a", "start-wallet-a")?;
    operation(
        client,
        directory,
        "proofstorm_liquidity_bootstrap",
        "bootstrap",
        json!({"chain":"chain","mint_lightning":"mint-lnd","payer_lightning":"payer-lnd","funding_sat":50_000_000,"channel_sat":10_000_000,"push_sat":5_000_000}),
    )?;
    let invoice = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "funding-invoice",
        json!({"component":"wallet-a","argv":["cocod","receive","bolt11","5000","--mint-url","http://mint:3338"],"timeout_seconds":60,"output":{"mode":"bolt11"}}),
    )?;
    let request = relay_invoice(&invoice, 5000)?;
    let paid = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "funding-payment",
        json!({"component":"payer-lnd","argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","payinvoice","--force","--json",request],"timeout_seconds":60,"output":{"mode":"json_fields","fields":["status","value_sat"]}}),
    )?;
    if paid["selected_output"]["status"] != "SUCCEEDED" {
        bail!("funding not settled");
    }
    native(
        client,
        directory,
        "issuance",
        "wallet-a",
        &python(
            "deadline=time.monotonic()+40\nwhile time.monotonic()<deadline:\n if api('/balance')['output'].get('http://mint:3338',{}).get('sats')==5000: break\n time.sleep(.5)\nelse: raise RuntimeError('issuance_not_observed')\nprint('issuance observed')",
        ),
    )?;
    balance(client, directory, "funded", "wallet-a", 5000)?;

    let outward = reserve(
        client,
        directory,
        "cashu-out-reserve",
        "wallet-a",
        "wallet-b",
        65536,
    )?;
    capture(
        client,
        directory,
        "cashu-out-send",
        "wallet-a",
        &outward,
        json!([
            "cocod",
            "send",
            "cashu",
            "700",
            "--mint-url",
            "http://mint:3338"
        ]),
        "cashu_token",
    )?;
    deliver(client, directory, "cashu-out-deliver", "wallet-b", &outward)?;
    consume(
        client,
        directory,
        "cashu-out-receive",
        "wallet-b",
        &outward,
        json!([
            "cdk-cli",
            "--work-dir",
            "/wallet/cdk",
            "--unit",
            "sat",
            "--non-interactive",
            "receive",
            "--allow-untrusted",
            "@proofstorm-private-input"
        ]),
        json!({"kind":"argv","index":8}),
    )?;
    cdk_balance(client, directory, "cdk-received", 700)?;
    let back = reserve(
        client,
        directory,
        "cashu-back-reserve",
        "wallet-b",
        "wallet-a",
        65536,
    )?;
    capture(
        client,
        directory,
        "cashu-back-send",
        "wallet-b",
        &back,
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
            "300"
        ]),
        "cashu_token",
    )?;
    deliver(client, directory, "cashu-back-deliver", "wallet-a", &back)?;
    let receiver = "import sys,json,urllib.request\nfrom pathlib import Path\ntoken=sys.stdin.read(); key=Path('/wallet/.cocod/credentials/current/client').read_text().strip()\nr=urllib.request.Request('http://127.0.0.1:62626/receive/cashu',data=json.dumps({'token':token}).encode(),headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})\nwith urllib.request.urlopen(r,timeout=40) as response: result=json.load(response)\nassert 'error' not in result and 'output' in result";
    consume(
        client,
        directory,
        "cashu-back-receive",
        "wallet-a",
        &back,
        json!(["python3", "-c", receiver]),
        json!({"kind":"stdin"}),
    )?;
    private(
        client,
        directory,
        "cdk-native-reconcile",
        "wallet-b",
        json!({"argv":["cdk-cli","--work-dir","/wallet/cdk","--unit","sat","--non-interactive","check-pending"]}),
        0,
    )?;
    cdk_balance(client, directory, "cdk-final", 400)?;
    balance(client, directory, "cocod-final", "wallet-a", 4600)?;
    for (id, source, reference) in [("out", "wallet-a", outward), ("back", "wallet-b", back)] {
        transfer(
            client,
            directory,
            &format!("release-{id}"),
            json!({"transferMethod":"release","component":source,"reference":reference}),
        )?;
    }
    lab::wait_ready(client, "cocod-wallet-instance")?;
    Ok(())
}

fn cdk_balance(client: &mut McpClient, directory: &Path, id: &str, expected: u64) -> Result<()> {
    let receipt = operation(
        client,
        directory,
        "proofstorm_wallet_balance",
        id,
        json!({"wallet":"wallet-b","mint":"mint"}),
    )?;
    if receipt["balance_sat"] != expected
        || receipt["reserved_sat"] != 0
        || receipt["pending_sat"] != 0
        || receipt["pending_spent_sat"] != 0
    {
        bail!("unexpected native CDK balance categories: {receipt}");
    }
    Ok(())
}
