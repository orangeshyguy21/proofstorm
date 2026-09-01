#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time


def fail(message):
    raise RuntimeError(message)


binary, database = sys.argv[1:3]
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": "slice5",
        "PROOFSTORM_PRINCIPAL": "experiment-agent",
        "PROOFSTORM_CAPABILITIES": ",".join(
            [
                "catalog.read",
                "lab.read",
                "lab.create",
                "lab.edit",
                "lab.validate",
                "lab.publish",
                "lab.materialize",
                "lab.status",
                "lab.close",
                "experiment.create",
                "experiment.read",
                "experiment.close",
                "lease.acquire",
                "lease.release",
                "action.cancel",
                "topology.mutate",
                "node.control",
                "chain.mine",
                "wallet.create",
                "wallet.control",
                "wallet.fund",
                "peer.connect",
                "peer.disconnect",
                "channel.open",
                "channel.close",
                "channel.force_close",
                "channel.rebalance",
                "network.delay",
                "network.drop",
                "network.partition",
                "network.heal",
                "oracle.run",
                "artifact.read",
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
identifier = 0


def request(method, params):
    global identifier
    identifier += 1
    message = {"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}
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


def expect_tool_error(name, arguments, code):
    global identifier
    identifier += 1
    message = {
        "jsonrpc": "2.0",
        "id": identifier,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()
    line = process.stdout.readline()
    if not line:
        fail("MCP server closed: " + process.stderr.read())
    response = json.loads(line)
    if code not in json.dumps(response.get("error") or response.get("result")):
        fail(f"tool {name} did not refuse with {code}: {response}")


def wait_operation(operation_id, attempts=120):
    for _ in range(attempts):
        operation = call("proofstorm_operation_status", {"operation_id": operation_id})
        if operation["phase"] == "succeeded":
            return operation
        if operation["phase"] == "failed":
            fail(f"operation {operation_id} failed: {operation}")
        time.sleep(3)
    fail(f"operation {operation_id} did not finish")


def wait_operation_phase(operation_id, expected, attempts=120):
    for _ in range(attempts):
        operation = call("proofstorm_operation_status", {"operation_id": operation_id})
        if operation["phase"] == expected:
            return operation
        if operation["phase"] in {"succeeded", "failed", "cancelled"}:
            fail(f"operation {operation_id} reached {operation['phase']}, expected {expected}")
        time.sleep(1)
    fail(f"operation {operation_id} did not reach {expected}")


request(
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-slice5", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

network_backend = call("proofstorm_network_capabilities", {})
if (
    network_backend.get("id") != "kubernetes-network-policy"
    or network_backend.get("version") != "networking.k8s.io/v1"
    or network_backend.get("features") != ["partition", "heal"]
    or network_backend.get("directions") != ["bidirectional"]
    or network_backend.get("bounds")
    != {
        "max_delay_ms": None,
        "max_jitter_ms": None,
        "max_loss_basis_points": None,
    }
):
    fail(f"network backend discovery is not explicit and bounded: {network_backend}")

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "slice5-cashu-round-trip",
    "components": [
        {
            "id": "chain",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "version": "30.0",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {"txindex": True, "fallback_fee": 0.0002},
        },
        {
            "id": "mint-lnd",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-mint"},
        },
        {
            "id": "payer-lnd",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-payer"},
        },
        {
            "id": "attacker-cln",
            "kind": "lightning",
            "implementation": "cln",
            "version": "26.06.7",
            "config_version": "v1alpha1",
            "control": "attacker",
            "config": {"alias": "proofstorm-attacker"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk",
            "version": "0.17.1",
            "config_version": "v1alpha1",
            "control": "target",
            "config": {"name": "Proofstorm Slice 5", "description": "Agent-created Cashu lab"},
        },
        {
            "id": "wallet",
            "kind": "wallet",
            "implementation": "nutshell-wallet",
            "version": "0.20.2",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {},
        },
        {
            "id": "receiver-wallet",
            "kind": "wallet",
            "implementation": "nutshell-wallet",
            "version": "0.20.2",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {},
        },
    ],
    "links": [
        {"kind": "chain_backend", "from": "mint-lnd", "to": "chain"},
        {"kind": "chain_backend", "from": "payer-lnd", "to": "chain"},
        {"kind": "chain_backend", "from": "attacker-cln", "to": "chain"},
        {"kind": "lightning_backend", "from": "mint", "to": "mint-lnd"},
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

components = lab["components"]
links = lab["links"]
lab["components"] = []
lab["links"] = []
draft = call(
    "proofstorm_lab_create",
    {"draft_id": "slice5", "lab": lab, "idempotency_key": "create-slice5"},
)
for component in components:
    mutation = {
        "draft_id": "slice5",
        "expected_version": draft["version"],
        "component": component,
        "idempotency_key": f"add-component-{component['id']}",
    }
    draft = call("proofstorm_component_add", mutation)
    if component["id"] == "chain":
        replayed = call("proofstorm_component_add", mutation)
        if replayed != draft:
            fail("component mutation replay was not idempotent")
for link in links:
    draft = call(
        "proofstorm_link_add",
        {
            "draft_id": "slice5",
            "expected_version": draft["version"],
            "link": link,
            "idempotency_key": f"add-link-{link['kind']}-{link['from']}-{link['to']}",
        },
    )
if [component["id"] for component in draft["lab"]["components"]] != sorted(
    component["id"] for component in components
):
    fail("component composer did not produce canonical ordering")
validation = call("proofstorm_lab_validate", {"lab": draft["lab"]})
if not validation["valid"]:
    fail(f"agent-composed draft is invalid: {validation}")
published = call(
    "proofstorm_lab_publish",
    {
        "draft_id": "slice5",
        "expected_version": draft["version"],
        "idempotency_key": "publish-slice5",
    },
)
if not all("@sha256:" in entry["image"] for entry in published["lock"]["entries"]):
    fail("published lock contains an unpinned image")

call(
    "proofstorm_lab_materialize",
    {
        "instance_id": "slice5-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-slice5",
    },
)
for _ in range(180):
    status = call("proofstorm_lab_status", {"instance_id": "slice5-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"lab did not become ready: {status}")

ready = sorted(component["id"] for component in status["components"] if component["ready"])
if ready != [
    "attacker-cln",
    "chain",
    "mint",
    "mint-lnd",
    "payer-lnd",
    "receiver-wallet",
    "wallet",
]:
    fail(f"lab topology is not ready: {status['components']}")

expect_tool_error(
    "proofstorm_network_delay",
    {
        "instance_id": "slice5-instance",
        "experiment_id": "unsupported-network-experiment",
        "lease_id": "unsupported-network-lease",
        "operation_id": "unsupported-network-delay",
        "from_component": "wallet",
        "to_component": "mint",
        "direction": "from_to",
        "delay_ms": 100,
        "jitter_ms": 10,
        "idempotency_key": "unsupported-network-delay-slice5",
    },
    "network_fault_unsupported",
)
expect_tool_error(
    "proofstorm_network_loss",
    {
        "instance_id": "slice5-instance",
        "experiment_id": "unsupported-network-experiment",
        "lease_id": "unsupported-network-lease",
        "operation_id": "unsupported-network-loss",
        "from_component": "wallet",
        "to_component": "mint",
        "direction": "bidirectional",
        "loss_basis_points": 250,
        "idempotency_key": "unsupported-network-loss-slice5",
    },
    "network_fault_unsupported",
)

# Kubernetes operators can submit typed actions directly, so the controller must
# terminally reject a schema-valid but semantically invalid request without ever
# creating a Job.
labs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "proofstormlabs.proofstorm.dev",
        "-n",
        "proofstorm-system",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
lab_resource = next(
    item
    for item in json.loads(labs.stdout)["items"]
    if item["spec"]["instanceId"] == "slice5-instance"
)
invalid_action_name = "slice5-invalid-peer-action"
invalid_action = {
    "apiVersion": "proofstorm.dev/v1alpha1",
    "kind": "ProofstormLabAction",
    "metadata": {
        "name": invalid_action_name,
        "namespace": "proofstorm-system",
        "labels": {
            "proofstorm.dev/instance": lab_resource["spec"]["instanceKey"],
            "proofstorm.dev/lab": lab_resource["metadata"]["name"],
            "app.kubernetes.io/managed-by": "proofstorm-controller-conformance",
        },
    },
    "spec": {
        "labName": lab_resource["metadata"]["name"],
        "workspaceId": "slice5",
        "instanceId": "slice5-instance",
        "instanceKey": lab_resource["spec"]["instanceKey"],
        "experimentId": "controller-conformance",
        "leaseId": "controller-conformance",
        "principalId": "cluster-operator",
        "sequence": 1,
        "operationId": "invalid-peer-connect",
        "requestDigest": "sha256:controller-conformance",
        "capability": "peer.connect",
        "acceptedAtUnix": int(time.time()),
        "action": {
            "kind": "peer_connect",
            "parameters": {
                "fromLightning": "mint-lnd",
                "toLightning": "mint-lnd",
            },
        },
    },
}
subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "apply", "-f", "-"],
    check=True,
    input=json.dumps(invalid_action),
    text=True,
)
for _ in range(30):
    invalid_runtime = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "proofstormlabaction.proofstorm.dev",
            invalid_action_name,
            "-n",
            "proofstorm-system",
            "-o",
            "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    invalid_status = json.loads(invalid_runtime.stdout).get("status", {})
    if invalid_status.get("phase") == "Failed":
        break
    time.sleep(1)
else:
    fail(f"invalid typed action did not fail closed: {invalid_status}")
if invalid_status.get("error", {}).get("code") != "invalid_action":
    fail(f"invalid typed action has the wrong terminal error: {invalid_status}")
invalid_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "job",
        invalid_action_name,
        "-n",
        status["instance_namespace"],
        "--ignore-not-found",
        "-o",
        "name",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if invalid_jobs.stdout.strip():
    fail("invalid typed action created a runtime Job")
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "delete",
        "proofstormlabaction.proofstorm.dev",
        invalid_action_name,
        "-n",
        "proofstorm-system",
    ],
    check=True,
)

call(
    "proofstorm_experiment_create",
    {
        "experiment_id": "slice5-experiment",
        "instance_id": "slice5-instance",
        "idempotency_key": "create-slice5-experiment",
    },
)
call(
    "proofstorm_lease_acquire",
    {
        "experiment_id": "slice5-experiment",
        "lease_id": "slice5-lease",
        "duration_seconds": 900,
        "max_actions": 47,
        "idempotency_key": "acquire-slice5-lease",
    },
)
expect_tool_error(
    "proofstorm_lab_close",
    {"instance_id": "slice5-instance"},
    "instance_leased",
)

bootstrap_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "bootstrap",
    "chain": "chain",
    "mint_lightning": "mint-lnd",
    "payer_lightning": "payer-lnd",
    "funding_sat": 50000000,
    "channel_sat": 10000000,
    "push_sat": 5000000,
    "idempotency_key": "bootstrap-slice5",
}
accepted_bootstrap = call(
    "proofstorm_liquidity_bootstrap",
    bootstrap_request,
)
for _ in range(30):
    runtime_action = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "proofstormlabactions.proofstorm.dev",
            "-n",
            "proofstorm-system",
            "-l",
            "proofstorm.dev/instance",
            "-o",
            "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    items = json.loads(runtime_action.stdout)["items"]
    if items:
        break
    time.sleep(1)
else:
    fail("controller-owned ProofstormLabAction was not created")
if len(items) != 1 or items[0]["spec"]["action"]["kind"] != "bootstrap_liquidity":
    fail(f"unexpected typed runtime action: {items}")
retried_bootstrap = call("proofstorm_liquidity_bootstrap", bootstrap_request)
if (
    retried_bootstrap["resource_name"] != accepted_bootstrap["resource_name"]
    or retried_bootstrap["sequence"] != accepted_bootstrap["sequence"]
):
    fail("caller retry changed the accepted action identity")
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
runtime_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "jobs",
        "-n",
        status["instance_namespace"],
        "-l",
        "proofstorm.dev/action",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if len(json.loads(runtime_jobs.stdout)["items"]) != 1:
    fail("caller retry or controller restart duplicated the bootstrap Job")
bootstrap = wait_operation("bootstrap")
if not bootstrap["artifact"]["content"].get("ready"):
    fail(f"bootstrap artifact is invalid: {bootstrap}")
bootstrap_channel_id = bootstrap["artifact"]["content"].get("channel_id", "")
if not bootstrap_channel_id.startswith("ch-") or len(bootstrap_channel_id) != 67:
    fail(f"bootstrap did not return an opaque channel handle: {bootstrap}")

peer_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "peer-connect",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "idempotency_key": "peer-connect-slice5",
}
accepted_peer = call("proofstorm_peer_connect", peer_request)
retried_peer = call("proofstorm_peer_connect", peer_request)
if (
    retried_peer["resource_name"] != accepted_peer["resource_name"]
    or retried_peer["sequence"] != accepted_peer["sequence"]
):
    fail("caller retry changed the accepted peer action identity")
peer = wait_operation("peer-connect")
if not peer["artifact"]["content"].get("connected"):
    fail(f"peer-connect artifact is invalid: {peer}")

channel_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "channel-open",
    "chain": "chain",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "channel_sat": 2000000,
    "push_sat": 0,
    "idempotency_key": "channel-open-slice5",
}
accepted_channel = call("proofstorm_channel_open", channel_request)
retried_channel = call("proofstorm_channel_open", channel_request)
if (
    retried_channel["resource_name"] != accepted_channel["resource_name"]
    or retried_channel["sequence"] != accepted_channel["sequence"]
):
    fail("caller retry changed the accepted channel action identity")
channel = wait_operation("channel-open")
if not channel["artifact"]["content"].get("active"):
    fail(f"channel-open artifact is invalid: {channel}")
channel_id = channel["artifact"]["content"].get("channel_id", "")
if not channel_id.startswith("ch-") or len(channel_id) != 67:
    fail(f"channel open did not return an opaque channel handle: {channel}")

initialize_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-initialize",
    "wallet": "wallet",
    "mint": "mint",
    "idempotency_key": "wallet-initialize-slice5",
}
accepted_initialize = call("proofstorm_wallet_initialize", initialize_request)
retried_initialize = call("proofstorm_wallet_initialize", initialize_request)
if (
    retried_initialize["resource_name"] != accepted_initialize["resource_name"]
    or retried_initialize["sequence"] != accepted_initialize["sequence"]
):
    fail("caller retry changed the accepted wallet-initialize identity")
initialized = wait_operation("wallet-initialize")
if not initialized["artifact"]["content"].get("initialized"):
    fail(f"wallet-initialize artifact is invalid: {initialized}")

balance_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-balance",
    "wallet": "wallet",
    "mint": "mint",
    "idempotency_key": "wallet-balance-slice5",
}
call("proofstorm_wallet_balance", balance_request)
balance = wait_operation("wallet-balance")
if balance["artifact"]["content"].get("balance_sat") != 0:
    fail(f"new wallet did not have a zero sanitized balance: {balance}")

