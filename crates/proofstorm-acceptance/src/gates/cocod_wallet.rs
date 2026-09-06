//! Cocod deterministic vertical slice. Native daemon/CLI, private state and independent money evidence.
use crate::{GateContext, McpClient, json as expect, lab};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{fs, path::Path};

const INSTANCE: &str = "cocod-wallet-instance";
const EXPERIMENT: &str = "cocod-wallet-experiment";
const LEASE: &str = "cocod-wallet-lease";

pub(super) fn relay_invoice(receipt: &Value, amount_sat: u64) -> Result<&str> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    if receipt["exit_code"] != 0
        || receipt["cleanup_verified"] != true
        || receipt["streams_complete"] != true
        || receipt["output_truncated"] != false
        || receipt["projection_succeeded"] != true
        || receipt["stdout"] != ""
        || receipt["stderr"] != ""
        || receipt["selected_output"]["amount_msat"] != amount_sat * 1000
        || receipt["selected_output"]["currency"] != "bcrt"
        || receipt["selected_output"]["expires_at_unix"]
            .as_u64()
            .is_none_or(|expiry| expiry <= now)
    {
        bail!("invoice relay contract failed");
    }
    expect::string(receipt, "/selected_output/payment_request")
}

fn document() -> Value {
    json!({
        "api_version":"proofstorm/v1alpha1", "name":"cocod-wallet-checkpoint",
        "components":[
            {"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"30.0","config_version":"bitcoin-core/30/v1","control":"laboratory","config":{}},
            {"id":"mint-lnd","kind":"lightning","implementation":"lnd","version":"0.20.0-beta","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
            {"id":"payer-lnd","kind":"lightning","implementation":"lnd","version":"0.20.0-beta","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
            {"id":"mint","kind":"mint","implementation":"cdk","version":"0.18.0","config_version":"cdk-mintd/0.18/v1","control":"target","config":{"input_fee_ppk":0}},
            {"id":"wallet-a","kind":"wallet","implementation":"cocod-wallet","version":"0.0.17-dev.44e5101c","config_version":"cocod-wallet/0.0.17/v1","control":"laboratory","config":{}},
            {"id":"wallet-b","kind":"wallet","implementation":"cocod-wallet","version":"0.0.17-dev.44e5101c","config_version":"cocod-wallet/0.0.17/v1","control":"laboratory","config":{}}
        ],
        "links":[
            {"id":"mint-chain","kind":"chain_backend","from":"mint-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"payer-chain","kind":"chain_backend","from":"payer-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
            {"id":"mint-backend","kind":"payment_backend","from":"mint","to":"mint-lnd","binding":{"type":"payment","method":"bolt11","unit":"sat"}}
        ],
        "policy":{"allow":["component.exec_live","component.control"],"limits":{"max_components":8,"max_links":16,"max_config_bytes":16384}}
    })
}

fn scoped(operation: &str, parameters: Value) -> Value {
    let Value::Object(mut request) = parameters else {
        panic!("parameters must be an object")
    };
    request.extend(
        json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,"lease_id":LEASE,
        "operation_id":operation,"idempotency_key":operation})
        .as_object()
        .expect("scope")
        .clone(),
    );
    Value::Object(request)
}

fn save(directory: &Path, name: &str, value: &Value) -> Result<()> {
    fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value)?,
    )?;
    Ok(())
}

pub(super) fn operation(
    client: &mut McpClient,
    directory: &Path,
    tool: &str,
    id: &str,
    args: Value,
) -> Result<Value> {
    client.call(tool, scoped(id, args))?;
    let result = match lab::wait_operation(client, id, 60) {
        Ok(result) => result,
        Err(error) => {
            if let Ok(failed) =
                client.call("proofstorm_operation_status", json!({"operation_id": id}))
            {
                save(directory, id, &failed)?;
            }
            return Err(error);
        }
    };
    save(directory, id, &result)?;
    Ok(lab::artifact_content(&result)?.clone())
}

