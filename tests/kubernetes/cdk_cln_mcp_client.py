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
        "PROOFSTORM_WORKSPACE": "cdk-cln-live",
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
        "clientInfo": {"name": "proofstorm-cdk-cln-live", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "cdk-cln-live-lab",
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
            "id": "mint-cln",
            "kind": "lightning",
            "implementation": "cln",
            "version": "26.06.7",
            "config_version": "cln/26.06/v1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-mint-cln"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk",
            "version": "0.17.6",
            "config_version": "cdk-mintd/0.17/v1",
            "control": "target",
            "config": {"name": "Proofstorm CDK CLN", "description": "Native CDK and CLN lab"},
        },
    ],
    "links": [
        {
            "id": "mint-cln-chain",
            "kind": "chain_backend",
            "from": "mint-cln",
            "to": "chain",
            "binding": {"type": "chain", "network": "regtest"},
        },
        {
            "id": "mint-cln-bolt11",
            "kind": "payment_backend",
            "from": "mint",
            "to": "mint-cln",
            "binding": {"type": "payment", "method": "bolt11", "unit": "sat"},
        },
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

call(2, "proofstorm_lab_create", {"draft_id": "cdk-cln", "lab": lab, "idempotency_key": "create-cdk-cln"})
published = call(
    3,
    "proofstorm_lab_publish",
    {"draft_id": "cdk-cln", "expected_version": 1, "idempotency_key": "publish-cdk-cln", "include_revision": True},
)
cdk_lock = next(entry for entry in published["lock"]["entries"] if entry["catalog_id"] == "cdk")
if cdk_lock["version"] != "0.17.6":
    fail(f"unexpected CDK version: {cdk_lock}")
if cdk_lock["image"] != "docker.io/cashubtc/mintd@sha256:e6018ad5ed3e9914c7892a53239cf602250e788c1fd7c055d4123803cee8dd00":
    fail(f"unexpected CDK image: {cdk_lock}")

status = call(
    4,
    "proofstorm_lab_materialize",
    {
        "instance_id": "cdk-cln-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-cdk-cln",
    },
)
for identifier in range(5, 165):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-cln-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"CDK+CLN lab did not become ready: {status}")

namespace = status["instance_namespace"]
config = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cat", "/config/config.toml"],
    check=True,
    capture_output=True,
    text=True,
).stdout
for expected in [
    'ln_backend = "cln"',
    'rpc_path = "/cln/regtest/lightning-rpc"',
    "bolt12 = false",
]:
    if expected not in config:
        fail(f"mint configuration is missing {expected!r}: {config}")
if "[lnd]" in config:
    fail(f"CLN lab rendered an LND stanza: {config}")

version = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cdk-mintd", "--version"],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
if "0.17.6" not in version:
    fail(f"live mint reports the wrong version: {version!r}")

call(165, "proofstorm_lab_close", {"instance_id": "cdk-cln-instance"})
for identifier in range(166, 226):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-cln-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"CDK+CLN lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "cdk": cdk_lock, "version": version, "status": status}, indent=2))
