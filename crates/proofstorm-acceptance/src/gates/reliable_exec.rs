//! Focused supervisor contract in real glibc and musl component environments.
use crate::{GateContext, McpClient, json as expect, lab};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::fs;

const INSTANCE: &str = "reliable-exec-instance";
const EXPERIMENT: &str = "reliable-exec-experiment";
const LEASE: &str = "reliable-exec-lease";

fn request(id: &str, command: Value) -> Value {
    let mut value = json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,"lease_id":LEASE,
        "operation_id":id,"idempotency_key":id,"component":"wallet","timeout_seconds":10});
    let Value::Object(fields) = command else {
        panic!("command must be an object")
    };
    value.as_object_mut().unwrap().extend(fields);
    value
}

fn terminal(client: &mut McpClient, root: &std::path::Path, id: &str) -> Result<Value> {
    let result = client.call(
        "proofstorm_operation_wait",
        json!({"operation_id":id,"timeout_seconds":120}),
    )?;
    fs::write(
        root.join(format!("{id}.json")),
        serde_json::to_vec_pretty(&result)?,
    )?;
    if result.get("terminal") != Some(&json!(true)) {
        bail!("operation did not finish: {result}");
    }
    let receipt = result
        .pointer("/artifact/content")
        .ok_or_else(|| anyhow::anyhow!("receipt missing: {result}"))?
        .clone();
    if receipt.get("cleanup_verified") != Some(&json!(true)) {
        bail!("cleanup unverified: {receipt}");
    }
    Ok(receipt)
}

fn execute(
    client: &mut McpClient,
    root: &std::path::Path,
    id: &str,
    command: Value,
) -> Result<Value> {
    client.call("proofstorm_component_exec_live", request(id, command))?;
    terminal(client, root, id)
}