fund_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-fund",
    "wallet": "wallet",
    "mint": "mint",
    "payer_lightning": "payer-lnd",
    "amount_sat": 1000,
    "idempotency_key": "wallet-fund-slice5",
}
accepted_fund = call("proofstorm_wallet_fund", fund_request)
retried_fund = call("proofstorm_wallet_fund", fund_request)
if (
    retried_fund["resource_name"] != accepted_fund["resource_name"]
    or retried_fund["sequence"] != accepted_fund["sequence"]
):
    fail("caller retry changed the accepted wallet-fund identity")
funded = wait_operation("wallet-fund")
fund_result = funded["artifact"]["content"]
if fund_result.get("funded_sat") != 1000 or fund_result.get("balance_sat") != 1000:
    fail(f"wallet-fund artifact is invalid: {funded}")

wallet_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "round-trip",
    "wallet": "wallet",
    "mint": "mint",
    "payer_lightning": "payer-lnd",
    "amount_sat": 1000,
    "tolerance_sat": 100,
    "idempotency_key": "round-trip-slice5",
}
accepted_wallet = call(
    "proofstorm_wallet_round_trip",
    wallet_request,
)
retried_wallet = call("proofstorm_wallet_round_trip", wallet_request)
if (
    retried_wallet["resource_name"] != accepted_wallet["resource_name"]
    or retried_wallet["sequence"] != accepted_wallet["sequence"]
):
    fail("caller retry changed the accepted wallet action identity")

