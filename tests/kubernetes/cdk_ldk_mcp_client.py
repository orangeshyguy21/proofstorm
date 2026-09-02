#!/usr/bin/env python3
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

from postgres_acceptance import (
    assert_materialized,
    augment_lab,
    enabled as postgres_enabled,
    restart_database,
    seed_sentinel,
    verify_sentinel,
)


def fail(message):
    raise RuntimeError(message)


binary, database = sys.argv[1:3]
run_id = os.environ.get("PROOFSTORM_RUN_ID", str(os.getpid()))
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": (
            f"cdk-ldk-live-postgres-{run_id}"
            if postgres_enabled()
            else f"cdk-ldk-live-{run_id}"
        ),
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


def local_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def http_json(url, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    headers = {} if data is None else {"content-type": "application/json"}
    request_object = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(request_object, timeout=10) as response:
        return json.load(response)


request(
    1,
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-cdk-ldk-live", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "cdk-ldk-live-lab",
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
            "id": "peer",
            "kind": "lightning",
            "implementation": "cln",
            "version": "26.06.7",
            "config_version": "cln/26.06/v1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-ldk-introduction-peer"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk-ldk",
            "version": "0.17.6",
            "config_version": "cdk-mintd-ldk/0.17/v1",
            "control": "target",
            "config": {
                "name": "Proofstorm CDK LDK",
                "description": "Native CDK embedded-LDK BOLT12 lab",
            },
        },
    ],
    "links": [
        {
            "id": "peer-chain",
            "kind": "chain_backend",
            "from": "peer",
            "to": "chain",
            "binding": {"type": "chain", "network": "regtest"},
        },
        {
            "id": "mint-chain",
            "kind": "chain_backend",
            "from": "mint",
            "to": "chain",
            "binding": {"type": "chain", "network": "regtest"},
        }
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}
augment_lab(lab, "proofstorm_ldk")

call(2, "proofstorm_lab_create", {"draft_id": "cdk-ldk", "lab": lab, "idempotency_key": "create-cdk-ldk"})
published = call(
    3,
    "proofstorm_lab_publish",
    {"draft_id": "cdk-ldk", "expected_version": 1, "idempotency_key": "publish-cdk-ldk", "include_revision": True},
)
ldk_lock = next(entry for entry in published["lock"]["entries"] if entry["catalog_id"] == "cdk-ldk")
if ldk_lock["version"] != "0.17.6":
    fail(f"unexpected CDK-LDK version: {ldk_lock}")
if ldk_lock["image"] != "docker.io/cashubtc/mintd@sha256:418527bb3642a2cfd9091caca9d706b5f7582c5c5923cb852f3fe6c29f587392":
    fail(f"unexpected CDK-LDK image: {ldk_lock}")

status = call(
    4,
    "proofstorm_lab_materialize",
    {
        "instance_id": "cdk-ldk-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-cdk-ldk",
    },
)
for identifier in range(5, 165):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-ldk-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"CDK+LDK lab did not become ready: {status}")

namespace = status["instance_namespace"]
config = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cat", "/config/config.toml"],
    check=True,
    capture_output=True,
    text=True,
).stdout
for expected in [
    'ln_backend = "ldknode"',
    'chain_source_type = "bitcoinrpc"',
    'bitcoind_rpc_host = "chain"',
    'ldk_node_host = "0.0.0.0"',
    'ldk_node_port = 9735',
]:
    if expected not in config:
        fail(f"mint configuration is missing {expected!r}: {config}")
if "[lnd]" in config or "[cln]" in config:
    fail("embedded-LDK lab rendered an external Lightning stanza")
postgres_tables = assert_materialized(namespace, config, "proofstorm_ldk")

version = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cdk-mintd", "--version"],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
if "0.17.6" not in version:
    fail(f"live mint reports the wrong version: {version!r}")

mint_logs = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "logs", "deployment/mint", "-n", namespace],
    check=True,
    capture_output=True,
    text=True,
).stdout
node_match = re.search(r"Created node ([0-9a-f]{66})", mint_logs)
if node_match is None:
    fail(f"could not discover embedded LDK node identity from bounded startup logs: {mint_logs}")
ldk_node_id = node_match.group(1)
connect = subprocess.run(
    [
        "kubectl",
        "--context",
        "k3d-proofstorm",
        "exec",
        "statefulset/peer",
        "-n",
        namespace,
        "--",
        "lightning-cli",
        "--lightning-dir=/home/cln/.lightning",
        "--network=regtest",
        "connect",
        f"{ldk_node_id}@mint:9735",
    ],
    check=True,
    capture_output=True,
    text=True,
).stdout
time.sleep(2)

port = local_port()
forward = subprocess.Popen(
    ["kubectl", "--context", "k3d-proofstorm", "port-forward", "-n", namespace, "service/mint", f"{port}:3338"],
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
try:
    info = None
    for _ in range(30):
        try:
            info = http_json(f"http://127.0.0.1:{port}/v1/info")
            break
        except (urllib.error.URLError, ConnectionError):
            if forward.poll() is not None:
                fail("mint port-forward stopped: " + forward.stderr.read())
            time.sleep(1)
    if info is None:
        fail("mint info endpoint was not reachable through port-forward")
    serialized_info = json.dumps(info, sort_keys=True).lower()
    if "bolt12" not in serialized_info:
        fail(f"live mint does not advertise BOLT12: {info}")

    quote = http_json(
        f"http://127.0.0.1:{port}/v1/mint/quote/bolt12",
        {
            "amount": 100,
            "unit": "sat",
            "description": "Proofstorm BOLT12 acceptance",
            "pubkey": "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        },
    )
    if not quote.get("request", "").lower().startswith("lno"):
        fail(f"BOLT12 quote did not return an offer: {quote}")
    if quote.get("unit") != "sat" or quote.get("amount") != 100:
        fail(f"BOLT12 quote returned unexpected terms: {quote}")
    if postgres_enabled():
        seed_sentinel(namespace, "ldk-persistent")
        restart_database(namespace)
        subprocess.run(
            ["kubectl", "--context", "k3d-proofstorm", "rollout", "restart", "deployment/mint", "-n", namespace],
            check=True,
        )
        subprocess.run(
            ["kubectl", "--context", "k3d-proofstorm", "rollout", "status", "deployment/mint", "-n", namespace, "--timeout=180s"],
            check=True,
        )
        verify_sentinel(namespace, "ldk-persistent")
        for _ in range(30):
            try:
                persisted_quote = http_json(
                    f"http://127.0.0.1:{port}/v1/mint/quote/bolt12/{quote['quote']}"
                )
                break
            except urllib.error.URLError:
                time.sleep(1)
        else:
            fail("BOLT12 quote did not survive PostgreSQL and mint restarts")
        if persisted_quote.get("quote") != quote.get("quote"):
            fail(f"recovered BOLT12 quote changed identity: {persisted_quote}")
finally:
    forward.terminate()
    forward.wait(timeout=10)

call(165, "proofstorm_lab_close", {"instance_id": "cdk-ldk-instance"})
for identifier in range(166, 226):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-ldk-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"CDK+LDK lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "revision": published["digest"],
            "cdk_ldk": ldk_lock,
            "version": version,
            "ldk_node_id": ldk_node_id,
            "peer_connect": json.loads(connect),
            "quote": quote,
            "storage": "postgres" if postgres_enabled() else "sqlite",
            "postgres_schema_tables": postgres_tables,
            "status": status,
        },
        indent=2,
    )
)
