#!/usr/bin/env python3
import hashlib
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
        "PROOFSTORM_WORKSPACE": "cross-mint-wallet-live",
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


def wait_operation(operation_id, attempts=160):
    for _ in range(attempts):
        operation = call("proofstorm_operation_status", {"operation_id": operation_id})
        if operation["phase"] == "succeeded":
            return operation
        if operation["phase"] in {"failed", "cancelled"}:
            fail(f"operation {operation_id} failed: {operation}")
        time.sleep(3)
    fail(f"operation {operation_id} did not finish")


request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "proofstorm-cross-mint-wallet-live", "version": "0.1.0"}})
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "cross-mint-wallet-live-lab",
    "components": [
        {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
        {"id": "mint-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cross-mint"}},
        {"id": "payer-lnd", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-cross-payer"}},
        {"id": "cache", "kind": "database", "implementation": "redis", "version": "8.10.1", "config_version": "redis/8.10/v1", "control": "laboratory", "config": {"maxmemory_mb": 64}},
        {"id": "cdk-mint", "kind": "mint", "implementation": "cdk", "version": "0.17.6", "config_version": "cdk-mintd/0.17/v1", "control": "target", "config": {"name": "Proofstorm CDK Cross-Parity", "description": "Cross-implementation wallet acceptance"}},
        {"id": "nutshell-mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.3", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell Cross-Parity", "description": "Cross-implementation wallet acceptance", "redis_cache_ttl_seconds": 900}},
        {"id": "cdk-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
        {"id": "nutshell-wallet", "kind": "wallet", "implementation": "nutshell-wallet", "version": "0.20.3", "config_version": "nutshell-wallet/0.20/v1", "control": "laboratory", "config": {}},
    ],
    "links": [
        {"id": "mint-lnd-chain", "kind": "chain_backend", "from": "mint-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "payer-lnd-chain", "kind": "chain_backend", "from": "payer-lnd", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "cdk-bolt11", "kind": "payment_backend", "from": "cdk-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
        {"id": "nutshell-bolt11", "kind": "payment_backend", "from": "nutshell-mint", "to": "mint-lnd", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
        {"id": "nutshell-cache", "kind": "database_backend", "from": "nutshell-mint", "to": "cache", "binding": {"type": "database", "role": "cache"}},
    ],
    "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}},
}

call("proofstorm_lab_create", {"draft_id": "cross-mint-wallet", "lab": lab, "idempotency_key": "create-cross-mint-wallet"})
published = call("proofstorm_lab_publish", {"draft_id": "cross-mint-wallet", "expected_version": 1, "idempotency_key": "publish-cross-mint-wallet", "include_revision": True})
locks = {entry["component_id"]: entry for entry in published["lock"]["entries"]}
expected_locks = {
    "cache": ("redis", "8.10.1", "redis/8.10/v1"),
    "cdk-mint": ("cdk", "0.17.6", "cdk-mintd/0.17/v1"),
    "nutshell-mint": ("nutshell", "0.20.3", "nutshell-mint/0.20/v1"),
}
for component_id, expected in expected_locks.items():
    entry = locks.get(component_id)
    actual = (entry.get("catalog_id"), entry.get("version"), entry.get("config_version")) if entry else None
    if actual != expected or "@sha256:" not in entry.get("image", ""):
        fail(f"unexpected pinned lock for {component_id}: {entry}")

call("proofstorm_lab_materialize", {"instance_id": "cross-mint-wallet-instance", "revision_digest": published["digest"], "idempotency_key": "materialize-cross-mint-wallet"})
for _ in range(200):
    status = call("proofstorm_lab_status", {"instance_id": "cross-mint-wallet-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"cross-implementation lab did not become ready: {status}")

expected_ready = {component["id"] for component in lab["components"]}
component_status = call(
    "proofstorm_lab_component_status_list",
    {"instance_id": "cross-mint-wallet-instance", "limit": 50},
)["components"]
actual_ready = {component["id"] for component in component_status if component["ready"]}
if actual_ready != expected_ready:
    fail(f"cross-implementation topology is not fully ready: {component_status}")

namespace = status["instance_namespace"]
public_config = json.loads(kubectl("get", "configmap/nutshell-mint-config", "-n", namespace, "-o", "json"))["data"]
if public_config.get("MINT_REDIS_CACHE_ENABLED") != "TRUE" or public_config.get("MINT_REDIS_CACHE_TTL") != "900" or public_config.get("MINT_REDIS_CACHE_CLUSTER") != "FALSE":
    fail(f"Nutshell Redis cache settings were not materialized: {public_config}")
if "MINT_REDIS_CACHE_URL" in public_config or any("redis://" in value for value in public_config.values()):
    fail("public Nutshell configuration contains the private Redis URL")
cache_secret = kubectl("get", "secret/cache-credentials", "-n", namespace, "-o", "json")
cache_secret_digest = hashlib.sha256(cache_secret.encode()).hexdigest()
if set(json.loads(cache_secret).get("data", {})) != {"PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"}:
    fail("generated Redis Secret has an unexpected key contract")
settings_script = """
import json
from urllib.parse import urlparse
from cashu.core.settings import settings
url = urlparse(settings.mint_redis_cache_url)
print(json.dumps({
    'enabled': settings.mint_redis_cache_enabled,
    'host': url.hostname,
    'password_length': len(url.password or ''),
    'ttl': settings.mint_redis_cache_ttl,
    'cluster': settings.mint_redis_cache_cluster,
}))
"""
cache_settings = json.loads(kubectl("exec", "deployment/nutshell-mint", "-n", namespace, "--", "python3", "-c", settings_script).strip())
if cache_settings != {"enabled": True, "host": "cache", "password_length": 64, "ttl": 900, "cluster": False}:
    fail(f"live Nutshell Redis settings differ: {cache_settings}")

call("proofstorm_experiment_create", {"experiment_id": "cross-mint-experiment", "instance_id": "cross-mint-wallet-instance", "idempotency_key": "create-cross-mint-experiment"})
call("proofstorm_lease_acquire", {"experiment_id": "cross-mint-experiment", "lease_id": "cross-mint-lease", "duration_seconds": 1200, "max_actions": 12, "idempotency_key": "acquire-cross-mint-lease"})

accepted_bootstrap = call(
    "proofstorm_liquidity_bootstrap",
    {
        "instance_id": "cross-mint-wallet-instance",
        "experiment_id": "cross-mint-experiment",
        "lease_id": "cross-mint-lease",
        "operation_id": "cross-mint-bootstrap",
        "chain": "chain",
        "mint_lightning": "mint-lnd",
        "payer_lightning": "payer-lnd",
        "funding_sat": 50000000,
        "channel_sat": 10000000,
        "push_sat": 5000000,
        "idempotency_key": "bootstrap-cross-mint",
    },
)
bootstrap = wait_operation("cross-mint-bootstrap")
if not bootstrap["artifact"]["content"].get("ready"):
    fail(f"liquidity bootstrap artifact is invalid: {bootstrap}")

results = {}
for implementation, mint, wallet in [
    ("cdk", "cdk-mint", "cdk-wallet"),
    ("nutshell", "nutshell-mint", "nutshell-wallet"),
]:
    prefix = f"{implementation}-wallet"
    common = {
        "instance_id": "cross-mint-wallet-instance",
        "experiment_id": "cross-mint-experiment",
        "lease_id": "cross-mint-lease",
        "wallet": wallet,
        "mint": mint,
    }
    call("proofstorm_wallet_initialize", {**common, "operation_id": f"{prefix}-initialize", "idempotency_key": f"{prefix}-initialize"})
    initialized = wait_operation(f"{prefix}-initialize")
    if not initialized["artifact"]["content"].get("initialized"):
        fail(f"{implementation} wallet initialization failed: {initialized}")

    call("proofstorm_wallet_balance", {**common, "operation_id": f"{prefix}-balance", "idempotency_key": f"{prefix}-balance"})
    balance = wait_operation(f"{prefix}-balance")
    if balance["artifact"]["content"].get("balance_sat") != 0:
        fail(f"{implementation} wallet did not start empty: {balance}")

    call("proofstorm_wallet_fund", {**common, "operation_id": f"{prefix}-fund", "payer_lightning": "payer-lnd", "amount_sat": 1000, "idempotency_key": f"{prefix}-fund"})
    funded = wait_operation(f"{prefix}-fund")
    fund_content = funded["artifact"]["content"]
    if fund_content.get("funded_sat") != 1000 or fund_content.get("balance_sat") != 1000:
        fail(f"{implementation} wallet funding failed: {funded}")

    call("proofstorm_wallet_round_trip", {**common, "operation_id": f"{prefix}-round-trip", "payer_lightning": "payer-lnd", "amount_sat": 1000, "tolerance_sat": 100, "idempotency_key": f"{prefix}-round-trip"})
    round_trip = wait_operation(f"{prefix}-round-trip")
    round_content = round_trip["artifact"]["content"]
    if round_content.get("inflation") is not False or round_content.get("minted_sat") != 1000:
        fail(f"{implementation} wallet round trip failed: {round_trip}")

    call("proofstorm_conservation_oracle", {**common, "operation_id": f"{prefix}-conservation", "expected_sat": round_content["balance_after_swap_sat"], "tolerance_sat": 0, "idempotency_key": f"{prefix}-conservation"})
    oracle = wait_operation(f"{prefix}-conservation")
    if not oracle["artifact"]["content"].get("conserved"):
        fail(f"{implementation} conservation check failed: {oracle}")
    results[implementation] = {"fund": fund_content, "round_trip": round_content, "conservation": oracle["artifact"]["content"]}

cache_size = int(kubectl("exec", "deployment/cache", "-n", namespace, "--", "sh", "-c", 'redis-cli --no-auth-warning -a "$REDIS_PASSWORD" dbsize').strip())
if cache_size < 1:
    fail("Nutshell wallet workflow did not populate Redis")
kubectl("exec", "deployment/cache", "-n", namespace, "--", "sh", "-c", 'redis-cli --no-auth-warning -a "$REDIS_PASSWORD" set proofstorm:restart-canary present >/dev/null')

kubectl("rollout", "restart", "deployment/proofstormd", "-n", "proofstorm-system", capture=False)
kubectl("rollout", "status", "deployment/proofstormd", "-n", "proofstorm-system", "--timeout=120s", capture=False)
time.sleep(5)
if hashlib.sha256(kubectl("get", "secret/cache-credentials", "-n", namespace, "-o", "json").encode()).hexdigest() != cache_secret_digest:
    fail("controller restart rotated the Redis credentials")

kubectl("rollout", "restart", "deployment/cache", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/cache", "-n", namespace, "--timeout=180s", capture=False)
canary = kubectl("exec", "deployment/cache", "-n", namespace, "--", "sh", "-c", 'redis-cli --no-auth-warning -a "$REDIS_PASSWORD" exists proofstorm:restart-canary').strip()
if canary != "0":
    fail(f"ephemeral Redis cache survived restart unexpectedly: {canary}")
kubectl("rollout", "restart", "deployment/nutshell-mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/nutshell-mint", "-n", namespace, "--timeout=180s", capture=False)
for _ in range(80):
    status = call("proofstorm_lab_status", {"instance_id": "cross-mint-wallet-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"cross-implementation lab did not recover after Redis restart: {status}")

call("proofstorm_lease_release", {"lease_id": "cross-mint-lease", "idempotency_key": "release-cross-mint-lease"})
closed_experiment = call("proofstorm_experiment_close", {"experiment_id": "cross-mint-experiment", "idempotency_key": "close-cross-mint-experiment"})
if closed_experiment.get("phase") != "closed":
    fail(f"cross-mint experiment did not close: {closed_experiment}")

call("proofstorm_lab_close", {"instance_id": "cross-mint-wallet-instance"})
for _ in range(80):
    status = call("proofstorm_lab_status", {"instance_id": "cross-mint-wallet-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"cross-implementation lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "bootstrap_action": accepted_bootstrap["resource_name"], "redis": {"settings": cache_settings, "populated_keys": cache_size, "ephemeral_restart_canary": canary}, "implementations": results, "status": status}, indent=2))