runtime_actions = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "proofstormlabactions.proofstorm.dev",
        "-n",
        "proofstorm-system",
        "-l",
        "proofstorm.dev/instance",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
action_kinds = {
    item["metadata"]["name"]: item["spec"]["action"]["kind"]
    for item in json.loads(runtime_actions.stdout)["items"]
}
if action_kinds.get(accepted_wallet["resource_name"]) != "wallet_round_trip":
    fail(f"wallet request did not create a typed runtime action: {action_kinds}")

round_trip = wait_operation("round-trip")
wallet_result = round_trip["artifact"]["content"]
if wallet_result.get("inflation") is not False:
    fail(f"round-trip artifact is invalid: {round_trip}")

wallet_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "jobs",
        "-n",
        status["instance_namespace"],
        "-l",
        f"proofstorm.dev/action={accepted_wallet['resource_name']}",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if len(json.loads(wallet_jobs.stdout)["items"]) != 1:
    fail("caller retry duplicated the controller-owned wallet Job")

lost_oracle_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "lost-conservation",
    "wallet": "wallet",
    "mint": "mint",
    "expected_sat": wallet_result["balance_after_swap_sat"],
    "tolerance_sat": 0,
    "idempotency_key": "lost-conservation-slice5",
}
accepted_lost = call("proofstorm_conservation_oracle", lost_oracle_request)
for _ in range(60):
    lost_runtime = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "proofstormlabaction.proofstorm.dev",
            accepted_lost["resource_name"],
            "-n",
            "proofstorm-system",
            "-o",
            "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    if json.loads(lost_runtime.stdout).get("status", {}).get("phase") == "Running":
        break
    time.sleep(0.25)
else:
    fail("lost-Job action never recorded its execution fence")

subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=0",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "wait",
        "--for=delete",
        "pod",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
        "--timeout=90s",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "delete",
        "job",
        accepted_lost["resource_name"],
        "-n",
        status["instance_namespace"],
        "--wait=true",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=1",
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
lost = wait_operation_phase("lost-conservation", "failed")
if lost["artifact"]["content"].get("code") != "action_job_lost":
    fail(f"lost Job did not produce the replay-safe terminal error: {lost}")
lost_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "job",
        accepted_lost["resource_name"],
        "-n",
        status["instance_namespace"],
        "--ignore-not-found",
        "-o",
        "name",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if lost_jobs.stdout.strip():
    fail("controller replayed a lost action Job")

subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=0",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "wait",
        "--for=delete",
        "pod",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
        "--timeout=90s",
    ],
    check=True,
)
cancelled_oracle_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cancelled-conservation",
    "wallet": "wallet",
    "mint": "mint",
    "expected_sat": wallet_result["balance_after_swap_sat"],
    "tolerance_sat": 0,
    "idempotency_key": "cancelled-conservation-slice5",
}
accepted_cancelled = call("proofstorm_conservation_oracle", cancelled_oracle_request)
cancel_request = {
    "operation_id": "cancelled-conservation",
    "idempotency_key": "cancel-action-slice5",
}
first_cancel = call("proofstorm_action_cancel", cancel_request)
retried_cancel = call("proofstorm_action_cancel", cancel_request)
if (
    first_cancel["resource_name"] != accepted_cancelled["resource_name"]
    or retried_cancel["resource_name"] != accepted_cancelled["resource_name"]
    or retried_cancel["sequence"] != accepted_cancelled["sequence"]
):
    fail("cancellation retry changed the accepted action identity")
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=1",
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
cancelled = wait_operation_phase("cancelled-conservation", "cancelled")
if cancelled["artifact"]["content"].get("code") != "action_cancelled":
    fail(f"cancelled action artifact is invalid: {cancelled}")
cancelled_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "jobs",
        "-n",
        status["instance_namespace"],
        "-l",
        f"proofstorm.dev/action={accepted_cancelled['resource_name']}",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if json.loads(cancelled_jobs.stdout)["items"]:
    fail("cancelled action created or retained a runtime Job across controller restart")

oracle_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "conservation",
    "wallet": "wallet",
    "mint": "mint",
    "expected_sat": wallet_result["balance_after_swap_sat"],
    "tolerance_sat": 0,
    "idempotency_key": "conservation-slice5",
}
accepted_oracle = call(
    "proofstorm_conservation_oracle",
    oracle_request,
)
retried_oracle = call("proofstorm_conservation_oracle", oracle_request)
if (
    retried_oracle["resource_name"] != accepted_oracle["resource_name"]
    or retried_oracle["sequence"] != accepted_oracle["sequence"]
):
    fail("caller retry changed the accepted oracle action identity")
runtime_actions = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "proofstormlabactions.proofstorm.dev",
        "-n",
        "proofstorm-system",
        "-l",
        "proofstorm.dev/instance",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
