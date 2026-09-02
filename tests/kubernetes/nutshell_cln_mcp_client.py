#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time


def fail(message):
    raise RuntimeError(message)


def kubectl(*arguments, capture=True):
    return subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", *arguments],
        check=True,
        capture_output=capture,
        text=True,
    ).stdout


binary, database = sys.argv[1:3]
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": "nutshell-cln-live",
        "PROOFSTORM_PRINCIPAL": "experiment-agent",
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
                "experiment.create",
                "experiment.read",
                "experiment.close",
                "lease.acquire",
                "lease.release",
                "wallet.create",
                "wallet.control",
                "wallet.fund",
                "chain.mine",
                "peer.connect",
                "channel.open",
                "oracle.run",
                "artifact.read",
            ]
        ),
        "PROOFSTORM_CONTROL_NAMESPACE": "proofstorm-system",
    }
)
process = subprocess.Popen(
    [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, env=environment,
)
identifier = 0


def request(method, params):
    global identifier
    identifier += 1
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}, separators=(",", ":")) + "\n")
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


def wait_operation(operation_id, attempts=180):
    for _ in range(attempts):
        operation = call("proofstorm_operation_status", {"operation_id": operation_id})
        if operation["phase"] == "succeeded":
            return operation
        if operation["phase"] in {"failed", "cancelled"}:
            fail(f"operation {operation_id} failed: {operation}")
        time.sleep(3)
    fail(f"operation {operation_id} did not finish")


request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "proofstorm-nutshell-cln-live", "version": "0.1.0"}})
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

catalog = call("proofstorm_catalog_list", {})
nutshell = next(entry for entry in catalog["entries"] if entry["id"] == "nutshell")
if set(nutshell["support_matrix"]["payment_backends"]) != {"cln", "lnd"}:
    fail(f"Nutshell does not advertise exact CLN and LND support: {nutshell['support_matrix']}")
