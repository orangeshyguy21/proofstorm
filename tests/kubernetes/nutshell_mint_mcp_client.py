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
        "PROOFSTORM_WORKSPACE": "nutshell-mint-live",
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
        "clientInfo": {"name": "proofstorm-nutshell-mint-live", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "nutshell-mint-live-lab",
    "components": [
        {
            "id": "chain",
            "kind": "bitcoin",
            "implementation": "bitcoin-core",
            "version": "30.0",
            "config_version": "bitcoin-core/30/v1",
            "control": "laboratory",
            "config": {},
        },
        {
            "id": "lightning",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "lnd/0.20/v1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-nutshell-lnd"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "nutshell",
            "version": "0.20.2",
            "config_version": "nutshell-mint/0.20/v1",
            "control": "target",
            "config": {
                "name": "Proofstorm Nutshell Native",
                "description": "Native Nutshell and LND lab",
                "input_fee_ppk": 123,
                "mint_quote_ttl_seconds": 321,
                "melt_quote_ttl_seconds": 123,
                "max_mint_sat": 400000,
                "max_melt_sat": 300000,
                "max_balance_sat": 9000000,
                "global_rate_limit_per_minute": 77,
                "transaction_rate_limit_per_minute": 33,
                "lightning_fee_percent": 0.5,
                "lightning_reserve_fee_min_sat": 7,
            },
        },
    ],
    "links": [
        {
            "id": "lightning-chain",
            "kind": "chain_backend",
            "from": "lightning",
            "to": "chain",
            "binding": {"type": "chain", "network": "regtest"},
        },
        {
            "id": "mint-lightning-bolt11",
            "kind": "payment_backend",
            "from": "mint",
            "to": "lightning",
            "binding": {"type": "payment", "method": "bolt11", "unit": "sat"},
        },
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

call(2, "proofstorm_lab_create", {"draft_id": "nutshell-mint", "lab": lab, "idempotency_key": "create-nutshell-mint"})
published = call(
    3,
    "proofstorm_lab_publish",
    {"draft_id": "nutshell-mint", "expected_version": 1, "idempotency_key": "publish-nutshell-mint"},
)
nutshell_lock = next(entry for entry in published["lock"]["entries"] if entry["catalog_id"] == "nutshell")
if nutshell_lock["version"] != "0.20.2" or nutshell_lock["config_version"] != "nutshell-mint/0.20/v1":
    fail(f"unexpected Nutshell lock: {nutshell_lock}")
if nutshell_lock["image"] != "docker.io/cashubtc/nutshell@sha256:65e9cbe23aaa1aeb27ce7206fa854a80f39ce8db1c9121eaecfc053a22506574":
    fail(f"unexpected Nutshell image: {nutshell_lock}")

call(
    4,
    "proofstorm_lab_materialize",
    {
        "instance_id": "nutshell-mint-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-nutshell-mint",
    },
)
for identifier in range(5, 165):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "nutshell-mint-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell mint lab did not become ready: {status}")

namespace = status["instance_namespace"]
settings_script = """
import json
from cashu.core.settings import settings
print(json.dumps({
    'version': settings.version,
    'name': settings.mint_info_name,
    'description': settings.mint_info_description,
    'input_fee_ppk': settings.mint_input_fee_ppk,
    'mint_quote_ttl': settings.mint_quote_ttl,
    'melt_quote_ttl': settings.melt_quote_ttl,
    'max_mint_sat': settings.mint_max_mint_bolt11_sat,
    'max_melt_sat': settings.mint_max_melt_bolt11_sat,
    'max_balance_sat': settings.mint_max_balance,
    'global_rate_limit': settings.mint_global_rate_limit_per_minute,
    'transaction_rate_limit': settings.mint_transaction_rate_limit_per_minute,
    'lightning_fee_percent': settings.lightning_fee_percent,
    'lightning_reserve_fee_min': settings.lightning_reserve_fee_min,
    'backend': settings.mint_backend_bolt11_sat,
    'lnd_endpoint': settings.mint_lnd_rest_endpoint,
    'database': settings.mint_database,
    'private_key_length': len(settings.mint_private_key or ''),
}))
"""
native = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "python3", "-c", settings_script],
    check=True,
    capture_output=True,
    text=True,
)
settings = json.loads(native.stdout.strip())
expected = {
    "version": "0.20.2",
    "name": "Proofstorm Nutshell Native",
    "description": "Native Nutshell and LND lab",
    "input_fee_ppk": 123,
    "mint_quote_ttl": 321,
    "melt_quote_ttl": 123,
    "max_mint_sat": 400000,
    "max_melt_sat": 300000,
    "max_balance_sat": 9000000,
    "global_rate_limit": 77,
    "transaction_rate_limit": 33,
    "lightning_fee_percent": 0.5,
    "lightning_reserve_fee_min": 7,
    "backend": "LndRestWallet",
    "lnd_endpoint": "https://lightning:8080",
    "database": "/app/data",
    "private_key_length": 64,
}
if settings != expected:
    fail(f"live Nutshell settings differ: expected={expected!r} actual={settings!r}")

call(165, "proofstorm_lab_close", {"instance_id": "nutshell-mint-instance"})
for identifier in range(166, 226):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "nutshell-mint-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"Nutshell mint lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "nutshell": nutshell_lock, "settings": settings, "status": status}, indent=2))