action_kinds = {
    item["metadata"]["name"]: item["spec"]["action"]["kind"]
    for item in json.loads(runtime_actions.stdout)["items"]
}
if action_kinds.get(accepted_oracle["resource_name"]) != "conservation_oracle":
    fail(f"oracle request did not create a typed runtime action: {action_kinds}")
oracle = wait_operation("conservation")
if not oracle["artifact"]["content"].get("conserved"):
    fail(f"oracle artifact is invalid: {oracle}")

oracle_jobs = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "jobs",
        "-n",
        status["instance_namespace"],
        "-l",
        f"proofstorm.dev/action={accepted_oracle['resource_name']}",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if len(json.loads(oracle_jobs.stdout)["items"]) != 1:
    fail("caller retry duplicated the controller-owned oracle Job")

receiver_initialize_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "receiver-initialize",
    "wallet": "receiver-wallet",
    "mint": "mint",
    "idempotency_key": "receiver-initialize-slice5",
}
call("proofstorm_wallet_initialize", receiver_initialize_request)
receiver_initialized = wait_operation("receiver-initialize")
if receiver_initialized["artifact"]["content"].get("balance_sat") != 0:
    fail(f"receiver wallet did not initialize empty: {receiver_initialized}")

cancelled_invoice_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cancelled-wallet-invoice",
    "quote_id": "cancelled-receiver-quote",
    "wallet": "receiver-wallet",
    "mint": "mint",
    "amount_sat": 50,
    "timeout_seconds": 300,
    "idempotency_key": "cancelled-wallet-invoice-slice5",
}
cancelled_invoice_action = call(
    "proofstorm_wallet_invoice", cancelled_invoice_request
)
private_invoice_path = (
    "/wallet/.proofstorm/quotes/cancelled-receiver-quote/invoice.log"
)
for _ in range(120):
    invoice_exists = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "exec",
            "-n",
            status["instance_namespace"],
            "deployment/receiver-wallet",
            "--",
            "test",
            "-s",
            private_invoice_path,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if invoice_exists.returncode == 0:
        break
    time.sleep(0.25)
else:
    fail("cancelled invoice never materialized its private payment request")
call(
    "proofstorm_action_cancel",
    {
        "operation_id": "cancelled-wallet-invoice",
        "idempotency_key": "cancel-wallet-invoice-slice5",
    },
)
cancelled_invoice = wait_operation_phase("cancelled-wallet-invoice", "cancelled")
cancelled_quote = call(
    "proofstorm_wallet_quote_status", {"quote_id": "cancelled-receiver-quote"}
)
if (
    cancelled_quote["phase"] != "cancelled"
    or cancelled_quote.get("terminal_code") != "action_cancelled"
):
    fail(f"pre-payment invoice cancellation was not final: {cancelled_quote}")
for _ in range(120):
    invoice_removed = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "exec",
            "-n",
            status["instance_namespace"],
            "deployment/receiver-wallet",
            "--",
            "test",
            "!",
            "-e",
            private_invoice_path,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if invoice_removed.returncode == 0:
        break
    time.sleep(0.25)
else:
    fail("cancelled invoice left private payment material on the wallet volume")

invoice_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-invoice",
    "quote_id": "receiver-quote",
    "wallet": "receiver-wallet",
    "mint": "mint",
    "amount_sat": 100,
    "timeout_seconds": 300,
    "idempotency_key": "wallet-invoice-slice5",
}
accepted_invoice = call("proofstorm_wallet_invoice", invoice_request)
retried_invoice = call("proofstorm_wallet_invoice", invoice_request)
if (
    retried_invoice["resource_name"] != accepted_invoice["resource_name"]
    or retried_invoice["sequence"] != accepted_invoice["sequence"]
):
    fail("invoice retry changed the accepted action identity")
quote = call("proofstorm_wallet_quote_status", {"quote_id": "receiver-quote"})
if quote["phase"] != "ready" or quote["amount_sat"] != 100:
    fail(f"receive quote was not ready and sanitized: {quote}")

pay_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-pay",
    "quote_id": "receiver-quote",
    "wallet": "wallet",
    "mint": "mint",
    "idempotency_key": "wallet-pay-slice5",
}
accepted_pay = call("proofstorm_wallet_pay", pay_request)
retried_pay = call("proofstorm_wallet_pay", pay_request)
if (
    retried_pay["resource_name"] != accepted_pay["resource_name"]
    or retried_pay["sequence"] != accepted_pay["sequence"]
):
    fail("pay retry changed the accepted action identity")
paid = wait_operation("wallet-pay")
paid_content = paid["artifact"]["content"]
if paid_content.get("phase") != "paid" or paid_content.get("amount_sat") != 100:
    fail(f"wallet pay artifact is invalid: {paid}")
settled_invoice = wait_operation("wallet-invoice")
invoice_content = settled_invoice["artifact"]["content"]
if invoice_content.get("phase") != "settled" or invoice_content.get("balance_sat") != 100:
    fail(f"wallet invoice artifact is invalid: {settled_invoice}")
quote = call("proofstorm_wallet_quote_status", {"quote_id": "receiver-quote"})
if quote["phase"] != "settled" or quote.get("settled_at_unix") is None:
    fail(f"receive quote did not settle: {quote}")
quote_list = call(
    "proofstorm_wallet_quote_list",
    {"experiment_id": "slice5-experiment", "limit": 10},
)["quotes"]
if quote_list != [cancelled_quote, quote]:
    fail(f"quote list is not canonical: {quote_list}")
serialized_quote_flow = json.dumps(
    {"quote": quote, "pay": paid_content, "invoice": invoice_content}
)
for forbidden in ["lnbcrt", "payment_request", "adapter_quote", "mnemonic"]:
    if forbidden in serialized_quote_flow.lower():
        fail(f"private payment material crossed MCP in quote flow: {forbidden}")

node_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "component": "payer-lnd",
}
stop_request = {
    **node_request,
    "operation_id": "payer-stop",
    "idempotency_key": "payer-stop-slice5",
}
accepted_stop = call("proofstorm_node_stop", stop_request)
retried_stop = call("proofstorm_node_stop", stop_request)
if (
    retried_stop["resource_name"] != accepted_stop["resource_name"]
    or retried_stop["sequence"] != accepted_stop["sequence"]
):
    fail("node stop retry changed the accepted action identity")
stopped = wait_operation("payer-stop")
if stopped["artifact"]["content"].get("state") != "stopped":
    fail(f"node stop artifact is invalid: {stopped}")
stateful = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "statefulset/payer-lnd",
        "-n",
        status["instance_namespace"],
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
if json.loads(stateful.stdout)["spec"]["replicas"] != 0:
    fail("stopped Lightning node did not retain zero desired replicas")
for _ in range(60):
    stopped_lab = call("proofstorm_lab_status", {"instance_id": "slice5-instance"})
    payer_status = next(
        component
        for component in stopped_lab["components"]
        if component["id"] == "payer-lnd"
    )
    if stopped_lab["phase"] == "ready" and not payer_status["ready"]:
        break
    time.sleep(1)
