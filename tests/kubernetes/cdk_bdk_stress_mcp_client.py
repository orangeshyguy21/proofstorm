#!/usr/bin/env python3
import concurrent.futures
import json
import os
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
            f"cdk-bdk-stress-postgres-{run_id}"
            if postgres_enabled()
            else f"cdk-bdk-stress-{run_id}"
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
    [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, env=environment,
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
    with urllib.request.urlopen(
        urllib.request.Request(url, data=data, headers=headers), timeout=15
    ) as response:
        return json.load(response)


request(
    1,
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-cdk-bdk-stress", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "cdk-bdk-stress-lab",
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
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk-bdk",
            "version": "0.17.6",
            "config_version": "cdk-mintd-bdk/0.17/v1",
            "control": "target",
            "config": {
                "name": "Proofstorm CDK BDK",
                "description": "Native CDK embedded-BDK NUT-30 stress lab",
                "description_long": "Agent-authored long-form CDK metadata",
                "motd": "Proofstorm agents welcome",
                "icon_url": "https://proofstorm.invalid/cdk-bdk.png",
                "contact_email": "operator@proofstorm.invalid",
                "contact_nostr_public_key": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
                "tos_url": "https://proofstorm.invalid/terms",
                "enable_info_page": True,
                "input_fee_ppk": 321,
                "use_keyset_v2": False,
                "mint_quote_ttl_seconds": 900,
                "melt_quote_ttl_seconds": 180,
                "http_cache_ttl_seconds": 75,
                "http_cache_tti_seconds": 45,
                "max_inputs": 64,
                "max_outputs": 96,
                "min_mint_sat": 1200,
                "max_mint_sat": 5000,
                "min_melt_sat": 1300,
                "max_melt_sat": 6000,
            },
        },
    ],
    "links": [
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
augment_lab(lab, "proofstorm_bdk")

call(2, "proofstorm_lab_create", {"draft_id": "cdk-bdk", "lab": lab, "idempotency_key": "create-cdk-bdk"})
published = call(
    3,
    "proofstorm_lab_publish",
    {"draft_id": "cdk-bdk", "expected_version": 1, "idempotency_key": "publish-cdk-bdk"},
)
bdk_lock = next(entry for entry in published["lock"]["entries"] if entry["catalog_id"] == "cdk-bdk")
expected_image = "docker.io/cashubtc/mintd@sha256:e6018ad5ed3e9914c7892a53239cf602250e788c1fd7c055d4123803cee8dd00"
if bdk_lock["version"] != "0.17.6" or bdk_lock["image"] != expected_image:
    fail(f"unexpected CDK-BDK lock: {bdk_lock}")

status = call(
    4,
    "proofstorm_lab_materialize",
    {
        "instance_id": "cdk-bdk-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-cdk-bdk",
    },
)
for identifier in range(5, 165):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-bdk-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"CDK+BDK lab did not become ready: {status}")

namespace = status["instance_namespace"]
config = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cat", "/config/config.toml"],
    check=True, capture_output=True, text=True,
).stdout
for expected in [
    "enable_info_page = true",
    "input_fee_ppk = 321",
    "use_keyset_v2 = false",
    "mint_ttl = 900",
    "melt_ttl = 180",
    'backend = "memory"',
    "ttl = 75",
    "tti = 45",
    'description_long = "Agent-authored long-form CDK metadata"',
    'motd = "Proofstorm agents welcome"',
    'icon_url = "https://proofstorm.invalid/cdk-bdk.png"',
    'contact_email = "operator@proofstorm.invalid"',
    'contact_nostr_public_key = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"',
    'tos_url = "https://proofstorm.invalid/terms"',
    "max_inputs = 64",
    "max_outputs = 96",
    'ln_backend = "none"',
    "min_mint = 1200",
    "max_mint = 5000",
    "min_melt = 1300",
    "max_melt = 6000",
    'onchain_backend = "bdk"',
    "min_receive_amount_sat = 1200",
    'chain_source_type = "bitcoinrpc"',
    'bitcoind_rpc_host = "chain"',
    'num_confs = 1',
]:
    if expected not in config:
        fail(f"mint configuration is missing {expected!r}: {config}")
if "[lnd]" in config or "[cln]" in config or "[ldk_node]" in config:
    fail("on-chain-only mint rendered a Lightning backend")
postgres_tables = assert_materialized(namespace, config, "proofstorm_bdk")

version = subprocess.run(
    ["kubectl", "--context", "k3d-proofstorm", "exec", "deployment/mint", "-n", namespace, "--", "cdk-mintd", "--version"],
    check=True, capture_output=True, text=True,
).stdout.strip()
if "0.17.6" not in version:
    fail(f"live mint reports the wrong version: {version!r}")