pub(super) fn native(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    wallet: &str,
    script: &str,
) -> Result<String> {
    let result = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        id,
        json!({"component":wallet,"script":format!("umask 077; {script}"),"timeout_seconds":60,"output":{"mode":"public"}}),
    )?;
    if result.get("exit_code") != Some(&json!(0))
        || result.get("timed_out") != Some(&json!(false))
        || result.get("cleanup_verified") != Some(&json!(true))
        || result.get("streams_complete") != Some(&json!(true))
        || result.get("output_truncated") != Some(&json!(false))
    {
        bail!("native command {id} failed: {result}");
    }
    Ok(expect::string(&result, "/stdout")?.trim().into())
}

const API: &str = r"import json, urllib.request, urllib.error, hashlib, secrets, time
from pathlib import Path
root=Path('/wallet/.cocod')
def api(path, body=None):
 credential=(root/'credentials/current/client').read_text().strip()
 request=urllib.request.Request('http://127.0.0.1:62626'+path,
  data=None if body is None else json.dumps(body).encode(),
  headers={'Authorization':'Bearer '+credential,'Content-Type':'application/json'})
 with urllib.request.urlopen(request,timeout=20) as response: return json.load(response)
def session(expected):
 deadline=time.monotonic()+40
 while time.monotonic()<deadline:
  status=api('/v1/status')
  if status['cocoSession']['state']==expected: return status
  if status['cocoSession']['state']=='failed': raise RuntimeError('native_session_failed')
  time.sleep(.25)
 raise RuntimeError('native_session_deadline')
";

pub(super) fn python(code: &str) -> String {
    format!("python3 - <<'PY'\n{API}\n{code}\nPY")
}

pub(super) fn private(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    wallet: &str,
    args: Value,
    exit: i64,
) -> Result<Value> {
    let mut args = args;
    args["component"] = json!(wallet);
    args["timeout_seconds"] = json!(60);
    let receipt = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        id,
        args,
    )?;
    if receipt["exit_code"] != exit
        || receipt["cleanup_verified"] != true
        || receipt["timed_out"] != false
        || receipt["streams_complete"] != true
        || receipt["output_truncated"] != false
        || receipt["stdout"] != ""
        || receipt["stderr"] != ""
    {
        bail!("private execution {id} lacked expected exit and cleanup: {receipt}");
    }
    Ok(receipt)
}

pub(super) fn balance(
    client: &mut McpClient,
    directory: &Path,
    id: &str,
    wallet: &str,
    expected: u64,
) -> Result<()> {
    let receipt = operation(
        client,
        directory,
        "proofstorm_wallet_balance",
        id,
        json!({"wallet":wallet,"mint":"mint"}),
    )?;
    if receipt["balance_sat"] != expected
        || receipt["reserved_sat"] != 0
        || receipt["inflight_sat"] != 0
        || receipt["total_ready_sat"] != expected
    {
        bail!("unexpected passive balance: {receipt}");
    }
    Ok(())
}

pub(super) fn restart(
    context: &GateContext,
    client: &mut McpClient,
    directory: &Path,
    namespace: &str,
    wallet: &str,
    id: &str,
) -> Result<()> {
    let uid = || {
        context.kubectl.get_json(&[
            "get",
            "pods",
            "-n",
            namespace,
            "-l",
            &format!("proofstorm.dev/component={wallet}"),
            "--field-selector=status.phase=Running",
        ])
    };
    let before = uid()?;
    operation(
        client,
        directory,
        "proofstorm_component_restart",
        id,
        json!({"component":wallet}),
    )?;
    lab::wait_ready(client, INSTANCE)?;
    let after = uid()?;
    if after["items"].as_array().map(Vec::len) != Some(1)
        || before["items"][0]["metadata"]["uid"] == after["items"][0]["metadata"]["uid"]
    {
        bail!("restart did not replace the exclusive wallet owner");
    }
    save(
        directory,
        &format!("{id}-owners"),
        &json!({"before_uid":before["items"][0]["metadata"]["uid"],"after_uid":after["items"][0]["metadata"]["uid"],"running_owners":1}),
    )
}