else:
    fail(f"intentionally stopped node corrupted lab readiness: {stopped_lab}")

start_request = {
    **node_request,
    "operation_id": "payer-start",
    "idempotency_key": "payer-start-slice5",
}
call("proofstorm_node_start", start_request)
started = wait_operation("payer-start")
if started["artifact"]["content"].get("state") != "running":
    fail(f"node start artifact is invalid: {started}")
pod_before_restart = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "pod/payer-lnd-0",
        "-n",
        status["instance_namespace"],
        "-o",
        "jsonpath={.metadata.uid}",
    ],
    check=True,
    capture_output=True,
    text=True,
).stdout
restart_request = {
    **node_request,
    "operation_id": "payer-restart",
    "idempotency_key": "payer-restart-slice5",
}
call("proofstorm_node_restart", restart_request)
restarted = wait_operation("payer-restart")
if not restarted["artifact"]["content"].get("restarted"):
    fail(f"node restart artifact is invalid: {restarted}")
pod_after_restart = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "pod/payer-lnd-0",
        "-n",
        status["instance_namespace"],
        "-o",
        "jsonpath={.metadata.uid}",
    ],
    check=True,
    capture_output=True,
    text=True,
).stdout
if pod_after_restart == pod_before_restart:
    fail("node restart completed without replacing the component pod")

def component_pod(component):
    return subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "pod",
            "-n",
            status["instance_namespace"],
            "-l",
            f"proofstorm.dev/component={component}",
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


wallet_pod = component_pod("wallet")
receiver_wallet_pod = component_pod("receiver-wallet")


reachability_observations = []


def observe_mint_reachability(operation_id, component, expected):
    accepted = call(
        "proofstorm_reachability_oracle",
        {
            "instance_id": "slice5-instance",
            "experiment_id": "slice5-experiment",
            "lease_id": "slice5-lease",
            "operation_id": operation_id,
            "from_component": component,
            "to_component": "mint",
            "service": "http",
            "timeout_seconds": 2,
            "attempts": 3,
            "idempotency_key": f"{operation_id}-slice5",
        },
    )
    observed = wait_operation(operation_id)
    content = observed["artifact"]["content"]
    if (
        content.get("from_component") != component
        or content.get("to_component") != "mint"
        or content.get("service") != "http"
        or content.get("port") != 3338
        or content.get("reachable") is not expected
        or not 1 <= content.get("attempts", 0) <= 3
        or content.get("timeout_seconds") != 2
    ):
        fail(f"invalid MCP reachability observation: {observed}")
    reachability_observations.append(observed)
    return accepted, observed


def pod_can_reach_mint(pod):
    probe = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "exec",
            "-n",
            status["instance_namespace"],
            pod,
            "--",
            "python3",
            "-c",
            'import socket; s=socket.create_connection(("mint",3338),3); s.close()',
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    return probe.returncode == 0


if not pod_can_reach_mint(wallet_pod) or not pod_can_reach_mint(receiver_wallet_pod):
    fail("wallets could not reach mint before the requested partitions")
observe_mint_reachability("reachability-baseline", "wallet", True)
partition_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-mint-partition",
    "from_component": "wallet",
    "to_component": "mint",
    "idempotency_key": "wallet-mint-partition-slice5",
}
accepted_partition = call("proofstorm_network_partition", partition_request)
retried_partition = call("proofstorm_network_partition", partition_request)
if (
    retried_partition["resource_name"] != accepted_partition["resource_name"]
    or retried_partition["sequence"] != accepted_partition["sequence"]
):
    fail("network partition retry changed the accepted action identity")
partitioned = wait_operation("wallet-mint-partition")
partition_content = partitioned["artifact"]["content"]
if (
    not partition_content.get("partitioned")
    or partition_content.get("from_component") != "wallet"
    or partition_content.get("to_component") != "mint"
    or partition_content.get("active_partition_count") != 1
):
    fail(f"network partition artifact is invalid: {partitioned}")
for _ in range(30):
    if not pod_can_reach_mint(wallet_pod):
        break
    time.sleep(1)
else:
    fail("CNI continued to pass wallet-to-mint traffic after partition")
if not pod_can_reach_mint(receiver_wallet_pod):
    fail("wallet-to-mint partition also blocked the independent receiver wallet")
observe_mint_reachability("reachability-wallet-blocked", "wallet", False)
observe_mint_reachability("reachability-receiver-open", "receiver-wallet", True)

receiver_partition_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "receiver-wallet-mint-partition",
    "from_component": "receiver-wallet",
    "to_component": "mint",
    "idempotency_key": "receiver-wallet-mint-partition-slice5",
}
call("proofstorm_network_partition", receiver_partition_request)
receiver_partitioned = wait_operation("receiver-wallet-mint-partition")
receiver_partition_content = receiver_partitioned["artifact"]["content"]
if (
    not receiver_partition_content.get("partitioned")
    or receiver_partition_content.get("from_component") != "receiver-wallet"
    or receiver_partition_content.get("to_component") != "mint"
    or receiver_partition_content.get("active_partition_count") != 2
):
    fail(f"overlapping network partition artifact is invalid: {receiver_partitioned}")
for _ in range(30):
    if not pod_can_reach_mint(receiver_wallet_pod):
        break
    time.sleep(1)
else:
    fail("CNI continued to pass receiver-wallet-to-mint traffic after partition")
observe_mint_reachability("reachability-receiver-blocked", "receiver-wallet", False)

controller_pod_before_fault_restart = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "pod",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
        "-o",
        "jsonpath={.items[0].metadata.uid}",
    ],
    check=True,
    capture_output=True,
    text=True,
).stdout
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=0",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "wait",
        "--for=delete",
        "pod",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
        "--timeout=90s",
    ],
    check=True,
)
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "delete",
        "networkpolicy",
        "default-deny-all",
        "wallet",
        "receiver-wallet",
        "mint",
        "-n",
        status["instance_namespace"],
        "--wait=true",
    ],
    check=True,
)
for _ in range(30):
    if pod_can_reach_mint(wallet_pod) and pod_can_reach_mint(receiver_wallet_pod):
        break
    time.sleep(1)
else:
    fail("removing fault policies while proofstormd was stopped did not restore traffic")
subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "scale",
        "deployment/proofstormd",
        "-n",
        "proofstorm-system",
        "--replicas=1",
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
controller_pod_after_fault_restart = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "pod",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
        "-o",
        "jsonpath={.items[0].metadata.uid}",
    ],
    check=True,
    capture_output=True,
    text=True,
).stdout
if controller_pod_after_fault_restart == controller_pod_before_fault_restart:
    fail("network-fault persistence check did not replace the proofstormd pod")
for _ in range(30):
    if not pod_can_reach_mint(wallet_pod) and not pod_can_reach_mint(
        receiver_wallet_pod
    ):
        break
    time.sleep(1)
else:
    fail("proofstormd restart did not reconstruct both active partitions")
observe_mint_reachability("reachability-wallet-reconstructed", "wallet", False)
observe_mint_reachability(
    "reachability-receiver-reconstructed", "receiver-wallet", False
)

