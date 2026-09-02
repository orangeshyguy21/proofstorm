#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time


def fail(message):
    raise RuntimeError(message)


binary, database = sys.argv[1:3]
run_id = os.environ.get("PROOFSTORM_TEST_RUN_ID", str(int(time.time())))
workspace_id = f"native-exec-{run_id}"
draft_id = f"native-exec-{run_id}"
instance_id = f"native-exec-instance-{run_id}"
experiment_id = f"native-exec-experiment-{run_id}"
lease_id = f"native-exec-lease-{run_id}"
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": workspace_id,
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
                "component.exec",
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


def wait_operation(operation_id):
    operation = call(
        "proofstorm_operation_wait",
        {"operation_id": operation_id, "timeout_seconds": 120},
    )
    if operation["timed_out"] or not operation["terminal"]:
        fail(f"operation {operation_id} did not finish: {operation}")
    if operation["phase"] != "succeeded":
        fail(f"operation {operation_id} terminated unexpectedly: {operation}")
    return operation


request(
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-native-exec", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

tools = request("tools/list", {})["tools"]
tool_names = {tool["name"] for tool in tools}
if "proofstorm_component_exec" not in tool_names:
    fail(f"component exec was not advertised for an authorized principal: {tool_names}")

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "native-exec-acceptance",
    "components": [
        {
            "id": "chain",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "version": "30.0",
            "config_version": "bitcoin-core/30/v1",
            "control": "laboratory",
            "config": {"txindex": True, "fallback_fee": 0.0002},
        },
        {
            "id": "chain-b",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "version": "30.0",
            "config_version": "bitcoin-core/30/v1",
            "control": "laboratory",
            "config": {"txindex": True, "fallback_fee": 0.0002},
        },
        {
            "id": "lightning",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "lnd/0.20/v1",
            "control": "laboratory",
            "config": {"alias": "native-exec-lnd"},
        },
        {
            "id": "wallet",
            "kind": "wallet",
            "implementation": "nutshell-wallet",
            "version": "0.20.3",
            "config_version": "nutshell-wallet/0.20/v1",
            "control": "laboratory",
            "config": {},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk",
            "version": "0.17.6",
            "config_version": "cdk-mintd/0.17/v1",
            "control": "target",
            "config": {"name": "Native Exec Mint"},
        },
    ],
    "links": [
        {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
    ],
    "policy": {
        "allow": ["component.exec"],
        "limits": {"max_components": 8, "max_links": 16, "max_config_bytes": 16384},
    },
}
draft = call(
    "proofstorm_lab_create",
    {"draft_id": draft_id, "lab": lab, "idempotency_key": f"create-{run_id}"},
)
draft_document = call("proofstorm_lab_read", {"draft_id": draft_id})
validation = call("proofstorm_lab_validate", {"lab": draft_document["lab"]})
if not validation["valid"]:
    fail(f"native exec lab is invalid: {validation}")
published = call(
    "proofstorm_lab_publish",
    {
        "draft_id": draft_id,
        "expected_version": draft["version"],
        "idempotency_key": f"publish-{run_id}",
        "include_revision": True,
    },
)
locks = {entry["component_id"]: entry["image"] for entry in published["lock"]["entries"]}
if set(locks) != {"chain", "chain-b", "lightning", "mint", "wallet"} or not all(
    "@sha256:" in image for image in locks.values()
):
    fail(f"native exec lab did not resolve exact images: {locks}")

call(
    "proofstorm_lab_materialize",
    {
        "instance_id": instance_id,
        "revision_digest": published["digest"],
        "idempotency_key": f"materialize-{run_id}",
    },
)
waited_ready = call(
    "proofstorm_lab_wait",
    {"instance_id": instance_id, "target_phase": "ready", "timeout_seconds": 120},
)
if not waited_ready["reached"] or waited_ready["timed_out"]:
    fail(f"native exec lab did not become ready: {waited_ready}")
status = call("proofstorm_lab_status", {"instance_id": instance_id})

call(
    "proofstorm_experiment_create",
    {
        "experiment_id": experiment_id,
        "instance_id": instance_id,
        "idempotency_key": f"create-experiment-{run_id}",
    },
)
call(
    "proofstorm_lease_acquire",
    {
        "experiment_id": experiment_id,
        "lease_id": lease_id,
        "duration_seconds": 600,
        "max_actions": 6,
        "idempotency_key": f"acquire-lease-{run_id}",
    },
)

commands = [
    ("bitcoin-help", "chain", "chain", "bitcoin-cli --help", ["bitcoin-cli"]),
    (
        "bitcoin-rpc",
        "chain",
        "chain",
        'bitcoin-cli -regtest -rpcconnect="$BITCOIN_RPC_HOST" '
        '-rpcport="$BITCOIN_RPC_PORT" '
        '-rpcuser="$BITCOIN_RPC_USER" -rpcpassword="$BITCOIN_RPC_PASSWORD" '
        "-rpcwait -rpcwaittimeout=20 getblockchaininfo",
        ['"chain"', '"regtest"'],
    ),
    (
        "bitcoin-rpc-chain-b",
        "chain",
        "chain-b",
        'bitcoin-cli -regtest -rpcconnect="$BITCOIN_RPC_HOST" '
        '-rpcport="$BITCOIN_RPC_PORT" '
        '-rpcuser="$BITCOIN_RPC_USER" -rpcpassword="$BITCOIN_RPC_PASSWORD" '
        "-rpcwait -rpcwaittimeout=20 getblockchaininfo",
        ['"chain"', '"regtest"'],
    ),
    ("lnd-help", "lightning", "lightning", "lncli --help", ["lncli"]),
    (
        "wallet-help",
        "wallet",
        "wallet",
        "cd /app && python3 -c 'from cashu.wallet.cli.cli import cli; cli()' --help",
        ["usage", "cashu"],
    ),
    (
        "token-isolation",
        "wallet",
        "wallet",
        "test ! -e /var/run/secrets/kubernetes.io/serviceaccount/token && echo token_absent",
        ["token_absent"],
    ),
]
operations = []
for operation_id, component, target_component, script, expected_fragments in commands:
    accepted = call(
        "proofstorm_component_exec",
        {
            "instance_id": instance_id,
            "experiment_id": experiment_id,
            "lease_id": lease_id,
            "operation_id": operation_id,
            "component": component,
            "target_component": target_component,
            "script": script,
            "timeout_seconds": 30,
            "idempotency_key": f"{operation_id}-native-exec",
        },
    )
    replayed = call(
        "proofstorm_component_exec",
        {
            "instance_id": instance_id,
            "experiment_id": experiment_id,
            "lease_id": lease_id,
            "operation_id": operation_id,
            "component": component,
            "target_component": target_component,
            "script": script,
            "timeout_seconds": 30,
            "idempotency_key": f"{operation_id}-native-exec",
        },
    )
    if replayed["resource_name"] != accepted["resource_name"] or replayed["sequence"] != accepted["sequence"]:
        fail(f"native exec retry changed action identity: {accepted} {replayed}")
    operation = wait_operation(operation_id)
    content = operation.get("artifact", {}).get("content", {})
    output = content.get("combined_output", "")
    if (
        content.get("component") != component
        or content.get("target_component") != target_component
        or content.get("exit_code") != 0
    ):
        fail(f"native exec returned invalid identity or exit status: {operation}")
    if any(fragment.lower() not in output.lower() for fragment in expected_fragments):
        fail(f"native output for {operation_id} lacks {expected_fragments}: {output}")
    if len(json.dumps(content).encode()) > 32 * 1024:
        fail(f"native artifact exceeded the durable ceiling: {len(json.dumps(content).encode())}")
    operation["id"] = operation["operation_id"]
    operation["resource_name"] = accepted["resource_name"]
    operations.append(operation)

# Operator-side conformance inspection: Proofstorm, not the MCP caller, fixed
# the image, identity, token policy, and target component network labels.
for operation in operations:
    action = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "proofstormlabaction.proofstorm.dev",
            operation["resource_name"],
            "-n",
            "proofstorm-system",
            "-o",
            "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    action_spec = json.loads(action.stdout)["spec"]
    component = action_spec["action"]["parameters"]["component"]
    job = subprocess.run(
        [
            "kubectl",
            "--context",
            "k3d-proofstorm",
            "get",
            "job",
            operation["resource_name"],
            "-n",
            status["instance_namespace"],
            "-o",
            "json",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    pod = json.loads(job.stdout)["spec"]["template"]
    container = pod["spec"]["containers"][0]
    labels = pod["metadata"]["labels"]
    if (
        container["image"] != locks[component]
        or pod["spec"].get("automountServiceAccountToken") is not False
        or labels.get("proofstorm.dev/network-identity") != component
        or "proofstorm.dev/component" in labels
        or "proofstorm.dev/operation" in labels
    ):
        fail(f"controller did not preserve the native exec isolation contract: {pod}")

journal_page = call(
    "proofstorm_action_list",
    {"experiment_id": experiment_id, "after_sequence": 0, "limit": 10},
)
journal = journal_page["actions"]
if [entry["sequence"] for entry in journal] != [1, 2, 3, 4, 5, 6] or any(
    entry["phase"] != "succeeded" for entry in journal
):
    fail(f"native exec journal is not ordered and terminal: {journal}")

call(
    "proofstorm_lease_release",
    {"lease_id": lease_id, "idempotency_key": f"release-lease-{run_id}"},
)
closed_experiment = call(
    "proofstorm_experiment_close",
    {
        "experiment_id": experiment_id,
        "idempotency_key": f"close-experiment-{run_id}",
    },
)
if closed_experiment.get("phase") != "closed":
    fail(f"native exec experiment did not close: {closed_experiment}")
evidence = call(
    "proofstorm_artifact_export",
    {
        "experiment_id": experiment_id,
        "include_oracle_artifacts": False,
        "include_content": True,
        "artifact_operation_ids": [operation["id"] for operation in operations],
    },
)
if (
    not evidence.get("digest", "").startswith("sha256:")
    or len(evidence.get("content", {}).get("journal", [])) != 6
    or len(evidence.get("content", {}).get("artifacts", [])) != 6
):
    fail(f"native exec evidence is incomplete: {evidence}")

call("proofstorm_lab_close", {"instance_id": instance_id})
status = call(
    "proofstorm_lab_wait",
    {"instance_id": instance_id, "target_phase": "closed", "timeout_seconds": 120},
)
if not status["reached"] or status["timed_out"]:
    fail(f"native exec lab did not close: {status}")
if not (status.get("teardown_receipt") or {}).get("verified_absent"):
    fail(f"native exec teardown was not verified: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "operation_ids": [operation["id"] for operation in operations],
            "evidence_digest": evidence["digest"],
            "teardown_receipt": status["teardown_receipt"],
        },
        indent=2,
    )
)
