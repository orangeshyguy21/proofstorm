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
        "PROOFSTORM_WORKSPACE": "cdk-postgres-live",
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
        "clientInfo": {"name": "proofstorm-cdk-postgres-live", "version": "0.1.0"},
    },
)
process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
process.stdin.flush()

catalog = call(2, "proofstorm_catalog_list", {})
postgres = next((entry for entry in catalog["entries"] if entry["id"] == "postgresql"), None)
if not postgres or postgres["version"] != "17.11":
    fail(f"PostgreSQL 17.11 is absent from the catalog: {postgres}")
cdk = next(entry for entry in catalog["entries"] if entry["id"] == "cdk")
if "postgres" not in cdk["features"] or "postgres" not in cdk["support_matrix"]["storage"]:
    fail("CDK does not advertise its typed PostgreSQL support")

lab = {
    "api_version": "proofstorm/v1alpha1",
    "name": "cdk-postgres-live-lab",
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
            "id": "mint-lnd",
            "kind": "lightning",
            "implementation": "lnd",
            "version": "0.20.0-beta",
            "config_version": "lnd/0.20/v1",
            "control": "laboratory",
            "config": {"alias": "proofstorm-postgres-lnd"},
        },
        {
            "id": "database",
            "kind": "database",
            "implementation": "postgresql",
            "version": "17.11",
            "config_version": "postgresql/17/v1",
            "control": "laboratory",
            "config": {"database_name": "proofstorm_mint", "storage_size": "2Gi"},
        },
        {
            "id": "mint",
            "kind": "mint",
            "implementation": "cdk",
            "version": "0.17.6",
            "config_version": "cdk-mintd/0.17/v1",
            "control": "target",
            "config": {
                "name": "Proofstorm CDK PostgreSQL",
                "description": "Secret-backed PostgreSQL persistence acceptance",
                "mint_quote_ttl_seconds": 601,
                "melt_quote_ttl_seconds": 121,
            },
        },
    ],
    "links": [
        {
            "id": "lnd-chain",
            "kind": "chain_backend",
            "from": "mint-lnd",
            "to": "chain",
            "binding": {"type": "chain", "network": "regtest"},
        },
        {
            "id": "mint-bolt11",
            "kind": "payment_backend",
            "from": "mint",
            "to": "mint-lnd",
            "binding": {"type": "payment", "method": "bolt11", "unit": "sat"},
        },
        {
            "id": "mint-database",
            "kind": "database_backend",
            "from": "mint",
            "to": "database",
            "binding": {"type": "database", "role": "primary"},
        },
    ],
    "policy": {
        "allow": [],
        "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536},
    },
}

call(
    3,
    "proofstorm_lab_create",
    {"draft_id": "cdk-postgres", "lab": lab, "idempotency_key": "create-cdk-postgres"},
)
published = call(
    4,
    "proofstorm_lab_publish",
    {
        "draft_id": "cdk-postgres",
        "expected_version": 1,
        "idempotency_key": "publish-cdk-postgres",
    },
)
database_lock = next(
    entry for entry in published["lock"]["entries"] if entry["catalog_id"] == "postgresql"
)
if database_lock["image"] != postgres["image"] or "@sha256:" not in database_lock["image"]:
    fail(f"PostgreSQL lock is not the catalog-pinned image: {database_lock}")

status = call(
    5,
    "proofstorm_lab_materialize",
    {
        "instance_id": "cdk-postgres-instance",
        "revision_digest": published["digest"],
        "idempotency_key": "materialize-cdk-postgres",
    },
)
for identifier in range(6, 206):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-postgres-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"CDK+PostgreSQL lab did not become ready: {status}")

namespace = status["instance_namespace"]
public_config = kubectl("get", "configmap/mint-config", "-n", namespace, "-o", "jsonpath={.data.config\\.toml}")
if "postgresql://" in public_config or "[database]" in public_config:
    fail("the public mint ConfigMap contains the private database configuration")