def bitcoin(*arguments):
    command = [
        "kubectl", "--context", "k3d-proofstorm", "exec", "statefulset/chain",
        "-n", namespace, "--", "bitcoin-cli", "-regtest", "-rpcuser=proofstorm",
        "-rpcpassword=proofstorm-regtest-only", *arguments,
    ]
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()


bitcoin("createwallet", "default")
miner_address = bitcoin("-rpcwallet=default", "getnewaddress")
bitcoin("-rpcwallet=default", "generatetoaddress", "101", miner_address)

port = local_port()
forward = subprocess.Popen(
    ["kubectl", "--context", "k3d-proofstorm", "port-forward", "-n", namespace, "service/mint", f"{port}:3338"],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
)
base_url = f"http://127.0.0.1:{port}"
try:
    for _ in range(30):
        try:
            info = http_json(base_url + "/v1/info")
            break
        except urllib.error.URLError:
            if forward.poll() is not None:
                fail("mint port-forward stopped: " + forward.stderr.read())
            time.sleep(1)
    else:
        fail("mint info endpoint was not reachable")
    if "onchain" not in json.dumps(info, sort_keys=True).lower():
        fail(f"live mint does not advertise on-chain support: {info}")
    info_text = json.dumps(info, sort_keys=True)
    for expected in [
        "Agent-authored long-form CDK metadata",
        "Proofstorm agents welcome",
        "https://proofstorm.invalid/cdk-bdk.png",
        "operator@proofstorm.invalid",
        "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        "https://proofstorm.invalid/terms",
    ]:
        if expected not in info_text:
            fail(f"live NUT-06 mint info is missing {expected!r}: {info}")

    public_key = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"

    def create_quote(_):
        return http_json(
            base_url + "/v1/mint/quote/onchain",
            {"unit": "sat", "pubkey": public_key},
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        quotes = list(executor.map(create_quote, range(24)))
    addresses = [quote["request"] for quote in quotes]
    if len(set(addresses)) != 24 or not all(address.startswith("bcrt1") for address in addresses):
        fail(f"concurrent NUT-30 quotes did not return unique regtest addresses: {addresses}")

    try:
        http_json(base_url + "/v1/mint/quote/onchain", {"unit": "sat"})
        fail("on-chain quote without a NUT-20 pubkey was accepted")
    except urllib.error.HTTPError as error:
        if error.code != 400:
            raise

    funded = quotes[:3]
    dust = quotes[3]
    for quote in funded:
        bitcoin("-rpcwallet=default", "sendtoaddress", quote["request"], "0.00001200")
    bitcoin("-rpcwallet=default", "sendtoaddress", dust["request"], "0.00001199")
    bitcoin("-rpcwallet=default", "generatetoaddress", "1", miner_address)

    def quote_status(quote):
        return http_json(base_url + f"/v1/mint/quote/onchain/{quote['quote']}")

    for _ in range(60):
        settled = [quote_status(quote) for quote in funded]
        if all(item.get("amount_paid", 0) >= 1200 for item in settled):
            break
        time.sleep(1)
    else:
        fail(f"funded on-chain quotes did not settle after one confirmation: {settled}")
    dust_status = quote_status(dust)
    if dust_status.get("amount_paid", 0) != 0:
        fail(f"sub-minimum on-chain deposit was credited: {dust_status}")

    if postgres_enabled():
        seed_sentinel(namespace, "bdk-persistent")
        restart_database(namespace)
    subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", "rollout", "restart", "deployment/mint", "-n", namespace],
        check=True,
    )
    subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", "rollout", "status", "deployment/mint", "-n", namespace, "--timeout=180s"],
        check=True,
    )
    for _ in range(30):
        try:
            persisted = [quote_status(quote) for quote in funded]
            if all(item.get("amount_paid", 0) >= 1200 for item in persisted):
                break
        except urllib.error.URLError:
            pass
        time.sleep(1)
    else:
        fail(f"settled quote state did not survive mint restart: {persisted}")
    verify_sentinel(namespace, "bdk-persistent")
finally:
    forward.terminate()
    forward.wait(timeout=10)

call(165, "proofstorm_lab_close", {"instance_id": "cdk-bdk-instance"})
for identifier in range(166, 226):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-bdk-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"CDK+BDK lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "revision": published["digest"],
            "cdk_bdk": bdk_lock,
            "version": version,
            "quotes_created": len(quotes),
            "unique_addresses": len(set(addresses)),
            "settled_quotes": len(persisted),
            "dust_amount_paid": dust_status.get("amount_paid", 0),
            "storage": "postgres" if postgres_enabled() else "sqlite",
            "postgres_schema_tables": postgres_tables,
            "status": status,
        },
        indent=2,
    )
)
