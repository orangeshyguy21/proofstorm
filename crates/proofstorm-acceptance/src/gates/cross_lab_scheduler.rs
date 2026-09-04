//! Six-lab global protocol-probe scheduling: fair rotation under a hard cap of
//! four active probers, convergence across a controller restart, and verified
//! teardown of every lab.
//!
//! Ported from `tests/kubernetes/cross_lab_scheduler_mcp_client.py`.

use std::{
    collections::BTreeSet,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{GateContext, LIFECYCLE_CAPABILITIES, gate::CONTROL_NAMESPACE, json as expect};

/// The scheduler must never run more than this many probers at once.
const ACTIVE_CAP: usize = 4;
const LABS: usize = 6;

struct Snapshot {
    deployments: usize,
    active: BTreeSet<String>,
    running: BTreeSet<String>,
}

fn lab_document() -> Value {
    json!({
        "api_version": "proofstorm/v1alpha1",
        "name": "cross-lab-scheduler-acceptance",
        "components": [
            {
                "id": "chain",
                "kind": "bitcoin",
                "implementation": "bitcoin-core",
                "version": "30.0",
                "config_version": "bitcoin-core/30/v1",
                "control": "laboratory",
                "config": {"txindex": true, "fallback_fee": 0.0002}
            }
        ],
        "links": [],
        "policy": {
            "allow": [],
            "limits": {"max_components": 8, "max_links": 8, "max_config_bytes": 16384}
        }
    })
}

/// Read one consistent view of prober deployments, pods, and lab lease
/// annotations, retrying while the annotations are still converging.
fn prober_snapshot(context: &GateContext) -> Result<Snapshot> {
    let mut mismatch = None;
    for _ in 0..20 {
        let deployments = context.kubectl.get_json(&[
            "get",
            "deployments",
            "-A",
            "-l",
            "proofstorm.dev/prober=true",
        ])?;
        let pods =
            context
                .kubectl
                .get_json(&["get", "pods", "-A", "-l", "proofstorm.dev/prober=true"])?;
        let labs = context
            .kubectl
            .get_json(&["get", "proofstormlabs", "-n", CONTROL_NAMESPACE])?;

        let mut lab_leases = std::collections::BTreeMap::new();
        for item in expect::array(&labs, "/items")? {
            let key = expect::string(item, "/spec/instanceKey")?.to_string();
            let lease = item
                .pointer("/metadata/annotations/proofstorm.dev~1prober-lease")
                .and_then(Value::as_str)
                .map(str::to_owned);
            lab_leases.insert(key, lease);
        }

        let deployment_items = expect::array(&deployments, "/items")?;
        let mut active = BTreeSet::new();
        mismatch = None;
        for deployment in deployment_items {
            let instance = expect::string(deployment, "/metadata/labels/proofstorm.dev~1instance")?;
            let replicas = deployment
                .pointer("/spec/replicas")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if replicas != 1 {
                continue;
            }
            active.insert(instance.to_string());
            let lease = deployment
                .pointer("/metadata/annotations/proofstorm.dev~1prober-lease")
                .and_then(Value::as_str);
            match lease {
                None | Some("inactive") => {
                    mismatch = Some(format!(
                        "active deployment {instance} has no current scheduler lease"
                    ));
                    break;
                }
                Some(value) => {
                    if lab_leases.get(instance).and_then(Option::as_deref) != Some(value) {
                        mismatch = Some(format!(
                            "lab/deployment scheduler lease mismatch for {instance}: {:?} != {value}",
                            lab_leases.get(instance)
                        ));
                        break;
                    }
                }
            }
        }

        let mut running = BTreeSet::new();
        for pod in expect::array(&pods, "/items")? {
            let deleting = pod.pointer("/metadata/deletionTimestamp").is_some();
            let phase = pod.pointer("/status/phase").and_then(Value::as_str);
            if !deleting && phase == Some("Running") {
                running.insert(
                    expect::string(pod, "/metadata/labels/proofstorm.dev~1instance")?.to_string(),
                );
            }
        }

        if active.len() > ACTIVE_CAP || running.len() > ACTIVE_CAP {
            bail!("global scheduler cap exceeded: active={active:?}, running={running:?}");
        }
        if mismatch.is_none() {
            return Ok(Snapshot {
                deployments: deployment_items.len(),
                active,
                running,
            });
        }
        sleep(Duration::from_millis(100));
    }
    bail!("scheduler lease annotations did not converge for observation: {mismatch:?}");
}

pub fn run(context: &GateContext) -> Result<()> {
    let run_id = &context.run_id;
    let mut client = context.session(
        &format!("cross-lab-{run_id}"),
        "designer",
        LIFECYCLE_CAPABILITIES,
    )?;

    let draft = format!("cross-lab-{run_id}");
    client.call(
        "proofstorm_lab_create",
        json!({"draft_id": draft, "lab": lab_document(), "idempotency_key": format!("create-{run_id}")}),
    )?;
    let published = client.call(
        "proofstorm_lab_publish",
        json!({"draft_id": draft, "expected_version": 1, "idempotency_key": format!("publish-{run_id}")}),
    )?;
    let digest = expect::string(&published, "/digest")?.to_string();

    let instances: Vec<String> = (0..LABS)
        .map(|index| format!("cross-lab-{index}-{run_id}"))
        .collect();
    for (index, instance) in instances.iter().enumerate() {
        client.call(
            "proofstorm_lab_materialize",
            json!({
                "instance_id": instance,
                "revision_digest": digest,
                "idempotency_key": format!("materialize-{index}-{run_id}")
            }),
        )?;
    }

    let mut observed: BTreeSet<String> = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(105);
    let mut fair = false;
    while Instant::now() < deadline {
        let snapshot = prober_snapshot(context)?;
        if snapshot.deployments > LABS {
            bail!(
                "unexpected protocol prober deployments from another test: {}",
                snapshot.deployments
            );
        }
        if snapshot.active.len() > ACTIVE_CAP || snapshot.running.len() > ACTIVE_CAP {
            bail!(
                "global scheduler cap exceeded: active={:?}, running={:?}",
                snapshot.active,
                snapshot.running
            );
        }
        if snapshot.deployments == LABS {
            observed.extend(snapshot.active.iter().cloned());
            if observed.len() == LABS {
                fair = true;
                break;
            }
        }
        sleep(Duration::from_secs(2));
    }
    if !fair {
        bail!("scheduler did not fairly activate all six labs; observed={observed:?}");
    }

    context.kubectl.run(&[
        "rollout",
        "restart",
        "deployment/proofstormd",
        "-n",
        CONTROL_NAMESPACE,
    ])?;
    context.kubectl.run(&[
        "rollout",
        "status",
        "deployment/proofstormd",
        "-n",
        CONTROL_NAMESPACE,
        "--timeout=90s",
    ])?;

    let restart_deadline = Instant::now() + Duration::from_secs(45);
    let mut converged = false;
    while Instant::now() < restart_deadline {
        let snapshot = prober_snapshot(context)?;
        if snapshot.active.len() > ACTIVE_CAP || snapshot.running.len() > ACTIVE_CAP {
            bail!(
                "scheduler cap exceeded after restart: active={:?}, running={:?}",
                snapshot.active,
                snapshot.running
            );
        }
        if snapshot.deployments == LABS
            && snapshot.active.len() == ACTIVE_CAP
            && snapshot.running.len() <= ACTIVE_CAP
        {
            converged = true;
            break;
        }
        sleep(Duration::from_secs(2));
    }
    if !converged {
        bail!("scheduler did not converge to four active labs after controller restart");
    }

    for instance in &instances {
        client.call("proofstorm_lab_close", json!({"instance_id": instance}))?;
    }
    for instance in &instances {
        let mut closed = false;
        for _ in 0..60 {
            let status = client.call("proofstorm_lab_status", json!({"instance_id": instance}))?;
            if expect::string(&status, "/phase")? == "closed" {
                if !expect::boolean(&status, "/teardown_receipt/verified_absent")? {
                    bail!("lab {instance} closed without verified teardown: {status}");
                }
                closed = true;
                break;
            }
            sleep(Duration::from_secs(2));
        }
        if !closed {
            bail!("lab {instance} did not close");
        }
    }

    context.kubectl.assert_no_instance_namespaces()?;

    println!(
        "MCP six-lab global protocol-probe scheduling, fair rotation, restart convergence, and verified teardown acceptance passed"
    );
    Ok(())
}
