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
        "PROOFSTORM_WORKSPACE": "nutshell-postgres-live",
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


request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "proofstorm-nutshell-postgres-live", "version": "0.1.0"}})
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "nutshell-postgres-live-lab",
    "components": [
        {"id": "chain", "kind": "bitcoin", "implementation": "bitcoin-core", "version": "30.0", "config_version": "bitcoin-core/30/v1", "control": "laboratory", "config": {}},
        {"id": "lightning", "kind": "lightning", "implementation": "lnd", "version": "0.20.0-beta", "config_version": "lnd/0.20/v1", "control": "laboratory", "config": {"alias": "proofstorm-nutshell-postgres"}},
        {"id": "database", "kind": "database", "implementation": "postgresql", "version": "17.11", "config_version": "postgresql/17/v1", "control": "laboratory", "config": {"database_name": "nutshell_mint", "storage_size": "2Gi"}},
        {"id": "mint", "kind": "mint", "implementation": "nutshell", "version": "0.20.2", "config_version": "nutshell-mint/0.20/v1", "control": "target", "config": {"name": "Proofstorm Nutshell PostgreSQL", "description": "Secret-backed persistence acceptance", "mint_quote_ttl_seconds": 701, "melt_quote_ttl_seconds": 131}},
    ],
    "links": [
        {"id": "lightning-chain", "kind": "chain_backend", "from": "lightning", "to": "chain", "binding": {"type": "chain", "network": "regtest"}},
        {"id": "mint-bolt11", "kind": "payment_backend", "from": "mint", "to": "lightning", "binding": {"type": "payment", "method": "bolt11", "unit": "sat"}},
        {"id": "mint-database", "kind": "database_backend", "from": "mint", "to": "database", "binding": {"type": "database", "role": "primary"}},
    ],
    "policy": {"allow": [], "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}},
}

call("proofstorm_lab_create", {"draft_id": "nutshell-postgres", "lab": lab, "idempotency_key": "create-nutshell-postgres"})
published = call("proofstorm_lab_publish", {"draft_id": "nutshell-postgres", "expected_version": 1, "idempotency_key": "publish-nutshell-postgres"})
call("proofstorm_lab_materialize", {"instance_id": "nutshell-postgres-instance", "revision_digest": published["digest"], "idempotency_key": "materialize-nutshell-postgres"})
for _ in range(200):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-postgres-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell+PostgreSQL lab did not become ready: {status}")

namespace = status["instance_namespace"]
public_config = json.loads(kubectl("get", "configmap/mint-config", "-n", namespace, "-o", "json"))["data"]
if "MINT_DATABASE" in public_config or "MINT_PRIVATE_KEY" in public_config or any("postgresql://" in value for value in public_config.values()):
    fail("public Nutshell configuration contains private database or mint credentials")

database_secret = kubectl("get", "secret/database-credentials", "-n", namespace, "-o", "json")
mint_secret = kubectl("get", "secret/mint-credentials", "-n", namespace, "-o", "json")
database_secret_digest = hashlib.sha256(database_secret.encode()).hexdigest()
mint_secret_digest = hashlib.sha256(mint_secret.encode()).hexdigest()
if set(json.loads(database_secret).get("data", {})) != {"DATABASE_URL", "POSTGRES_DB", "POSTGRES_PASSWORD", "POSTGRES_USER", "database.toml"}:
    fail("generated PostgreSQL Secret has an unexpected key contract")
if set(json.loads(mint_secret).get("data", {})) != {"MINT_PRIVATE_KEY", "PROOFSTORM_SECRET_KIND"}:
    fail("generated Nutshell Secret has an unexpected key contract")

settings_script = """
import json
from urllib.parse import urlparse
from cashu.core.settings import settings
url = urlparse(settings.mint_database)
print(json.dumps({'version': settings.version, 'name': settings.mint_info_name, 'database_host': url.hostname, 'database_name': url.path.lstrip('/'), 'private_key_length': len(settings.mint_private_key or '')}))
"""
settings = json.loads(kubectl("exec", "deployment/mint", "-n", namespace, "--", "python3", "-c", settings_script).strip())
if settings != {"version": "0.20.2", "name": "Proofstorm Nutshell PostgreSQL", "database_host": "database", "database_name": "nutshell_mint", "private_key_length": 64}:
    fail(f"live Nutshell PostgreSQL settings differ: {settings}")

sql = ('PGPASSWORD="$POSTGRES_PASSWORD" psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" '
       '-c "CREATE TABLE IF NOT EXISTS proofstorm_acceptance (id integer primary key, marker text not null);" '
       '-c "INSERT INTO proofstorm_acceptance VALUES (1, \'nutshell-persistent\') ON CONFLICT (id) DO UPDATE SET marker = EXCLUDED.marker;"')
kubectl("exec", "statefulset/database", "-n", namespace, "--", "sh", "-c", sql)
table_count = int(kubectl("exec", "statefulset/database", "-n", namespace, "--", "sh", "-c", 'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "SELECT count(*) FROM pg_tables WHERE schemaname = \'public\';"').strip())
if table_count < 2:
    fail(f"Nutshell did not initialize its PostgreSQL schema: {table_count} tables")

kubectl("rollout", "restart", "deployment/proofstormd", "-n", "proofstorm-system", capture=False)
kubectl("rollout", "status", "deployment/proofstormd", "-n", "proofstorm-system", "--timeout=120s", capture=False)
time.sleep(5)
if hashlib.sha256(kubectl("get", "secret/database-credentials", "-n", namespace, "-o", "json").encode()).hexdigest() != database_secret_digest:
    fail("controller restart rotated the PostgreSQL Secret")
if hashlib.sha256(kubectl("get", "secret/mint-credentials", "-n", namespace, "-o", "json").encode()).hexdigest() != mint_secret_digest:
    fail("controller restart rotated the Nutshell private key")

kubectl("rollout", "restart", "statefulset/database", "-n", namespace, capture=False)
kubectl("rollout", "status", "statefulset/database", "-n", namespace, "--timeout=180s", capture=False)
kubectl("rollout", "restart", "deployment/mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/mint", "-n", namespace, "--timeout=180s", capture=False)
persisted = kubectl("exec", "statefulset/database", "-n", namespace, "--", "sh", "-c", 'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "SELECT marker FROM proofstorm_acceptance WHERE id = 1;"').strip()
if persisted != "nutshell-persistent":
    fail(f"PostgreSQL state did not survive restart: {persisted!r}")
for _ in range(80):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-postgres-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"Nutshell+PostgreSQL lab did not recover after restart: {status}")

call("proofstorm_lab_close", {"instance_id": "nutshell-postgres-instance"})
for _ in range(80):
    status = call("proofstorm_lab_status", {"instance_id": "nutshell-postgres-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"Nutshell+PostgreSQL lab did not close: {status}")
process.terminate()
process.wait(timeout=10)
print(json.dumps({"revision": published["digest"], "settings": settings, "schema_tables": table_count, "restart_persistence": persisted, "status": status}, indent=2))