fn wait_for_marker(context: &GateContext, namespace: &str, path: &str) -> Result<()> {
    for _ in 0..50 {
        if context
            .kubectl
            .exec(namespace, "deployment/wallet", &["test", "-s", path])
            .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    bail!("native command did not establish its start marker");
}

pub fn run(context: &GateContext) -> Result<()> {
    let root = context
        .root
        .join("dev/native-execution-runs")
        .join(&context.run_id);
    fs::create_dir_all(&root)?;
    let mut capabilities = crate::EXPERIMENT_CAPABILITIES.to_vec();
    capabilities.extend(["component.exec_live", "action.cancel"]);
    let mut client = context.session(
        &format!("reliable-exec-{}", context.run_id),
        "experiment-agent",
        &capabilities,
    )?;
    let document = json!({"api_version":"proofstorm/v1alpha1","name":"reliable-exec",
        "components":[
            {"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"30.0","config_version":"bitcoin-core/30/v1","control":"laboratory","config":{}},
            {"id":"lightning","kind":"lightning","implementation":"lnd","version":"0.20.0-beta","config_version":"lnd/0.20/v1","control":"laboratory","config":{}},
            {"id":"wallet","kind":"wallet","implementation":"cdk-cli-wallet","version":"0.18.0","config_version":"cdk-cli-wallet/0.18/v1","control":"laboratory","config":{}}
        ],"links":[{"id":"chain-link","kind":"chain_backend","from":"lightning","to":"chain","binding":{"type":"chain","network":"regtest"}}],
        "policy":{"allow":["component.exec_live"],"limits":{"max_components":4,"max_links":4,"max_config_bytes":16384}}});
    client.call(
        "proofstorm_lab_create",
        json!({"draft_id":"reliable-exec","lab":document,"idempotency_key":"create"}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id":"reliable-exec","expected_version":1,"idempotency_key":"publish"}),
    )?;
    client.call("proofstorm_lab_materialize",json!({"instance_id":INSTANCE,"revision_digest":expect::string(&published,"/digest")?,"idempotency_key":"apply"}))?;
    let result = (|| -> Result<()> {
        let ready = lab::wait_ready(&mut client, INSTANCE)?;
        let namespace = expect::string(&ready, "/instance_namespace")?;
        client.call("proofstorm_experiment_create",json!({"experiment_id":EXPERIMENT,"instance_id":INSTANCE,"idempotency_key":"experiment"}))?;
        client.call("proofstorm_lease_acquire",json!({"experiment_id":EXPERIMENT,"lease_id":LEASE,"duration_seconds":900,"max_actions":20,"idempotency_key":"lease"}))?;
        for (id, component, argv) in [
            ("musl-help", "lightning", vec!["lncli", "--version"]),
            ("glibc-help", "wallet", vec!["cdk-cli", "--version"]),
        ] {
            let receipt = execute(
                &mut client,
                &root,
                id,
                json!({"component":component,"argv":argv,"output":{"mode":"public"}}),
            )?;
            if receipt["exit_code"] != 0 || receipt["exit_scope"] != "command" {
                bail!("native help failed: {receipt}");
            }
        }
        let canary = "synthetic-private-preimage-canary";
        context.kubectl.exec_stdin(
            namespace,
            "deployment/wallet",
            &["sh", "-c", "umask 077; cat > /tmp/reliable-private.json"],
            &json!({"status":"SUCCEEDED","value_sat":"700","payment_preimage":canary}).to_string(),
        )?;
        for (id, output) in [
            ("private", json!({"mode":"private"})),
            (
                "projection",
                json!({"mode":"json_fields","fields":["status","value_sat"]}),
            ),
        ] {
            let receipt = execute(
                &mut client,
                &root,
                id,
                json!({"argv":["cat","/tmp/reliable-private.json"],"output":output}),
            )?;
            if receipt.to_string().contains(canary) || receipt["stdout"] != "" {
                bail!("private output escaped");
            }
            if id == "projection"
                && receipt["selected_output"] != json!({"status":"SUCCEEDED","value_sat":"700"})
            {
                bail!("invalid projection: {receipt}");
            }
        }
        let malformed = execute(
            &mut client,
            &root,
            "format-failure",
            json!({"argv":["sh","-c","printf 'PREIMAGE '; cat /tmp/reliable-private.json"],"output":{"mode":"json_fields","fields":["status"]}}),
        )?;
        if malformed["projection_succeeded"] != false || malformed.to_string().contains(canary) {
            bail!("parser failed open");
        }
        let failed = execute(
            &mut client,
            &root,
            "native-exit",
            json!({"argv":["sh","-c","exit 7"]}),
        )?;
        if failed["exit_code"] != 7 {
            bail!("native exit masked: {failed}");
        }
        let noisy = execute(
            &mut client,
            &root,
            "escaped-output",
            json!({"argv":["sh","-c","head -c 40000 /dev/zero; head -c 40000 /dev/zero >&2"],"output":{"mode":"public"}}),
        )?;
        if noisy["exit_code"] != 0 || noisy["output_truncated"] != true {
            bail!("large escaped output lost its receipt");
        }
        expect::within_bytes(&noisy, 32 * 1024, "escaped native artifact")?;
        let timeout = execute(
            &mut client,
            &root,
            "deadline",
            json!({"script":"sleep 120 & wait","timeout_seconds":1}),
        )?;
        if timeout["timed_out"] != true {
            bail!("deadline missing: {timeout}");
        }
        client.call(
            "proofstorm_component_exec_live",
            request(
                "cancel",
                json!({"argv":["sh","-c","printf x > /tmp/reliable-cancel-started; exec sleep 120"],"timeout_seconds":120}),
            ),
        )?;
        wait_for_marker(context, namespace, "/tmp/reliable-cancel-started")?;
        client.call(
            "proofstorm_action_cancel",
            json!({"operation_id":"cancel","idempotency_key":"cancel-owned"}),
        )?;
        let cancelled = terminal(&mut client, &root, "cancel")?;
        if cancelled["cancelled"] != true {
            bail!("cancellation not established: {cancelled}");
        }
        let once = request(
            "once",
            json!({"script":"printf x >> /tmp/reliable-once; sleep 20", "timeout_seconds":30}),
        );
        let first = client.call("proofstorm_component_exec_live", once.clone())?;
        let replay = client.call("proofstorm_component_exec_live", once)?;
        if first["resource_name"] != replay["resource_name"] {
            bail!("replay identity changed");
        }
        wait_for_marker(context, namespace, "/tmp/reliable-once")?;
        context
            .kubectl
            .rollout_restart("proofstorm-system", "deployment/proofstormd")?;
        terminal(&mut client, &root, "once")?;
        let count = execute(
            &mut client,
            &root,
            "once-count",
            json!({"argv":["wc","-c","/tmp/reliable-once"],"output":{"mode":"public"}}),
        )?;
        if !expect::string(&count, "/stdout")?.trim().starts_with("1 ") {
            bail!("native command replayed: {count}");
        }
        client.call(
            "proofstorm_lease_release",
            json!({"lease_id":LEASE,"idempotency_key":"release"}),
        )?;
        client.call(
            "proofstorm_experiment_close",
            json!({"experiment_id":EXPERIMENT,"idempotency_key":"close-experiment"}),
        )?;
        let evidence=client.call("proofstorm_artifact_export",json!({"experiment_id":EXPERIMENT,"include_content":true,"artifact_operation_ids":["private","projection","format-failure","native-exit","deadline","cancel","once"]}))?;
        if evidence.to_string().contains(canary) {
            bail!("evidence export disclosed private output");
        }
        fs::write(
            root.join("evidence.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        Ok(())
    })();
    let _ = client.call(
        "proofstorm_lease_release",
        json!({"lease_id":LEASE,"idempotency_key":"release"}),
    );
    let _ = client.call(
        "proofstorm_experiment_close",
        json!({"experiment_id":EXPERIMENT,"idempotency_key":"close-experiment"}),
    );
    client.call("proofstorm_lab_close", json!({"instance_id":INSTANCE}))?;
    let closed = lab::wait_closed(&mut client, INSTANCE)?;
    fs::write(
        root.join("closed.json"),
        serde_json::to_vec_pretty(&closed)?,
    )?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("teardown unverified");
    }
    fs::write(
        root.join("outcome.json"),
        serde_json::to_vec_pretty(
            &json!({"passed":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}),
        )?,
    )?;
    result?;
    println!(
        "Reliable native execution passed; evidence: {}",
        root.display()
    );
    Ok(())
}