heal_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "wallet-mint-heal",
    "partition_operation_id": "wallet-mint-partition",
    "idempotency_key": "wallet-mint-heal-slice5",
}
call("proofstorm_network_heal", heal_request)
healed = wait_operation("wallet-mint-heal")
heal_content = healed["artifact"]["content"]
if (
    not heal_content.get("healed")
    or heal_content.get("partition_operation_id") != "wallet-mint-partition"
    or heal_content.get("active_partition_count") != 1
):
    fail(f"network heal artifact is invalid: {healed}")
for _ in range(30):
    if pod_can_reach_mint(wallet_pod):
        break
    time.sleep(1)
else:
    fail("wallet-to-mint traffic did not recover after heal")
if pod_can_reach_mint(receiver_wallet_pod):
    fail("healing one partition also healed the overlapping receiver partition")
observe_mint_reachability("reachability-wallet-healed", "wallet", True)
observe_mint_reachability(
    "reachability-receiver-still-blocked", "receiver-wallet", False
)

receiver_heal_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "receiver-wallet-mint-heal",
    "partition_operation_id": "receiver-wallet-mint-partition",
    "idempotency_key": "receiver-wallet-mint-heal-slice5",
}
call("proofstorm_network_heal", receiver_heal_request)
receiver_healed = wait_operation("receiver-wallet-mint-heal")
receiver_heal_content = receiver_healed["artifact"]["content"]
if (
    not receiver_heal_content.get("healed")
    or receiver_heal_content.get("partition_operation_id")
    != "receiver-wallet-mint-partition"
    or receiver_heal_content.get("active_partition_count") != 0
):
    fail(f"overlapping network heal artifact is invalid: {receiver_healed}")
for _ in range(30):
    if pod_can_reach_mint(wallet_pod) and pod_can_reach_mint(receiver_wallet_pod):
        break
    time.sleep(1)
else:
    fail("receiver-wallet-to-mint traffic did not recover after its targeted heal")
observe_mint_reachability("reachability-receiver-healed", "receiver-wallet", True)

cln_peer_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cln-peer-connect",
    "from_lightning": "attacker-cln",
    "to_lightning": "mint-lnd",
    "idempotency_key": "cln-peer-connect-slice5",
}
call("proofstorm_peer_connect", cln_peer_request)
cln_peer = wait_operation("cln-peer-connect")
if not cln_peer["artifact"]["content"].get("connected"):
    fail(f"CLN to LND peer connection artifact is invalid: {cln_peer}")

cln_channel_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cln-channel-open",
    "chain": "chain",
    "from_lightning": "mint-lnd",
    "to_lightning": "attacker-cln",
    "channel_sat": 1000000,
    "push_sat": 300000,
    "idempotency_key": "cln-channel-open-slice5",
}
call("proofstorm_channel_open", cln_channel_request)
cln_channel = wait_operation("cln-channel-open")
cln_channel_id = cln_channel["artifact"]["content"].get("channel_id", "")
if not cln_channel_id.startswith("ch-") or len(cln_channel_id) != 67:
    fail(f"LND to CLN channel did not return an opaque handle: {cln_channel}")

bridge_peer_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "rebalance-bridge-peer-connect",
    "from_lightning": "payer-lnd",
    "to_lightning": "attacker-cln",
    "idempotency_key": "rebalance-bridge-peer-connect-slice5",
}
call("proofstorm_peer_connect", bridge_peer_request)
bridge_peer = wait_operation("rebalance-bridge-peer-connect")
if not bridge_peer["artifact"]["content"].get("connected"):
    fail(f"rebalance bridge peer artifact is invalid: {bridge_peer}")

bridge_channel_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "rebalance-bridge-channel-open",
    "chain": "chain",
    "from_lightning": "payer-lnd",
    "to_lightning": "attacker-cln",
    "channel_sat": 1000000,
    "push_sat": 0,
    "idempotency_key": "rebalance-bridge-channel-open-slice5",
}
call("proofstorm_channel_open", bridge_channel_request)
bridge_channel = wait_operation("rebalance-bridge-channel-open")
bridge_channel_id = bridge_channel["artifact"]["content"].get("channel_id", "")
if not bridge_channel_id.startswith("ch-") or len(bridge_channel_id) != 67:
    fail(f"rebalance bridge did not return an opaque handle: {bridge_channel}")

rebalance_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "channel-rebalance",
    "lightning": "mint-lnd",
    "outgoing_channel_id": channel_id,
    "incoming_channel_id": cln_channel_id,
    "amount_sat": 100000,
    "max_fee_sat": 100,
    "idempotency_key": "channel-rebalance-slice5",
}
accepted_rebalance = call("proofstorm_channel_rebalance", rebalance_request)
retried_rebalance = call("proofstorm_channel_rebalance", rebalance_request)
if (
    retried_rebalance["resource_name"] != accepted_rebalance["resource_name"]
    or retried_rebalance["sequence"] != accepted_rebalance["sequence"]
):
    fail("channel rebalance retry changed the accepted action identity")
rebalanced = wait_operation("channel-rebalance")
rebalanced_content = rebalanced["artifact"]["content"]
if (
    not rebalanced_content.get("rebalanced")
    or rebalanced_content.get("amount_sat") != 100000
    or rebalanced_content.get("fee_sat", 101) > 100
    or rebalanced_content.get("outgoing_channel_id") != channel_id
    or rebalanced_content.get("incoming_channel_id") != cln_channel_id
    or rebalanced_content.get("outgoing_local_before_sat")
    <= rebalanced_content.get("outgoing_local_after_sat")
    or rebalanced_content.get("incoming_local_before_sat")
    >= rebalanced_content.get("incoming_local_after_sat")
):
    fail(f"channel rebalance artifact is invalid: {rebalanced}")

bridge_close_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "rebalance-bridge-channel-close",
    "chain": "chain",
    "from_lightning": "payer-lnd",
    "to_lightning": "attacker-cln",
    "channel_id": bridge_channel_id,
    "idempotency_key": "rebalance-bridge-channel-close-slice5",
}
call("proofstorm_channel_close", bridge_close_request)
bridge_closed = wait_operation("rebalance-bridge-channel-close")
if bridge_closed["artifact"]["content"].get("channel_id") != bridge_channel_id:
    fail(f"rebalance bridge close artifact is invalid: {bridge_closed}")

close_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "channel-close",
    "chain": "chain",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "channel_id": channel_id,
    "idempotency_key": "channel-close-slice5",
}
accepted_close = call("proofstorm_channel_close", close_request)
retried_close = call("proofstorm_channel_close", close_request)
if (
    retried_close["resource_name"] != accepted_close["resource_name"]
    or retried_close["sequence"] != accepted_close["sequence"]
):
    fail("channel close retry changed the accepted action identity")
closed = wait_operation("channel-close")
closed_content = closed["artifact"]["content"]
if (
    not closed_content.get("closed")
    or not closed_content.get("confirmed")
    or closed_content.get("force")
    or closed_content.get("pending_resolution")
    or closed_content.get("channel_id") != channel_id
):
    fail(f"cooperative channel close artifact is invalid: {closed}")

