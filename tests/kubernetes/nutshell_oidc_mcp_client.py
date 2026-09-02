#!/usr/bin/env python3
import base64
import hashlib
import json
import os
import subprocess
import sys
import time


def fail(message):
    raise RuntimeError(message)


def kubectl(*arguments, capture=True, input_text=None):
    result = subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", *arguments],
        check=False,
        capture_output=True,
        input=input_text,
        text=True,
    )
    if result.returncode:
        fail(f"kubectl {' '.join(arguments)} failed:\n{result.stderr[-4000:]}")
    if not capture:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
    return result.stdout


def secret_data(namespace, name):
    secret = json.loads(kubectl("get", f"secret/{name}", "-n", namespace, "-o", "json"))
    return {
        key: base64.b64decode(value).decode()
        for key, value in secret.get("data", {}).items()
    }


def exec_python(namespace, script, payload):
    output = kubectl(
        "exec",
        "-i",
        "deployment/mint",
        "-n",
        namespace,
        "--",
        "python3",
        "-c",
        script,
        input_text=json.dumps(payload),
    )
    return json.loads(output.strip().splitlines()[-1])


binary, database = sys.argv[1:3]
environment = os.environ.copy()
environment.update(
    {
        "PROOFSTORM_DB": database,
        "PROOFSTORM_WORKSPACE": "nutshell-oidc-live",
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


request(
    "initialize",
    {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "proofstorm-nutshell-oidc-live", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "nutshell-oidc-live-lab",
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
            "config": {"alias": "proofstorm-nutshell-oidc"},
        },
        {
            "id": "identity-db",
            "kind": "database",
            "implementation": "postgresql",
            "version": "17.11",
            "config_version": "postgresql/17/v1",
            "control": "laboratory",
            "config": {"database_name": "keycloak", "storage_size": "2Gi"},
        },
        {
            "id": "identity",
            "kind": "identity_provider",
            "implementation": "keycloak",
            "version": "25.0.6",
            "config_version": "keycloak/25/v1",
            "control": "laboratory",
            "config": {"access_token_lifespan_seconds": 600},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "nutshell",
            "version": "0.20.3",
            "config_version": "nutshell-mint/0.20/v1",
            "control": "target",
            "config": {
                "name": "Proofstorm Authenticated Nutshell",
                "description": "Live NUT-21 and NUT-22 acceptance",
                "auth_rate_limit_per_minute": 2,
                "auth_max_blind_tokens": 3,
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
            "id": "mint-lightning",
            "kind": "payment_backend",
            "from": "mint",
            "to": "lightning",
            "binding": {"type": "payment", "method": "bolt11", "unit": "sat"},
        },
        {
            "id": "identity-database",
            "kind": "database_backend",
            "from": "identity",
            "to": "identity-db",
            "binding": {"type": "database", "role": "primary"},
        },
        {
            "id": "mint-identity",
            "kind": "authentication_backend",
            "from": "mint",
            "to": "identity",
            "binding": {"type": "authentication", "protocol": "oidc"},
        },
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

call(
    "proofstorm_lab_create",
    {"draft_id": "nutshell-oidc", "lab": lab, "idempotency_key": "create-nutshell-oidc"},
)
published = call(
    "proofstorm_lab_publish",
    {
        "draft_id": "nutshell-oidc",
        "expected_version": 1,
        "idempotency_key": "publish-nutshell-oidc",
    },
)
locks = {entry["catalog_id"]: entry for entry in published["lock"]["entries"]}
expected_locks = {
    "nutshell": ("0.20.3", "nutshell-mint/0.20/v1"),
    "keycloak": ("25.0.6", "keycloak/25/v1"),
    "postgresql": ("17.11", "postgresql/17/v1"),
}
for backend, expected in expected_locks.items():
    actual = locks[backend]
    if (actual["version"], actual["config_version"]) != expected:
        fail(f"unexpected {backend} lock: {actual}")

call(
    "proofstorm_lab_materialize",
    {
        "instance_id": "nutshell-oidc-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-nutshell-oidc",
    },
)
for _ in range(240):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-oidc-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell OIDC lab did not become ready: {status}")

namespace = status["instance_namespace"]


def bitcoin(*arguments):
    return kubectl(
        "exec",
        "statefulset/chain",
        "-n",
        namespace,
        "--",
        "bitcoin-cli",
        "-regtest",
        "-rpcuser=proofstorm",
        "-rpcpassword=proofstorm-regtest-only",
        *arguments,
    ).strip()


bitcoin("createwallet", "default")
miner_address = bitcoin("-rpcwallet=default", "getnewaddress")
bitcoin("-rpcwallet=default", "generatetoaddress", "101", miner_address)
for _ in range(60):
    lightning_info = json.loads(
        kubectl(
            "exec",
            "statefulset/lightning",
            "-n",
            namespace,
            "--",
            "lncli",
            "--lnddir=/home/lnd/.lnd",
            "--network=regtest",
            "getinfo",
        )
    )
    if lightning_info.get("synced_to_chain") and int(lightning_info.get("block_height", 0)) >= 101:
        break
    time.sleep(1)
else:
    fail(f"LND did not synchronize to the acceptance chain: {lightning_info}")

mint_config = json.loads(kubectl("get", "configmap/mint-config", "-n", namespace, "-o", "json"))["data"]
expected_config = {
    "MINT_REQUIRE_AUTH": "TRUE",
    "MINT_AUTH_OICD_CLIENT_ID": "cashu-client",
    "MINT_AUTH_OICD_DISCOVERY_URL": "http://identity:8080/realms/proofstorm/.well-known/openid-configuration",
    "MINT_AUTH_RATE_LIMIT_PER_MINUTE": "2",
    "MINT_AUTH_MAX_BLIND_TOKENS": "3",
    "MINT_AUTH_DATABASE": "/app/data",
}
for key, expected in expected_config.items():
    if mint_config.get(key) != expected:
        fail(f"live mint config {key} differs: {mint_config.get(key)!r}")

identity_secret_json = kubectl("get", "secret/identity-credentials", "-n", namespace, "-o", "json")
database_secret_json = kubectl("get", "secret/identity-db-credentials", "-n", namespace, "-o", "json")
identity_secret_digest = hashlib.sha256(identity_secret_json.encode()).hexdigest()
database_secret_digest = hashlib.sha256(database_secret_json.encode()).hexdigest()
identity_credentials = secret_data(namespace, "identity-credentials")
expected_secret_keys = {
    "PROOFSTORM_SECRET_KIND",
    "OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS",
    "KEYCLOAK_ADMIN_PASSWORD",
    "OIDC_TEST_USERNAME",
    "OIDC_TEST_PASSWORD",
    "realm.json",
}
if set(identity_credentials) != expected_secret_keys:
    fail(f"generated Keycloak Secret has unexpected keys: {set(identity_credentials)}")
realm = json.loads(identity_credentials["realm.json"])
if realm["realm"] != "proofstorm" or realm["accessTokenLifespan"] != 600:
    fail("generated Keycloak realm does not preserve authored policy")
if realm["clients"][0]["clientId"] != "cashu-client" or not realm["clients"][0]["directAccessGrantsEnabled"]:
    fail("generated Keycloak client does not support the acceptance login flow")
if "basic" not in realm["clients"][0].get("defaultClientScopes", []):
    fail("generated Keycloak client does not request the standard subject claim")
if any(value in json.dumps(mint_config) for value in [identity_credentials["OIDC_TEST_PASSWORD"], identity_credentials["KEYCLOAK_ADMIN_PASSWORD"]]):
    fail("public mint configuration contains generated Keycloak credentials")

pre_restart_driver = r'''
import asyncio, hashlib, json, os, sys
import httpx
from cashu.core.crypto.secp import PrivateKey
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

def expect_code(response, code, label):
    try:
        body = response.json()
    except Exception:
        body = {"body": response.text}
    if response.status_code < 400 or body.get("code") != code:
        raise RuntimeError(f"{label}: expected code {code}, got {response.status_code} {body}")

async def main():
    async with httpx.AsyncClient(timeout=30) as client:
        info_response = await client.get(mint_url + "/v1/info")
        info_response.raise_for_status()
        info = info_response.json()
        nut21 = info["nuts"]["21"]
        nut22 = info["nuts"]["22"]
        if nut21["client_id"] != "cashu-client" or nut22["bat_max_mint"] != 3:
            raise RuntimeError(f"unexpected auth advertisement: {nut21} {nut22}")
        discovery = (await client.get(nut21["openid_discovery"])).json()
        rejected = await client.post(discovery["token_endpoint"], data={
            "grant_type": "password", "client_id": "cashu-client",
            "username": payload["username"], "password": "not-the-generated-password",
            "scope": "openid",
        })
        if rejected.status_code < 400:
            raise RuntimeError("Keycloak accepted an invalid password")
        quote_payload = {"amount": 1, "unit": "sat"}
        missing_blind = await client.post(mint_url + "/v1/mint/quote/bolt11", json=quote_payload)
        if missing_blind.status_code < 400:
            raise RuntimeError("protected quote accepted a missing BAT")
        invalid_blind = await client.post(
            mint_url + "/v1/mint/quote/bolt11", json=quote_payload,
            headers={"Blind-auth": "authAinvalid"},
        )
        expect_code(invalid_blind, 81002, "invalid BAT")
        missing_clear = await client.post(mint_url + "/v1/auth/blind/mint", json={"outputs": []})
        if missing_clear.status_code < 400:
            raise RuntimeError("blind mint accepted a missing CAT")
        invalid_clear = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": []},
            headers={"Clear-auth": "not-a-jwt"},
        )
        expect_code(invalid_clear, 80002, "invalid CAT")

    wallet_dir = "/tmp/proofstorm-nutshell-auth-pre"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=False, force_auth=True)
    if not required:
        raise RuntimeError("Nutshell wallet did not detect required authentication")
    claims = __import__("jwt").decode(
        wallet.oidc_client.access_token, options={"verify_signature": False}
    )
    if not claims.get("sub") or claims.get("iss") != discovery["issuer"] or claims.get("azp") != "cashu-client":
        metadata = {key: claims.get(key) for key in ("iss", "azp", "sub", "scope", "typ")}
        raise RuntimeError(f"Keycloak token claims do not satisfy Nutshell: {metadata}")
    def outputs(count):
        secrets = [hashlib.sha256(os.urandom(32)).hexdigest() for _ in range(count)]
        blinded, _ = wallet._construct_outputs(
            [1] * count, secrets, [PrivateKey(os.urandom(32)) for _ in range(count)]
        )
        return [entry.model_dump() for entry in blinded]
    async with httpx.AsyncClient(timeout=30) as client:
        headers = {"Clear-auth": wallet.oidc_client.access_token}
        excessive = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(4)}, headers=headers
        )
        expect_code(excessive, 81003, "BAT maximum")
        accepted = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(1)}, headers=headers
        )
        accepted.raise_for_status()
        signatures = accepted.json().get("signatures", [])
        if len(signatures) != 1 or not signatures[0].get("dleq"):
            raise RuntimeError(f"valid CAT did not mint one DLEQ-backed BAT: {accepted.text}")
        limited = await client.post(
            mint_url + "/v1/auth/blind/mint", json={"outputs": outputs(1)}, headers=headers
        )
        expect_code(limited, 81004, "CAT rate limit")
    print(json.dumps({
        "advertised_nut21": True, "advertised_nut22": True,
        "invalid_oidc_password_rejected": True, "missing_cat_rejected": True,
        "invalid_cat_code": 80002, "missing_bat_rejected": True,
        "invalid_bat_code": 81002, "valid_cat_bat_mint": True,
        "bat_max_code": 81003, "rate_limit_code": 81004,
    }))

asyncio.run(main())
'''
negative_result = exec_python(
    namespace,
    pre_restart_driver,
    {
        "username": identity_credentials["OIDC_TEST_USERNAME"],
        "password": identity_credentials["OIDC_TEST_PASSWORD"],
    },
)

kubectl("rollout", "restart", "deployment/proofstormd", "-n", "proofstorm-system", capture=False)
kubectl("rollout", "status", "deployment/proofstormd", "-n", "proofstorm-system", "--timeout=120s", capture=False)
time.sleep(5)
if hashlib.sha256(kubectl("get", "secret/identity-credentials", "-n", namespace, "-o", "json").encode()).hexdigest() != identity_secret_digest:
    fail("controller restart rotated the Keycloak Secret")
if hashlib.sha256(kubectl("get", "secret/identity-db-credentials", "-n", namespace, "-o", "json").encode()).hexdigest() != database_secret_digest:
    fail("controller restart rotated the Keycloak PostgreSQL Secret")

kubectl("rollout", "restart", "statefulset/identity-db", "-n", namespace, capture=False)
kubectl("rollout", "status", "statefulset/identity-db", "-n", namespace, "--timeout=240s", capture=False)
kubectl("rollout", "restart", "deployment/identity", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/identity", "-n", namespace, "--timeout=300s", capture=False)
kubectl("rollout", "restart", "deployment/mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/mint", "-n", namespace, "--timeout=240s", capture=False)

post_restart_driver = r'''
import asyncio, json, os, sys
import httpx
from cashu.core.base import AuthProof
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

async def main():
    wallet_dir = "/tmp/proofstorm-nutshell-auth-post"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    required = await wallet.init_auth_wallet(mint_auth_proofs=True, force_auth=True)
    if not required or len(wallet.proofs) != 3:
        raise RuntimeError(f"Nutshell wallet minted {len(wallet.proofs)} BATs after restart")
    if any(proof.dleq is None for proof in wallet.proofs):
        raise RuntimeError("mint returned a BAT without its NUT-12 DLEQ proof")
    token = AuthProof.from_proof(wallet.proofs[0]).to_base64()
    async with httpx.AsyncClient(timeout=30) as client:
        quote = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": token},
        )
        quote.raise_for_status()
        quote_id = quote.json().get("quote")
        if not quote_id:
            raise RuntimeError(f"protected quote did not return an id: {quote.text}")
    with open("/app/data/proofstorm-oidc-used-bat", "w", encoding="utf-8") as handle:
        os.chmod(handle.fileno(), 0o600)
        handle.write(token)
    print(json.dumps({"nutshell_client_bats": 3, "dleq": True, "protected_quote": True}))

asyncio.run(main())
'''
nutshell_result = exec_python(
    namespace,
    post_restart_driver,
    {
        "username": identity_credentials["OIDC_TEST_USERNAME"],
        "password": identity_credentials["OIDC_TEST_PASSWORD"],
    },
)

kubectl("rollout", "restart", "deployment/mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/mint", "-n", namespace, "--timeout=240s", capture=False)

replay_driver = r'''
import asyncio, json, os, sys
import httpx
from cashu.core.base import AuthProof
from cashu.wallet.auth.auth import WalletAuth

payload = json.load(sys.stdin)
mint_url = "http://127.0.0.1:3338"

async def main():
    with open("/app/data/proofstorm-oidc-used-bat", encoding="utf-8") as handle:
        spent_token = handle.read()
    async with httpx.AsyncClient(timeout=30) as client:
        replay = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": spent_token},
        )
        body = replay.json()
        if replay.status_code < 400 or body.get("code") != 81002:
            raise RuntimeError(f"spent BAT replay survived mint restart: {replay.status_code} {body}")
    wallet_dir = "/tmp/proofstorm-nutshell-auth-recovered"
    os.makedirs(wallet_dir, mode=0o700, exist_ok=True)
    wallet = await WalletAuth.with_db(
        url=mint_url, db=wallet_dir, username=payload["username"],
        password=payload["password"], client_id="cashu-client",
    )
    await wallet.init_auth_wallet(mint_auth_proofs=True, force_auth=True)
    if len(wallet.proofs) != 3:
        raise RuntimeError("mint could not issue fresh BATs after restart")
    fresh = AuthProof.from_proof(wallet.proofs[0]).to_base64()
    async with httpx.AsyncClient(timeout=30) as client:
        recovered = await client.post(
            mint_url + "/v1/mint/quote/bolt11",
            json={"amount": 1, "unit": "sat"}, headers={"Blind-auth": fresh},
        )
        recovered.raise_for_status()
    os.remove("/app/data/proofstorm-oidc-used-bat")
    print(json.dumps({"spent_bat_replay_code": 81002, "fresh_cat": True, "fresh_bat": True}))

asyncio.run(main())
'''
recovery_result = exec_python(
    namespace,
    replay_driver,
    {
        "username": identity_credentials["OIDC_TEST_USERNAME"],
        "password": identity_credentials["OIDC_TEST_PASSWORD"],
    },
)

for _ in range(100):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-oidc-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell OIDC lab did not recover: {status}")

call("proofstorm_lab_close", {"instance_id": "nutshell-oidc-instance"})
for _ in range(100):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-oidc-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"Nutshell OIDC lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "revision": published["digest"],
            "negative_contract": negative_result,
            "nutshell": nutshell_result,
            "recovery": recovery_result,
            "status": status,
        },
        indent=2,
    )
)