if not any(binding["backend"]["implementation"] == "cln" and binding["backend"]["versions"] == ["26.06.7"] for binding in nutshell["support_matrix"]["payment_bindings"]):
    fail("Nutshell does not advertise its exact Core Lightning binding")

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "nutshell-cln-live-lab",
    "components": [
        {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
        {"id": "seed-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-seed"}},
        {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-payer"}},
        {"id": "mint-cln", "kind": "lightning", "implementation": "cln", "version": "26.06.7", "config_version": "cln/26.06/v1", "control": "laboratory", "config": {"alias": "proofstorm-cln-mint"}},
        {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.2", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell CLN", "description": "Core Lightning REST acceptance", "clnrest_enable_mpp": True}},
        {"id": "wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.2", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
    ],
    "links": [
        {"id": "seed-chain", "kind": "chain_backend", "from": "seed-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "payer-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "cln-chain", "kind": "chain_backend", "from": "mint-cln", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "mint-cln-bolt11", "kind": "payment_backend", "from": "mint", "to": "mint-cln", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
    ],
    "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}},
}

call("proofstorm_lab_create", {"draft_id": "nutshell-cln", "lab": lab, "idempotency_key": "create-nutshell-cln"})
published = call("proofstorm_lab_publish", {"draft_id": "nutshell-cln", "expected_version": 1, "idempotency_key": "publish-nutshell-cln"})
locks = {entry["component_id"]: entry for entry in published["lock"]["entries"]}
if locks["mint"]["catalog_id"] != "nutshell" or locks["mint-cln"]["catalog_id"] != "cln":
    fail(f"unexpected Nutshell+CLN lock: {locks}")
if any("@sha256:" not in locks[component]["image"] for component in ["mint", "mint-cln"]):
    fail("Nutshell or Core Lightning image is not digest-pinned")

call("proofstorm_lab_materialize", {"instance_id": "nutshell-cln-instance", "revision_digest": published["digest"], "idempotency_key": "materialize-nutshell-cln"})
for _ in range(220):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-cln-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell+CLN lab did not become ready: {status}")

namespace = status["instance_namespace"]
mint_config = json.loads(kubectl("get", "configmap/mint-config", "-n", namespace, "-o", "json"))["data"]
expected_config = {
    "MINT_BACKEND_BOLT11_SAT": "CLNRestWallet",
    "MINT_CLNREST_ENABLE_MPP": "TRUE",
    "MINT_CLNREST_RUNE": "/app/data/.proofstorm/cln.rune",
    "MINT_CLNREST_URL": "http://mint-cln:3010",
}
if any(mint_config.get(key) != value for key, value in expected_config.items()):
    fail(f"live Nutshell CLN configuration differs: {mint_config}")
if any(key.startswith("MINT_LND_") for key in mint_config) or any("rune" in value.lower() and value != "/app/data/.proofstorm/cln.rune" for value in mint_config.values()):
    fail("Nutshell CLN public configuration contains an LND setting or rune material")

cln_service = json.loads(kubectl("get", "service/mint-cln", "-n", namespace, "-o", "json"))
ports = {port["name"]: port["port"] for port in cln_service["spec"]["ports"]}
if ports != {"p2p": 9735, "rest": 3010}:
    fail(f"Core Lightning service contract differs: {ports}")

rune_probe = """
import hashlib, json, os, httpx
path = '/app/data/.proofstorm/cln.rune'
rune = open(path).read().strip()
headers = {'rune': rune, 'accept': 'application/json'}
allowed = httpx.post('http://mint-cln:3010/v1/listfunds', headers=headers).status_code
forbidden = httpx.post('http://mint-cln:3010/v1/withdraw', headers=headers, data={'destination': 'x', 'satoshi': 'all'}).status_code
print(json.dumps({'length': len(rune), 'mode': oct(os.stat(path).st_mode & 0o777), 'digest': hashlib.sha256(rune.encode()).hexdigest(), 'allowed': allowed, 'forbidden': forbidden}))
"""
rune_before = json.loads(kubectl("exec", "deployment/mint", "-n", namespace, "--", "python3", "-c", rune_probe).strip())
if rune_before["length"] < 32 or rune_before["mode"] != "0o600" or rune_before["allowed"] not in {200, 201} or rune_before["forbidden"] not in {401, 403}:
    fail(f"restricted CLN rune contract failed: {rune_before}")

kubectl("rollout", "restart", "deployment/mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/mint", "-n", namespace, "--timeout=180s", capture=False)
rune_after = json.loads(kubectl("exec", "deployment/mint", "-n", namespace, "--", "python3", "-c", rune_probe).strip())
if rune_after != rune_before:
    fail(f"Nutshell restart changed its restricted CLN rune contract: before={rune_before} after={rune_after}")

call("proofstorm_experiment_create", {"experiment_id": "nutshell-cln-experiment", "instance_id": "nutshell-cln-instance", "idempotency_key": "create-nutshell-cln-experiment"})
call("proofstorm_lease_acquire", {"experiment_id": "nutshell-cln-experiment", "lease_id": "nutshell-cln-lease", "duration_seconds": 1200, "max_actions": 8, "idempotency_key": "acquire-nutshell-cln-lease"})
common = {"instance_id": "nutshell-cln-instance", "experiment_id": "nutshell-cln-experiment", "lease_id": "nutshell-cln-lease"}

call("proofstorm_liquidity_bootstrap", {**common, "operation_id": "nutshell-cln-bootstrap", "chain": "chain", "mint_lightning": "seed-lnd", "payer_lightning": "payer-lnd", "funding_sat": 50000000, "channel_sat": 10000000, "push_sat": 1000000, "idempotency_key": "bootstrap-nutshell-cln"})
bootstrap = wait_operation("nutshell-cln-bootstrap")
if not bootstrap["artifact"]["content"].get("ready"):
    fail(f"LND bootstrap failed: {bootstrap}")

call("proofstorm_peer_connect", {**common, "operation_id": "nutshell-cln-peer", "from_lightning": "payer-lnd", "to_lightning": "mint-cln", "idempotency_key": "peer-nutshell-cln"})
peer = wait_operation("nutshell-cln-peer")
if not peer["artifact"]["content"].get("connected"):
    fail(f"LND-to-CLN peer connection failed: {peer}")

call("proofstorm_channel_open", {**common, "operation_id": "nutshell-cln-channel", "chain": "chain", "from_lightning": "payer-lnd", "to_lightning": "mint-cln", "channel_sat": 4000000, "push_sat": 1000000, "idempotency_key": "channel-nutshell-cln"})
channel = wait_operation("nutshell-cln-channel")
if not channel["artifact"]["content"].get("active"):
    fail(f"LND-to-CLN channel failed: {channel}")

wallet_common = {**common, "wallet": "wallet", "mint": "mint"}
call("proofstorm_wallet_initialize", {**wallet_common, "operation_id": "nutshell-cln-initialize", "idempotency_key": "initialize-nutshell-cln"})
initialized = wait_operation("nutshell-cln-initialize")
if not initialized["artifact"]["content"].get("initialized"):
    fail(f"Nutshell CLN wallet initialization failed: {initialized}")

call("proofstorm_wallet_balance", {**wallet_common, "operation_id": "nutshell-cln-balance", "idempotency_key": "balance-nutshell-cln"})
balance = wait_operation("nutshell-cln-balance")
if balance["artifact"]["content"].get("balance_sat") != 0:
    fail(f"Nutshell CLN wallet did not start empty: {balance}")

call("proofstorm_wallet_fund", {**wallet_common, "operation_id": "nutshell-cln-fund", "payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": "fund-nutshell-cln"})
funded = wait_operation("nutshell-cln-fund")
fund_content = funded["artifact"]["content"]
if fund_content.get("funded_sat") != 1000 or fund_content.get("balance_sat") != 1000:
    fail(f"Nutshell CLN wallet funding failed: {funded}")

call("proofstorm_wallet_round_trip", {**wallet_common, "operation_id": "nutshell-cln-round-trip", "payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": "round-trip-nutshell-cln"})
round_trip = wait_operation("nutshell-cln-round-trip")
round_content = round_trip["artifact"]["content"]
if round_content.get("inflation") is not False or round_content.get("minted_sat") != 1000:
    fail(f"Nutshell CLN wallet round trip failed: {round_trip}")

call("proofstorm_conservation_oracle", {**wallet_common, "operation_id": "nutshell-cln-conservation", "expected_sat": round_content["balance_after_swap_sat"], "tolerance_sat": 0, "idempotency_key": "conservation-nutshell-cln"})
oracle = wait_operation("nutshell-cln-conservation")
if not oracle["artifact"]["content"].get("conserved"):
    fail(f"Nutshell CLN conservation failed: {oracle}")

call("proofstorm_lease_release", {"lease_id": "nutshell-cln-lease", "idempotency_key": "release-nutshell-cln-lease"})
closed_experiment = call("proofstorm_experiment_close", {"experiment_id": "nutshell-cln-experiment", "idempotency_key": "close-nutshell-cln-experiment"})
if closed_experiment.get("phase") != "closed":
    fail(f"Nutshell CLN experiment did not close: {closed_experiment}")
call("proofstorm_lab_close", {"instance_id": "nutshell-cln-instance"})
for _ in range(80):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-cln-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"Nutshell+CLN lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "rune": rune_before, "channel": channel["artifact"]["content"], "fund": fund_content, "round_trip": round_content, "conservation": oracle["artifact"]["content"], "status": status}, indent=2))