bootstrap_close_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "bootstrap-channel-close",
    "chain": "chain",
    "from_lightning": "payer-lnd",
    "to_lightning": "mint-lnd",
    "channel_id": bootstrap_channel_id,
    "idempotency_key": "bootstrap-channel-close-slice5",
}
call("proofstorm_channel_close", bootstrap_close_request)
bootstrap_closed = wait_operation("bootstrap-channel-close")
if bootstrap_closed["artifact"]["content"].get("channel_id") != bootstrap_channel_id:
    fail(f"bootstrap channel close artifact is invalid: {bootstrap_closed}")

disconnect_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "peer-disconnect",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "idempotency_key": "peer-disconnect-slice5",
}
accepted_disconnect = call("proofstorm_peer_disconnect", disconnect_request)
retried_disconnect = call("proofstorm_peer_disconnect", disconnect_request)
if (
    retried_disconnect["resource_name"] != accepted_disconnect["resource_name"]
    or retried_disconnect["sequence"] != accepted_disconnect["sequence"]
):
    fail("peer disconnect retry changed the accepted action identity")
disconnected = wait_operation("peer-disconnect")
if not disconnected["artifact"]["content"].get("disconnected"):
    fail(f"peer disconnect artifact is invalid: {disconnected}")

reconnect_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "peer-reconnect",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "idempotency_key": "peer-reconnect-slice5",
}
call("proofstorm_peer_connect", reconnect_request)
reconnected = wait_operation("peer-reconnect")
if not reconnected["artifact"]["content"].get("connected"):
    fail(f"peer reconnect artifact is invalid: {reconnected}")

force_channel_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "force-channel-open",
    "chain": "chain",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "channel_sat": 1000000,
    "push_sat": 0,
    "idempotency_key": "force-channel-open-slice5",
}
call("proofstorm_channel_open", force_channel_request)
force_channel = wait_operation("force-channel-open")
force_channel_id = force_channel["artifact"]["content"].get("channel_id", "")
if not force_channel_id.startswith("ch-") or len(force_channel_id) != 67:
    fail(f"force-close target did not return an opaque handle: {force_channel}")

force_close_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "channel-force-close",
    "chain": "chain",
    "from_lightning": "mint-lnd",
    "to_lightning": "payer-lnd",
    "channel_id": force_channel_id,
    "idempotency_key": "channel-force-close-slice5",
}
call("proofstorm_channel_force_close", force_close_request)
force_closed = wait_operation("channel-force-close")
force_closed_content = force_closed["artifact"]["content"]
if (
    not force_closed_content.get("closed")
    or not force_closed_content.get("confirmed")
    or not force_closed_content.get("force")
    or not force_closed_content.get("pending_resolution")
    or force_closed_content.get("channel_id") != force_channel_id
):
    fail(f"force channel close artifact is invalid: {force_closed}")

cln_close_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cln-channel-close",
    "chain": "chain",
    "from_lightning": "attacker-cln",
    "to_lightning": "mint-lnd",
    "channel_id": cln_channel_id,
    "idempotency_key": "cln-channel-close-slice5",
}
call("proofstorm_channel_close", cln_close_request)
cln_closed = wait_operation("cln-channel-close")
cln_closed_content = cln_closed["artifact"]["content"]
if (
    not cln_closed_content.get("closed")
    or not cln_closed_content.get("confirmed")
    or cln_closed_content.get("force")
    or cln_closed_content.get("pending_resolution")
    or cln_closed_content.get("channel_id") != cln_channel_id
):
    fail(f"CLN cooperative close artifact is invalid: {cln_closed}")

cln_disconnect_request = {
    "instance_id": "slice5-instance",
    "experiment_id": "slice5-experiment",
    "lease_id": "slice5-lease",
    "operation_id": "cln-peer-disconnect",
    "from_lightning": "attacker-cln",
    "to_lightning": "mint-lnd",
    "idempotency_key": "cln-peer-disconnect-slice5",
}
call("proofstorm_peer_disconnect", cln_disconnect_request)
cln_disconnected = wait_operation("cln-peer-disconnect")
if not cln_disconnected["artifact"]["content"].get("disconnected"):
    fail(f"CLN to LND disconnect artifact is invalid: {cln_disconnected}")

cln_reconnect_request = dict(cln_peer_request)
cln_reconnect_request.update(
    {
        "operation_id": "cln-peer-reconnect",
        "idempotency_key": "cln-peer-reconnect-slice5",
    }
)
call("proofstorm_peer_connect", cln_reconnect_request)
cln_reconnected = wait_operation("cln-peer-reconnect")
if not cln_reconnected["artifact"]["content"].get("connected"):
    fail(f"CLN to LND reconnect artifact is invalid: {cln_reconnected}")

cln_force_channel_request = dict(cln_channel_request)
cln_force_channel_request.update(
    {
        "operation_id": "cln-force-channel-open",
        "idempotency_key": "cln-force-channel-open-slice5",
    }
)
call("proofstorm_channel_open", cln_force_channel_request)
cln_force_channel = wait_operation("cln-force-channel-open")
cln_force_channel_id = cln_force_channel["artifact"]["content"].get("channel_id", "")
if not cln_force_channel_id.startswith("ch-") or len(cln_force_channel_id) != 67:
    fail(f"CLN force-close target did not return an opaque handle: {cln_force_channel}")

cln_force_close_request = dict(cln_close_request)
cln_force_close_request.update(
    {
        "operation_id": "cln-channel-force-close",
        "channel_id": cln_force_channel_id,
        "idempotency_key": "cln-channel-force-close-slice5",
    }
)
call("proofstorm_channel_force_close", cln_force_close_request)
cln_force_closed = wait_operation("cln-channel-force-close")
cln_force_closed_content = cln_force_closed["artifact"]["content"]
if (
    not cln_force_closed_content.get("closed")
    or not cln_force_closed_content.get("confirmed")
    or not cln_force_closed_content.get("force")
    or not cln_force_closed_content.get("pending_resolution")
    or cln_force_closed_content.get("channel_id") != cln_force_channel_id
):
    fail(f"CLN force-close artifact is invalid: {cln_force_closed}")

expect_tool_error(
    "proofstorm_wallet_balance",
    {
        "instance_id": "slice5-instance",
        "experiment_id": "slice5-experiment",
        "lease_id": "slice5-lease",
        "operation_id": "over-budget",
        "wallet": "wallet",
        "mint": "mint",
        "idempotency_key": "over-budget-slice5",
    },
    "action_budget_exceeded",
)
runtime_actions = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "get",
        "proofstormlabactions.proofstorm.dev",
        "-n",
        "proofstorm-system",
        "-l",
        "proofstorm.dev/instance",
        "-o",
        "json",
    ],
    check=True,
    capture_output=True,
    text=True,
)
runtime_items = json.loads(runtime_actions.stdout)["items"]
if any(item["spec"]["operationId"] == "over-budget" for item in runtime_items):
    fail("exhausted action budget created a runtime action")
runtime_kinds = {
    item["spec"]["operationId"]: item["spec"]["action"]["kind"]
    for item in runtime_items
}
if runtime_kinds.get("wallet-invoice") != "wallet_invoice" or runtime_kinds.get(
    "wallet-pay"
) != "wallet_pay":
    fail(f"quote flow did not use typed runtime actions: {runtime_kinds}")
