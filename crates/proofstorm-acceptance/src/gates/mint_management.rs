//! Native management CLI, mandatory TLS, loopback isolation and restart semantics.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{GateContext, McpClient, cell, json as expect};

const INSTANCE: &str = "mint-management";
const EXPERIMENT: &str = "mint-management-experiment";
const SESSION: &str = "mint-management-session";
const MINTS: &[&str] = &["cdk", "cdk-ldk", "cdk-bdk", "nutshell"];

fn document() -> Value {
    let mut components = vec![
        json!({"id":"chain","kind":"bitcoin","implementation":"bitcoin-core","version":"31.1","config_version":"bitcoin-core/31/v1","control":"cell","config":{}}),
        json!({"id":"lightning","kind":"lightning","implementation":"lnd","version":"0.21.3-beta","config_version":"lnd/0.20/v1","control":"cell","config":{}}),
    ];
    let mut links = vec![
        json!({"id":"lightning-chain","kind":"chain_backend","from":"lightning","to":"chain","binding":{"type":"chain","network":"regtest"}}),
    ];
    for implementation in MINTS {
        let version = if *implementation == "nutshell" {
            "0.20.3"
        } else {
            "0.18.0"
        };
        let config_version = match *implementation {
            "cdk" => "cdk-mintd/0.18/v1",
            "cdk-ldk" => "cdk-mintd-ldk/0.18/v1",
            "cdk-bdk" => "cdk-mintd-bdk/0.18/v1",
            _ => "nutshell-mint/0.20/v1",
        };
        components.push(json!({"id":implementation,"kind":"mint","implementation":implementation,"version":version,"config_version":config_version,"control":"target","config":{"motd":"authored-management-motd"}}));
        let binding = if matches!(*implementation, "cdk-ldk" | "cdk-bdk") {
            json!({"id":format!("{implementation}-chain"),"kind":"chain_backend","from":implementation,"to":"chain","binding":{"type":"chain","network":"regtest"}})
        } else {
            json!({"id":format!("{implementation}-lightning"),"kind":"payment_backend","from":implementation,"to":"lightning","binding":{"type":"payment","method":"bolt11","unit":"sat"}})
        };
        links.push(binding);
    }
    json!({"api_version":"proofstorm/v1alpha1","name":"mint-management","components":components,"links":links,"policy":{"allow":["component.exec_live","component.control"],"limits":{"max_components":8,"max_links":8,"max_config_bytes":32768}}})
}

fn cli(component: &str) -> Vec<String> {
    let argv = if component == "nutshell" {
        vec![
            "mint-cli",
            "--host",
            "127.0.0.1",
            "--port",
            "8086",
            "--ca-cert-path",
            "/management-client/tls/ca.pem",
            "--client-cert-path",
            "/management-client/tls/client.pem",
            "--client-key-path",
            "/management-client/tls/client.key",
        ]
    } else {
        vec![
            "cdk-mint-cli",
            "--addr",
            "https://127.0.0.1:8086",
            "--work-dir",
            "/management-client",
        ]
    };
    argv.into_iter().map(str::to_owned).collect()
}

fn execute(client: &mut McpClient, component: &str, id: &str, mut command: Value) -> Result<Value> {
    command.as_object_mut().unwrap().extend(json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,"session_id":SESSION,"operation_id":id,"idempotency_key":id,"component":component,"timeout_seconds":15,"output":{"mode":"public"}}).as_object().unwrap().clone());
    client.call("component_exec_live", command)?;
    let finished = client.call(
        "operation_wait",
        json!({"operation_id":id,"timeout_seconds":120}),
    )?;
    let content = cell::artifact_content(&finished)?.clone();
    if finished["phase"] != "succeeded" || content["cleanup_verified"] != true {
        bail!("management execution failed: {finished}");
    }
    let encoded = serde_json::to_string(&finished)?;
    if encoded.contains("BEGIN PRIVATE KEY") || encoded.contains("BEGIN CERTIFICATE") {
        bail!("management credential content leaked to the operation journal");
    }
    Ok(content)
}

fn run_cli(client: &mut McpClient, component: &str, id: &str, args: &[&str]) -> Result<Value> {
    let mut argv = cli(component);
    argv.extend(args.iter().map(|s| (*s).to_owned()));
    let result = execute(client, component, id, json!({"argv":argv}))?;
    if result["exit_code"] != 0 {
        bail!("native management CLI failed: {result}");
    }
    Ok(result)
}