private_config = kubectl("exec", "deployment/mint", "-n", namespace, "--", "cat", "/config/config.toml")
for expected in [
    '[database]\nengine = "postgres"',
    "[database.postgres]",
    'tls_mode = "disable"',
    "max_connections = 20",
    "connection_timeout_seconds = 10",
]:
    if expected not in private_config:
        fail(f"materialized mint configuration is missing {expected!r}")
if "@database:5432/proofstorm_mint" not in private_config:
    fail("materialized mint configuration does not target the selected database component")

secret_data = kubectl("get", "secret/database-credentials", "-n", namespace, "-o", "json")
secret_digest = hashlib.sha256(secret_data.encode()).hexdigest()
secret = json.loads(secret_data)
if set(secret.get("data", {})) != {
    "DATABASE_URL",
    "POSTGRES_DB",
    "POSTGRES_PASSWORD",
    "POSTGRES_USER",
    "database.toml",
}:
    fail("generated database Secret has an unexpected key contract")

sql = (
    'PGPASSWORD="$POSTGRES_PASSWORD" psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" '
    '-d "$POSTGRES_DB" -c "CREATE TABLE IF NOT EXISTS proofstorm_acceptance '
    '(id integer primary key, marker text not null);" '
    '-c "INSERT INTO proofstorm_acceptance VALUES (1, \'persistent\') '
    'ON CONFLICT (id) DO UPDATE SET marker = EXCLUDED.marker;"'
)
kubectl("exec", "statefulset/database", "-n", namespace, "--", "sh", "-c", sql)
table_count = int(
    kubectl(
        "exec",
        "statefulset/database",
        "-n",
        namespace,
        "--",
        "sh",
        "-c",
        'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" '
        "-c \"SELECT count(*) FROM pg_tables WHERE schemaname = 'public';\"",
    ).strip()
)
if table_count < 2:
    fail(f"CDK did not initialize its PostgreSQL schema: only {table_count} public tables")

kubectl("rollout", "restart", "deployment/proofstormd", "-n", "proofstorm-system", capture=False)
kubectl(
    "rollout",
    "status",
    "deployment/proofstormd",
    "-n",
    "proofstorm-system",
    "--timeout=120s",
    capture=False,
)
time.sleep(5)
reconciled_secret = kubectl("get", "secret/database-credentials", "-n", namespace, "-o", "json")
if hashlib.sha256(reconciled_secret.encode()).hexdigest() != secret_digest:
    fail("controller reconciliation rotated or mutated the generated database Secret")

kubectl("rollout", "restart", "statefulset/database", "-n", namespace, capture=False)
kubectl("rollout", "status", "statefulset/database", "-n", namespace, "--timeout=180s", capture=False)
kubectl("rollout", "restart", "deployment/mint", "-n", namespace, capture=False)
kubectl("rollout", "status", "deployment/mint", "-n", namespace, "--timeout=180s", capture=False)

persisted = kubectl(
    "exec",
    "statefulset/database",
    "-n",
    namespace,
    "--",
    "sh",
    "-c",
    'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" '
    "-c \"SELECT marker FROM proofstorm_acceptance WHERE id = 1;\"",
).strip()
if persisted != "persistent":
    fail(f"PostgreSQL state did not survive restart: {persisted!r}")

for identifier in range(206, 266):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-postgres-instance"})
    if status["phase"] == "ready":
        break
    time.sleep(3)
else:
    fail(f"CDK+PostgreSQL lab did not recover after restart: {status}")

call(266, "proofstorm_lab_close", {"instance_id": "cdk-postgres-instance"})
for identifier in range(267, 327):
    status = call(identifier, "proofstorm_lab_status", {"instance_id": "cdk-postgres-instance"})
    if status["phase"] == "closed":
        break
    time.sleep(3)
else:
    fail(f"CDK+PostgreSQL lab did not close: {status}")

process.terminate()
process.wait(timeout=10)
print(
    json.dumps(
        {
            "revision": published["digest"],
            "postgres_version": database_lock["version"],
            "postgres_image": database_lock["image"],
            "storage_size": "2Gi",
            "cdk_schema_tables": table_count,
            "restart_persistence": persisted,
            "status": status,
        },
        indent=2,
    )
)