if [runtime_kinds.get(operation) for operation in ["payer-stop", "payer-start", "payer-restart"]] != [
    "node_stop",
    "node_start",
    "node_restart",
]:
    fail(f"node lifecycle did not use typed runtime actions: {runtime_kinds}")
if [
    runtime_kinds.get(operation)
    for operation in [
        "channel-close",
        "bootstrap-channel-close",
        "peer-disconnect",
        "peer-reconnect",
        "force-channel-open",
        "channel-force-close",
    ]
] != [
    "channel_close",
    "channel_close",
    "peer_disconnect",
    "peer_connect",
    "channel_open",
    "channel_force_close",
]:
    fail(f"topology teardown did not use typed runtime actions: {runtime_kinds}")
if [
    runtime_kinds.get(operation)
    for operation in [
        "cln-peer-connect",
        "cln-channel-open",
        "cln-channel-close",
        "cln-peer-disconnect",
        "cln-peer-reconnect",
        "cln-force-channel-open",
        "cln-channel-force-close",
    ]
] != [
    "peer_connect",
    "channel_open",
    "channel_close",
    "peer_disconnect",
    "peer_connect",
    "channel_open",
    "channel_force_close",
]:
    fail(f"CLN interoperability did not use typed runtime actions: {runtime_kinds}")
if [
    runtime_kinds.get(operation)
    for operation in [
        "rebalance-bridge-peer-connect",
        "rebalance-bridge-channel-open",
        "channel-rebalance",
        "rebalance-bridge-channel-close",
    ]
] != ["peer_connect", "channel_open", "channel_rebalance", "channel_close"]:
    fail(f"rebalance did not use typed runtime actions: {runtime_kinds}")
if [
    runtime_kinds.get("wallet-mint-partition"),
    runtime_kinds.get("receiver-wallet-mint-partition"),
    runtime_kinds.get("wallet-mint-heal"),
    runtime_kinds.get("receiver-wallet-mint-heal"),
] != ["network_partition", "network_partition", "network_heal", "network_heal"]:
    fail(f"network faults did not use typed runtime actions: {runtime_kinds}")
if any(
    runtime_kinds.get(observation["id"]) != "reachability_oracle"
    for observation in reachability_observations
):
    fail(f"reachability observations did not use typed runtime actions: {runtime_kinds}")

journal_page = call(
    "proofstorm_action_list",
    {"experiment_id": "slice5-experiment", "after_sequence": 0, "limit": 100},
)
journal = journal_page["actions"]
if [action["sequence"] for action in journal] != list(range(1, 48)):
    fail(f"action journal is not canonical and ordered: {journal}")
if (
    journal[7]["phase"] != "failed"
    or journal[8]["phase"] != "cancelled"
    or journal[11]["phase"] != "cancelled"
    or any(
        action["phase"] != "succeeded"
        for index, action in enumerate(journal)
        if index not in {7, 8, 11}
    )
):
    fail(f"failure, cancellation, and success states are not ordered: {journal}")

call(
    "proofstorm_lease_release",
    {"lease_id": "slice5-lease", "idempotency_key": "release-slice5-lease"},
)
closed_experiment = call(
    "proofstorm_experiment_close",
    {"experiment_id": "slice5-experiment", "idempotency_key": "close-slice5-experiment"},
)
if closed_experiment.get("phase") != "closed":
    fail(f"experiment did not close before evidence export: {closed_experiment}")

evidence = call(
    "proofstorm_artifact_export",
    {
        "experiment_id": "slice5-experiment",
        "include_oracle_artifacts": True,
        "artifact_operation_ids": ["wallet-pay"],
    },
)
evidence_content = evidence.get("content", {})
if (
    evidence.get("media_type")
    != "application/vnd.proofstorm.evidence.v1alpha1+json"
    or not evidence.get("digest", "").startswith("sha256:")
    or not 0 < evidence.get("byte_length", 0) <= 512 * 1024
    or evidence_content.get("api_version") != "proofstorm/evidence/v1alpha1"
    or evidence_content.get("instance", {}).get("revision_digest")
    != status["instance"]["revision_digest"]
    or evidence_content.get("instance", {}).get("lock_digest")
    != status["instance"]["lock_digest"]
    or [action["sequence"] for action in evidence_content.get("journal", [])]
    != list(range(1, 48))
):
    fail(f"evidence bundle identity or journal is invalid: {evidence}")
expected_evidence_artifacts = {
    "lost-conservation",
    "cancelled-conservation",
    "conservation",
    "wallet-pay",
    *[observation["id"] for observation in reachability_observations],
}
actual_evidence_artifacts = {
    artifact["operation_id"] for artifact in evidence_content.get("artifacts", [])
}
if actual_evidence_artifacts != expected_evidence_artifacts:
    fail(
        "evidence bundle did not contain exactly the selected and oracle artifacts: "
        f"{actual_evidence_artifacts}"
    )
serialized_evidence = json.dumps(evidence).lower()
for forbidden in [
    "resource_name",
    "instance_key",
    "lnbcrt",
    "payment_request",
    "adapter_quote",
    "mnemonic",
]:
    if forbidden in serialized_evidence:
        fail(f"private or runtime-only material crossed evidence export: {forbidden}")

call("proofstorm_lab_close", {"instance_id": "slice5-instance"})
for _ in range(90):
    status = call("proofstorm_lab_status", {"instance_id": "slice5-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"lab did not close: {status}")
if not (status.get("teardown_receipt") or {}).get("verified_absent"):
    fail(f"invalid teardown receipt: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "runtime_action": items[0]["spec"],
            "bootstrap": bootstrap,
            "peer": peer,
            "channel": channel,
            "initialized": initialized,
            "balance": balance,
            "funded": funded,
            "round_trip": round_trip,
            "oracle": oracle,
            "cancelled_invoice": cancelled_invoice,
            "cancelled_quote": cancelled_quote,
            "quote": quote,
            "paid": paid,
            "settled_invoice": settled_invoice,
            "stopped": stopped,
            "started": started,
            "restarted": restarted,
            "partitioned": partitioned,
            "receiver_partitioned": receiver_partitioned,
            "healed": healed,
            "receiver_healed": receiver_healed,
            "reachability_observations": reachability_observations,
            "closed": closed,
            "bootstrap_closed": bootstrap_closed,
            "reconnected": reconnected,
            "force_channel": force_channel,
            "force_closed": force_closed,
            "disconnected": disconnected,
            "cln_peer": cln_peer,
            "cln_channel": cln_channel,
            "rebalance_bridge_peer": bridge_peer,
            "rebalance_bridge_channel": bridge_channel,
            "rebalanced": rebalanced,
            "rebalance_bridge_closed": bridge_closed,
            "cln_closed": cln_closed,
            "cln_disconnected": cln_disconnected,
            "cln_reconnected": cln_reconnected,
            "cln_force_channel": cln_force_channel,
            "cln_force_closed": cln_force_closed,
            "journal": journal,
            "evidence": evidence,
            "status": status,
        },
        indent=2,
    )
)