pub(super) fn start_session(
    client: &mut McpClient,
    directory: &Path,
    wallet: &str,
    id: &str,
) -> Result<()> {
    private(
        client,
        directory,
        id,
        wallet,
        json!({"script":python("api('/v1/admin/session/start',{'passphrase':Path('/wallet/session.passphrase').read_text()}); session('running')")}),
        0,
    )?;
    Ok(())
}

fn identity(client: &mut McpClient, directory: &Path, wallet: &str, id: &str) -> Result<String> {
    native(
        client,
        directory,
        id,
        wallet,
        &python(
            "r=api('/v1/admin/wallet/recovery-material',{'passphrase':Path('/wallet/session.passphrase').read_text()}); print(hashlib.sha256(r['mnemonic'].encode()).hexdigest())",
        ),
    )
}

fn exercise(
    context: &GateContext,
    client: &mut McpClient,
    directory: &Path,
    namespace: &str,
) -> Result<()> {
    native(client, directory, "help", "wallet-a", "cocod --help")?;
    for wallet in ["wallet-a", "wallet-b"] {
        native(
            client,
            directory,
            &format!("uninitialized-{wallet}"),
            wallet,
            &python(
                r"
h=json.load(urllib.request.urlopen('http://127.0.0.1:62626/health',timeout=2)); assert h['status']=='ok'
s=api('/v1/status'); assert s['wallet'] is None and s['cocoSession']['state']=='stopped'
try: urllib.request.urlopen('http://127.0.0.1:62626/v1/status',timeout=2)
except urllib.error.HTTPError as error: assert error.code==401
else: raise AssertionError('unauthenticated administrative read accepted')
assert (root/'credentials/current/client').stat().st_mode & 0o777 == 0o600
print(json.dumps({'healthy':True,'initialized':False,'unauthenticated_status':401}))
",
            ),
        )?;
        private(
            client,
            directory,
            &format!("initialize-{wallet}"),
            wallet,
            json!({"script":python(r"
p=Path('/wallet/session.passphrase'); p.write_text(secrets.token_urlsafe(32)); p.chmod(0o600)
r=api('/v1/admin/wallet/initialize',{'passphrase':p.read_text()})
assert r['generatedMnemonic'] and r['status']['cocoSession']['state']=='stopped'
config=root/'config.json'; settings=json.loads(config.read_text()); assert settings['encrypted'] is True
settings['mintUrl']='http://mint:3338'; config.write_text(json.dumps(settings)); config.chmod(0o600)
")}),
            0,
        )?;
        restart(
            context,
            client,
            directory,
            namespace,
            wallet,
            &format!("configured-{wallet}"),
        )?;
        native(
            client,
            directory,
            &format!("locked-{wallet}"),
            wallet,
            &python(
                "s=api('/v1/status'); assert s['seedAccess']['state']=='locked' and s['cocoSession']['state']=='stopped'; print('protected session remained stopped')",
            ),
        )?;
        start_session(client, directory, wallet, &format!("start-{wallet}"))?;
        balance(client, directory, &format!("empty-{wallet}"), wallet, 0)?;
    }
    let identity_a = identity(client, directory, "wallet-a", "identity-a")?;
    let identity_b = identity(client, directory, "wallet-b", "identity-b")?;
    if identity_a.len() != 64 || identity_a == identity_b {
        bail!("wallet identities are not isolated");
    }
    private(
        client,
        directory,
        "second-owner-refused",
        "wallet-a",
        json!({"argv":["env","-u","COCOD_URL","COCOD_LISTEN_PORT=62627","cocod","daemon"]}),
        1,
    )?;
    private(
        client,
        directory,
        "explicit-client-no-autostart",
        "wallet-a",
        json!({"argv":["cocod","--url","http://127.0.0.1:62625","health"]}),
        1,
    )?;
    native(
        client,
        directory,
        "owner-still-healthy",
        "wallet-a",
        &python("session('running'); print('original session running')"),
    )?;
    client.call_refused(
        "proofstorm_wallet_initialize",
        scoped(
            "unsupported-initialize",
            json!({"wallet":"wallet-a","mint":"mint"}),
        ),
        "runtime_control_unsupported",
    )?;
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
    if paid["exit_code"] != 0
        || paid["cleanup_verified"] != true
        || paid["selected_output"] != json!({"status":"SUCCEEDED","value_sat":"5000"})
    {
        bail!("funding payer did not settle: {paid}");
    }
    native(
        client,
        directory,
        "issuance",
        "wallet-a",
        &python(
            "deadline=time.monotonic()+40\nwhile time.monotonic()<deadline:\n b=api('/balance')['output'].get('http://mint:3338',{}).get('sats')\n if b==5000: break\n time.sleep(.5)\nelse: raise RuntimeError('issuance_not_observed')\nprint(json.dumps({'native_ready_total_sat':b}))",
        ),
    )?;
    balance(client, directory, "funded", "wallet-a", 5000)?;
    for (id, amount, remaining) in [
        ("first-payment", 700, 4300),
        ("after-restart-payment", 300, 4000),
    ] {
        if id == "after-restart-payment" {
            restart(
                context,
                client,
                directory,
                namespace,
                "wallet-a",
                "money-restart",
            )?;
            native(
                client,
                directory,
                "restart-session-locked",
                "wallet-a",
                &python(
                    "s=api('/v1/status'); assert s['seedAccess']['state']=='locked' and s['cocoSession']['state']=='stopped'; print('explicit unlock required')",
                ),
            )?;
            balance(
                client,
                directory,
                "locked-passive-balance",
                "wallet-a",
                4300,
            )?;
            start_session(client, directory, "wallet-a", "restart-unlock")?;
            if identity(client, directory, "wallet-a", "restart-identity")? != identity_a {
                bail!("wallet identity changed on restart");
            }
        }
        let invoice = operation(
            client,
            directory,
            "proofstorm_component_exec_live",
            &format!("{id}-invoice"),
            json!({"component":"payer-lnd","argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","addinvoice",format!("--amt={amount}")],"timeout_seconds":60,"output":{"mode":"lnd_invoice"}}),
        )?;
        let request = relay_invoice(&invoice, amount)?;
        private(
            client,
            directory,
            id,
            "wallet-a",
            json!({"argv":["cocod","send","bolt11",request,"--mint-url","http://mint:3338"]}),
            0,
        )?;
        let hash = expect::string(&invoice, "/selected_output/payment_hash")?;
        let settled = operation(
            client,
            directory,
            "proofstorm_component_exec_live",
            &format!("{id}-recipient"),
            json!({"component":"payer-lnd","argv":["lncli","--lnddir=/home/lnd/.lnd","--network=regtest","--rpcserver=127.0.0.1:10009","lookupinvoice","--rhash",hash],"timeout_seconds":60,"output":{"mode":"json_fields","fields":["state","settled"]}}),
        )?;
        if settled["exit_code"] != 0
            || settled["cleanup_verified"] != true
            || settled["projection_succeeded"] != true
            || settled["selected_output"] != json!({"state":"SETTLED","settled":true})
        {
            bail!("recipient settlement not observed");
        }
        balance(
            client,
            directory,
            &format!("{id}-balance"),
            "wallet-a",
            remaining,
        )?;
        native(
            client,
            directory,
            &format!("{id}-native-balance"),
            "wallet-a",
            &python(&format!(
                "b=api('/balance')['output']['http://mint:3338']['sats']; assert b=={remaining}; print(json.dumps({{'native_ready_total_sat':b}}))"
            )),
        )?;
        balance(client, directory, &format!("{id}-isolated"), "wallet-b", 0)?;
    }
    private(
        client,
        directory,
        "session-stop",
        "wallet-a",
        json!({"argv":["cocod","session","stop"]}),
        0,
    )?;
    native(
        client,
        directory,
        "stopped-session-healthy",
        "wallet-a",
        &python(
            "session('stopped'); assert json.load(urllib.request.urlopen('http://127.0.0.1:62626/health'))['status']=='ok'; print('daemon healthy with session stopped')",
        ),
    )?;
    balance(
        client,
        directory,
        "stopped-session-passive",
        "wallet-a",
        4000,
    )?;
    start_session(client, directory, "wallet-a", "session-restart")?;
    balance(client, directory, "final", "wallet-a", 4000)?;
    Ok(())
}

fn projection_checkpoint(client: &mut McpClient, directory: &Path) -> Result<()> {
    for (id, argv, fields, expected) in [
        (
            "project-health",
            json!(["cocod", "health"]),
            json!(["status"]),
            json!({"status":"ok"}),
        ),
        (
            "project-uninitialized",
            json!(["cocod", "status"]),
            json!([
                "seedAccess.state",
                "seedAccess.requiresPassphrase",
                "cocoSession.state"
            ]),
            json!({"seedAccess.state":null,"seedAccess.requiresPassphrase":null,"cocoSession.state":"stopped"}),
        ),
    ] {
        let receipt = operation(
            client,
            directory,
            "proofstorm_component_exec_live",
            id,
            json!({"component":"wallet-a","argv":argv,"timeout_seconds":30,"output":{"mode":"json_fields","fields":fields}}),
        )?;
        check_projection(&receipt, 0, &expected)?;
    }
    private(
        client,
        directory,
        "project-initialize",
        "wallet-a",
        json!({"script":
        "python3 - <<'PY'\nimport os,secrets\nfrom pathlib import Path\np=Path('/wallet/session.passphrase'); p.write_text(secrets.token_urlsafe(32)); p.chmod(0o600)\nos.execvp('cocod',['cocod','wallet','initialize','--passphrase',p.read_text()])\nPY"}),
        0,
    )?;
    let receipt = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "project-locked",
        json!({"component":"wallet-a","argv":["cocod","status"],"timeout_seconds":30,"output":{"mode":"json_fields","fields":["seedAccess.state","seedAccess.requiresPassphrase","cocoSession.state"]}}),
    )?;
    check_projection(
        &receipt,
        0,
        &json!({"seedAccess.state":"locked","seedAccess.requiresPassphrase":true,"cocoSession.state":"stopped"}),
    )?;
    let failed = operation(
        client,
        directory,
        "proofstorm_component_exec_live",
        "project-fail-closed",
        json!({"component":"wallet-a","script":"printf '%s' '{\"cocoSession\":{\"state\":\"unknown-canary\",\"lastFailure\":{\"message\":\"private-canary\"}}}'; exit 3","timeout_seconds":30,"output":{"mode":"json_fields","fields":["cocoSession.state"]}}),
    )?;
    if failed["projection_succeeded"] != false
        || failed["projection_error"] != "output_field_type_invalid"
    {
        bail!("unknown lifecycle state did not fail closed");
    }
    check_projection(&failed, 3, &Value::Null)?;
    Ok(())
}

fn check_projection(receipt: &Value, exit: i64, selected: &Value) -> Result<()> {
    if receipt["exit_code"] != exit
        || receipt["cleanup_verified"] != true
        || receipt["streams_complete"] != true
        || receipt["output_truncated"] != false
        || receipt["stdout"] != ""
        || receipt["stderr"] != ""
        || receipt["selected_output"] != *selected
        || (exit == 0 && receipt["projection_succeeded"] != true)
    {
        bail!("lifecycle projection receipt invalid: {receipt}");
    }
    Ok(())
}

pub fn run(context: &GateContext) -> Result<()> {
    run_scoped(context, false, false, false)
}

pub fn run_projection(context: &GateContext) -> Result<()> {
    run_scoped(context, true, false, false)
}

pub fn run_transfer(context: &GateContext) -> Result<()> {
    run_scoped(context, false, true, false)
}

pub fn run_handoff(context: &GateContext) -> Result<()> {
    run_scoped(context, false, true, true)
}

fn run_scoped(
    context: &GateContext,
    projection_only: bool,
    transfer: bool,
    handoff: bool,
) -> Result<()> {
    let directory = context
        .root
        .join("dev/wallet-integration-runs")
        .join(&context.run_id);
    fs::create_dir_all(&directory)?;
    let mut capabilities = crate::EXPERIMENT_CAPABILITIES.to_vec();
    capabilities.extend(["component.exec_live", "component.control"]);
    let mut client = context.session(
        &format!("cocod-wallet-{}", context.run_id),
        "experiment-agent",
        &capabilities,
    )?;
    let mut document = document();
    if transfer {
        document["components"][5] = json!({"id":"wallet-b","kind":"wallet","implementation":"cdk-cli-wallet","version":"0.18.0","config_version":"cdk-cli-wallet/0.18/v1","control":"laboratory","config":{}});
    }
    if projection_only {
        document["components"]
            .as_array_mut()
            .expect("components")
            .retain(|component| component["id"] == "wallet-a");
        document["links"] = json!([]);
    }
    client.call(
        "proofstorm_lab_create",
        json!({"draft_id":"cocod-wallet","lab":document,"idempotency_key":"create"}),
    )?;
    let published=client.call("proofstorm_lab_publish",json!({"draft_id":"cocod-wallet","expected_version":1,"idempotency_key":"publish","include_revision":true}))?;
    save(&directory, "published", &published)?;
    let lock = lab::lock_entry(&published, "cocod-wallet")?;
    if lock.pointer("/build_provenance/commit_sha")
        != Some(&json!("44e5101cbea370132af6e68f88e01b47e39431c4"))
    {
        bail!("cocod provenance lost from lock");
    }
    client.call("proofstorm_lab_materialize",json!({"instance_id":INSTANCE,"revision_digest":expect::string(&published,"/digest")?,"idempotency_key":"materialize"}))?;
    let result = (|| -> Result<()> {
        let ready = lab::wait_ready(&mut client, INSTANCE)?;
        save(&directory, "ready", &ready)?;
        client.call("proofstorm_experiment_create",json!({"experiment_id":EXPERIMENT,"instance_id":INSTANCE,"idempotency_key":"experiment"}))?;
        client.call("proofstorm_lease_acquire",json!({"experiment_id":EXPERIMENT,"lease_id":LEASE,"duration_seconds":1200,"max_actions":64,"idempotency_key":"lease"}))?;
        if transfer {
            super::private_transfer::exercise(
                context,
                &mut client,
                &directory,
                expect::string(&ready, "/instance_namespace")?,
            )?;
            if handoff {
                super::private_handoff::exercise(context, &mut client, &directory)?;
            }
            return Ok(());
        }
        if projection_only {
            return projection_checkpoint(&mut client, &directory);
        }
        exercise(
            context,
            &mut client,
            &directory,
            expect::string(&ready, "/instance_namespace")?,
        )
    })();
    let _ = client.call(
        "proofstorm_lease_release",
        json!({"lease_id":LEASE,"idempotency_key":"release"}),
    );
    let _ = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id":EXPERIMENT,"idempotency_key":"close-experiment"}),
    );
    let evidence = client.call(
        "proofstorm_artifact_export",
        json!({"experiment_id":EXPERIMENT,"include_content":true}),
    );
    if let Ok(value) = &evidence {
        save(&directory, "evidence", value)?;
    }
    save(
        &directory,
        "outcome",
        &json!({"passed":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}),
    )?;
    client.call("proofstorm_lab_close", json!({"instance_id":INSTANCE}))?;
    let closed = lab::wait_closed(&mut client, INSTANCE)?;
    save(&directory, "closed", &closed)?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("teardown unverified");
    }
    result.context(format!(
        "cocod checkpoint failed; evidence: {}",
        directory.display()
    ))?;
    evidence.context("required evidence export failed")?;
    println!(
        "Cocod wallet checkpoint passed; evidence: {}",
        directory.display()
    );
    Ok(())
}