fn verify_motd(client: &mut McpClient, component: &str, id: &str, motd: &str) -> Result<()> {
    let command = if component == "nutshell" {
        json!({"argv":["python3","-c","import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:3338/v1/info',timeout=3).read().decode())"]})
    } else {
        json!({"argv":["wget","-q","-T","3","-O","-","http://127.0.0.1:3338/v1/info"]})
    };
    let result = execute(client, component, id, command)?;
    let info: Value = serde_json::from_str(expect::string(&result, "/stdout")?)?;
    if result["exit_code"] != 0 || info["motd"] != motd {
        bail!("management change did not reach public mint info: {result}");
    }
    Ok(())
}

fn secret_fingerprint(context: &GateContext, namespace: &str, component: &str) -> Result<Vec<u8>> {
    let secret = context.kubectl.get_json(&[
        "get",
        "secret",
        &format!("{component}-management-tls"),
        "-n",
        namespace,
    ])?;
    // Never print or save the Secret; retain only a digest for restart comparison.
    Ok(Sha256::digest(serde_json::to_vec(&secret["data"])?).to_vec())
}

pub fn run(context: &GateContext) -> Result<()> {
    let mut capabilities = crate::EXPERIMENT_CAPABILITIES.to_vec();
    capabilities.extend(["component.exec_live", "component.control"]);
    let mut client = context.session(
        &format!("management-{}", context.run_id),
        "management-agent",
        &capabilities,
    )?;
    client.call(
        "cell_create",
        json!({"draft_id":INSTANCE,"cell":document(),"idempotency_key":"create"}),
    )?;
    let published = client.call(
        "cell_publish",
        json!({"draft_id":INSTANCE,"expected_version":1,"idempotency_key":"publish"}),
    )?;
    client.call("cell_materialize", json!({"instance_id":INSTANCE,"revision_digest":expect::string(&published,"/digest")?,"idempotency_key":"materialize"}))?;
    let result = (|| -> Result<()> {
        let ready = cell::wait_ready(&mut client, INSTANCE)?;
        let namespace = expect::string(&ready, "/instance_namespace")?;
        client.call("experiment_create", json!({"experiment_id":EXPERIMENT,"instance_id":INSTANCE,"idempotency_key":"experiment"}))?;
        client.call(
            "session_start",
            json!({"experiment_id":EXPERIMENT,"session_id":SESSION,"idempotency_key":"session"}),
        )?;
        for component in MINTS {
            println!("Checking {component}: native management, TLS and restart persistence");
            let fingerprint = secret_fingerprint(context, namespace, component)?;
            run_cli(
                &mut client,
                component,
                &format!("{component}-info"),
                &["get-info"],
            )?;
            let mutation = if *component == "nutshell" {
                vec!["update", "motd", "rpc-management-motd"]
            } else {
                vec!["update-motd", "rpc-management-motd"]
            };
            run_cli(
                &mut client,
                component,
                &format!("{component}-update"),
                &mutation,
            )?;
            verify_motd(
                &mut client,
                component,
                &format!("{component}-verify"),
                "rpc-management-motd",
            )?;

            let insecure = if *component == "nutshell" {
                json!({"argv":["python3","-c","import grpc; from cashu.mint.management_rpc.protos import management_pb2 as p,management_pb2_grpc as g; g.MintStub(grpc.insecure_channel('127.0.0.1:8086')).GetInfo(p.GetInfoRequest(),timeout=2)"]})
            } else {
                json!({"argv":["cdk-mint-cli","--addr","http://127.0.0.1:8086","--work-dir","/tmp/no-management-identity","get-info"]})
            };
            if execute(
                &mut client,
                component,
                &format!("{component}-insecure"),
                insecure,
            )?["exit_code"]
                == 0
            {
                bail!("{component} accepted unauthenticated plaintext management");
            }

            // A server identity has serverAuth only. Even though the CA is trusted,
            // presenting that certificate as a client must fail mutual TLS.
            let wrong_identity = if *component == "nutshell" {
                json!({"argv":["python3","-c","import pathlib,grpc; from cashu.mint.management_rpc.protos import management_pb2 as p,management_pb2_grpc as g; t=pathlib.Path('/management-server/tls'); c=grpc.ssl_channel_credentials((t/'ca.pem').read_bytes(),(t/'server.key').read_bytes(),(t/'server.pem').read_bytes()); g.MintStub(grpc.secure_channel('127.0.0.1:8086',c)).GetInfo(p.GetInfoRequest(),timeout=2)"]})
            } else {
                json!({"script":r#"set -eu
dir=$(mktemp -d /tmp/management-wrong-identity.XXXXXXXX)
trap 'rm -rf "$dir"' EXIT
mkdir "$dir/tls"
cp /management-server/tls/ca.pem "$dir/tls/ca.pem"
cp /management-server/tls/server.pem "$dir/tls/client.pem"
cp /management-server/tls/server.key "$dir/tls/client.key"
cdk-mint-cli --addr https://127.0.0.1:8086 --work-dir "$dir" get-info
"#})
            };
            if execute(
                &mut client,
                component,
                &format!("{component}-wrong-identity"),
                wrong_identity,
            )?["exit_code"]
                == 0
            {
                bail!("{component} accepted a certificate without client authentication usage");
            }
            if *component == "nutshell" {
                let missing_identity = json!({"argv":["python3","-c","import pathlib,grpc; from cashu.mint.management_rpc.protos import management_pb2 as p,management_pb2_grpc as g; c=grpc.ssl_channel_credentials(pathlib.Path('/management-client/tls/ca.pem').read_bytes()); g.MintStub(grpc.secure_channel('127.0.0.1:8086',c)).GetInfo(p.GetInfoRequest(),timeout=2)"]});
                if execute(
                    &mut client,
                    component,
                    "nutshell-no-client-certificate",
                    missing_identity,
                )?["exit_code"]
                    == 0
                {
                    bail!("Nutshell accepted TLS without a client certificate");
                }
            }

            let restart = format!("{component}-restart");
            client.call("component_restart", json!({"instance_id":INSTANCE,"experiment_id":EXPERIMENT,"session_id":SESSION,"operation_id":restart,"idempotency_key":restart,"component":component}))?;
            cell::wait_succeeded(&mut client, &restart)?;
            cell::wait_ready(&mut client, INSTANCE)?;
            if fingerprint != secret_fingerprint(context, namespace, component)? {
                bail!("restart rotated management credentials");
            }
            let expected = if *component == "nutshell" {
                "authored-management-motd"
            } else {
                "rpc-management-motd"
            };
            verify_motd(
                &mut client,
                component,
                &format!("{component}-restart-verify"),
                expected,
            )?;
        }
        // Same-cell traffic is allowed by network policy. A pod-IP refusal proves
        // the management listener itself is bound to loopback, not merely hidden by DNS.
        let pods = context
            .kubectl
            .get_json(&["get", "pods", "-n", namespace])?;
        for component in MINTS {
            let ip = expect::array(&pods, "/items")?
                .iter()
                .find(|pod| pod["metadata"]["labels"]["proofstorm.dev/component"] == *component)
                .and_then(|pod| pod.pointer("/status/podIP"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("mint pod IP missing"))?;
            let source = if *component == "nutshell" {
                "cdk"
            } else {
                "nutshell"
            };
            let command = if source == "nutshell" {
                json!({"argv":["python3","-c","import socket,sys; s=socket.create_connection((sys.argv[1],8086),timeout=2); s.close()",ip]})
            } else {
                json!({"argv":["nc","-z","-w","2",ip,"8086"]})
            };
            // Positive control: prove the same source can reach this pod's
            // public HTTP port before treating an RPC refusal as isolation.
            let public_command = if source == "nutshell" {
                json!({"argv":["python3","-c","import socket,sys; socket.create_connection((sys.argv[1],3338),timeout=2).close()",ip]})
            } else {
                json!({"argv":["nc","-z","-w","2",ip,"3338"]})
            };
            if execute(
                &mut client,
                source,
                &format!("{component}-pod-public"),
                public_command,
            )?["exit_code"]
                != 0
            {
                bail!("public mint port unreachable; management isolation is unproven");
            }
            if execute(
                &mut client,
                source,
                &format!("{component}-pod-isolation"),
                command,
            )?["exit_code"]
                == 0
            {
                bail!("management RPC reachable from another pod");
            }
        }
        Ok(())
    })();
    if let Err(error) = &result {
        eprintln!("Management checks failed before teardown: {error:#}");
    }
    client.call("cell_close", json!({"instance_id":INSTANCE}))?;
    let closed = cell::wait_closed(&mut client, INSTANCE)?;
    if closed.pointer("/teardown_receipt/verified_absent") != Some(&json!(true)) {
        bail!("management cell teardown was not verified");
    }
    result?;
    println!(
        "Native mint management, TLS enforcement, pod isolation and restart semantics passed for CDK, LDK, BDK and Nutshell"
    );
    Ok(())
}
