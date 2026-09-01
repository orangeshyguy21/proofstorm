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
        "PROOFSTORM_WORKSPACE": "slice4",
        "PROOFSTORM_PRINCIPAL": "designer",
        "PROOFSTORM_CAPABILITIES": ",".join(
            [
                "catalog.read",
                "lab.read",
                "lab.create",
                "lab.validate",
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


def request(identifier, method, params):
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


def call(identifier, name, arguments):
    result = request(identifier, "tools/call", {"name": name, "arguments": arguments})
    if result.get("isError"):
        fail(f"tool {name} failed: {result}")
    return json.loads(result["content"][0]["text"])


request(
    1,
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-slice4", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "slice4-static-lab",
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
            "id": "lightning",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "v1alpha1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-lightning"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk",
            "version": "0.17.1",
            "config_version": "v1alpha1",
            "control": "target",
            "config": {"name": "Proofstorm Slice 4", "description": "MCP-created static lab"},
        },
    ],
    "links": [
        {"kind": "chain_backend", "from": "lightning", "to": "chain"},
        {"kind": "lightning_backend", "from": "mint", "to": "lightning"},
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

call(2, "proofstorm_lab_create", {"draft_id": "slice4", "lab": lab, "idempotency_key": "create-slice4"})
published = call(
    3,
    "proofstorm_lab_publish",
    {"draft_id": "slice4", "expected_version": 1, "idempotency_key": "publish-slice4"},
)
if not all("@sha256:" in entry["image"] for entry in published["lock"]["entries"]):
    fail("published lock contains an unpinned image")

status = call(
    4,
    "proofstorm_lab_materialize",
    {
        "instance_id": "slice4-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-slice4",
    },
)
for identifier in range(5, 125):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "slice4-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"lab did not become ready: {status}")

if sorted(component["id"] for component in status["components"] if component["ready"]) != [
    "chain",
    "lightning",
    "mint",
]:
    fail(f"sanitized topology is not ready: {status['components']}")
encoded = json.dumps(status)
if "macaroon" in encoded or "proofstorm-regtest-only" in encoded:
    fail("sanitized status leaked a credential")

call(125, "proofstorm_lab_close", {"instance_id": "slice4-instance"})
for identifier in range(126, 186):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "slice4-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"lab did not close: {status}")

receipt = status.get("teardown_receipt") or {}
if not receipt.get("verified_absent") or not receipt.get("inventory_digest"):
    fail(f"invalid teardown receipt: {receipt}")

process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "lock": published["lock"]["digest"], "status": status}, indent=2))
