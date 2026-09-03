#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time


def fail(message):
    raise RuntimeError(message)


binary, database = sys.argv[1:3]
run_id = os.environ["PROOFSTORM_TEST_RUN_ID"]
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": f"cross-lab-{run_id}",
        "PROOFSTORM_PRINCIPAL": "designer",
        "PROOFSTORM_CAPABILITIES": ",".join(
            [
                "lab.read",
                "lab.create",
                "lab.publish",
                "lab.materialize",
                "lab.status",
                "lab.close",
            ]
        ),
        "PROOFSTORM_CONTROL_NAMESPACE": "proofstorm-system",
    }
)
process = subprocess.Popen(
    [binary],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    env=environment,
)
request_id = 0


def request(method, params):
    global request_id
    request_id += 1
    message = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()
    line = process.stdout.readline()
    if not line:
        fail("MCP server closed: " + process.stderr.read())
    response = json.loads(line)
    if "error" in response:
        fail(f"MCP {method} failed: {response['error']}")
    return response["result"]


def call(name, arguments):
    result = request("tools/call", {"name": name, "arguments": arguments})
    if result.get("isError"):
        fail(f"tool {name} failed: {result}")
    return json.loads(result["content"][0]["text"])


def kubectl_json(*arguments):
    completed = subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", *arguments, "-o", "json"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def prober_snapshot():
    mismatch = None
    for _ in range(20):
        deployments = kubectl_json(
            "get", "deployments", "-A", "-l", "proofstorm.dev/prober=true"
        )["items"]
        pods = kubectl_json("get", "pods", "-A", "-l", "proofstorm.dev/prober=true")[
            "items"
        ]
        labs = kubectl_json("get", "proofstormlabs", "-n", "proofstorm-system")[
            "items"
        ]
        lab_leases = {
            item["spec"]["instanceKey"]: item.get("metadata", {})
            .get("annotations", {})
            .get("proofstorm.dev/prober-lease")
            for item in labs
        }
        active = set()
        mismatch = None
        for deployment in deployments:
            instance = deployment["metadata"]["labels"]["proofstorm.dev/instance"]
            replicas = deployment.get("spec", {}).get("replicas", 0)
            if replicas != 1:
                continue
            active.add(instance)
            deployment_lease = (
                deployment.get("metadata", {})
                .get("annotations", {})
                .get("proofstorm.dev/prober-lease")
            )
            if not deployment_lease or deployment_lease == "inactive":
                mismatch = f"active deployment {instance} has no current scheduler lease"
                break
            if lab_leases.get(instance) != deployment_lease:
                mismatch = (
                    f"lab/deployment scheduler lease mismatch for {instance}: "
                    f"{lab_leases.get(instance)} != {deployment_lease}"
                )
                break
        running_pods = {
            pod["metadata"]["labels"]["proofstorm.dev/instance"]
            for pod in pods
            if pod.get("metadata", {}).get("deletionTimestamp") is None
            and pod.get("status", {}).get("phase") == "Running"
        }
        if len(active) > 4 or len(running_pods) > 4:
            fail(f"global scheduler cap exceeded: active={active}, running={running_pods}")
        if mismatch is None:
            return len(deployments), active, running_pods
        time.sleep(0.1)
    fail(f"scheduler lease annotations did not converge for observation: {mismatch}")


request(
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-cross-lab", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
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
            "config": {"txindex": True, "fallback_fee": 0.0002},
        }
    ],
    "links": [],
    "policy": {
        "allow": [],
        "limits": {"max_components": 8, "max_links": 8, "max_config_bytes": 16384},
    },
}
draft_id = f"cross-lab-{run_id}"
call(
    "proofstorm_lab_create",
    {"draft_id": draft_id, "lab": lab, "idempotency_key": f"create-{run_id}"},
)
published = call(
    "proofstorm_lab_publish",
    {
        "draft_id": draft_id,
        "expected_version": 1,
        "idempotency_key": f"publish-{run_id}",
    },
)
instance_ids = [f"cross-lab-{index}-{run_id}" for index in range(6)]
for index, instance_id in enumerate(instance_ids):
    call(
        "proofstorm_lab_materialize",
        {
            "instance_id": instance_id,
            "revision_digest": published["digest"],
            "idempotency_key": f"materialize-{index}-{run_id}",
        },
    )

observed = set()
deadline = time.monotonic() + 105
while time.monotonic() < deadline:
    count, active, running = prober_snapshot()
    if count > 6:
        fail(f"unexpected protocol prober deployments from another test: {count}")
    if len(active) > 4 or len(running) > 4:
        fail(f"global scheduler cap exceeded: active={active}, running={running}")
    if count == 6:
        observed.update(active)
        if len(observed) == 6:
            break
    time.sleep(2)
else:
    fail(f"scheduler did not fairly activate all six labs; observed={sorted(observed)}")

subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "rollout",
        "restart",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "rollout",
        "status",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--timeout=90s",
    ],
    check=True,
)

restart_deadline = time.monotonic() + 45
while time.monotonic() < restart_deadline:
    count, active, running = prober_snapshot()
    if len(active) > 4 or len(running) > 4:
        fail(f"scheduler cap exceeded after restart: active={active}, running={running}")
    if count == 6 and len(active) == 4 and len(running) <= 4:
        break
    time.sleep(2)
else:
    fail("scheduler did not converge to four active labs after controller restart")

for instance_id in instance_ids:
    call("proofstorm_lab_close", {"instance_id": instance_id})
for instance_id in instance_ids:
    for _ in range(60):
        status = call("proofstorm_lab_status", {"instance_id": instance_id})
        if status["phase"] == "closed":
            receipt = status.get("teardown_receipt") or {}
            if not receipt.get("verified_absent"):
                fail(f"lab {instance_id} closed without verified teardown: {status}")
            break
        time.sleep(2)
    else:
        fail(f"lab {instance_id} did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "instances": instance_ids,
            "observed_active_instances": sorted(observed),
            "global_active_limit": 4,
            "controller_restart_converged": True,
            "verified_teardowns": 6,
        },
        indent=2,
    )
)
