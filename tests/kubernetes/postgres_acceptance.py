import json
import os
import subprocess


def enabled():
    return os.environ.get("PROOFSTORM_STORAGE", "sqlite") == "postgres"


def augment_lab(lab, database_name):
    if not enabled():
        return
    lab["name"] += "-postgres"
    lab["components"].append(
        {
            "id": "database",
            "kind": "database",
            "implementation": "postgresql",
            "version": "17.11",
            "config_version": "postgresql/17/v1",
            "control": "laboratory",
            "config": {"database_name": database_name, "storage_size": "2Gi"},
        }
    )
    lab["links"].append(
        {
            "id": "mint-database",
            "kind": "database_backend",
            "from": "mint",
            "to": "database",
            "binding": {"type": "database", "role": "primary"},
        }
    )


def kubectl(namespace, *arguments, capture=True):
    return subprocess.run(
        ["kubectl", "--context", "k3d-proofstorm", "-n", namespace, *arguments],
        check=True,
        capture_output=capture,
        text=True,
    ).stdout


def assert_materialized(namespace, private_config, database_name):
    if not enabled():
        if '[database]\nengine = "sqlite"' not in private_config:
            raise RuntimeError("SQLite scenario did not render its database engine")
        return 0
    public_config = kubectl(
        namespace,
        "get",
        "configmap/mint-config",
        "-o",
        "jsonpath={.data.config\\.toml}",
    )
    if "postgresql://" in public_config or "[database]" in public_config:
        raise RuntimeError("public mint ConfigMap contains private PostgreSQL configuration")
    for expected in [
        '[database]\nengine = "postgres"',
        "[database.postgres]",
        'tls_mode = "disable"',
        "max_connections = 20",
        "connection_timeout_seconds = 10",
        f"@database:5432/{database_name}",
    ]:
        if expected not in private_config:
            raise RuntimeError(f"private PostgreSQL configuration is missing {expected!r}")
    secret = json.loads(
        kubectl(namespace, "get", "secret/database-credentials", "-o", "json")
    )
    if set(secret.get("data", {})) != {
        "POSTGRES_DB",
        "POSTGRES_PASSWORD",
        "POSTGRES_USER",
        "database.toml",
    }:
        raise RuntimeError("generated PostgreSQL Secret has an unexpected key contract")
    table_count = int(
        kubectl(
            namespace,
            "exec",
            "statefulset/database",
            "--",
            "sh",
            "-c",
            'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" '
            "-c \"SELECT count(*) FROM pg_tables WHERE schemaname = 'public';\"",
        ).strip()
    )
    if table_count < 13:
        raise RuntimeError(f"CDK initialized only {table_count} PostgreSQL schema tables")
    return table_count


def seed_sentinel(namespace, marker):
    if not enabled():
        return
    script = (
        'PGPASSWORD="$POSTGRES_PASSWORD" psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" '
        '-d "$POSTGRES_DB" -c "CREATE TABLE IF NOT EXISTS proofstorm_acceptance '
        '(id integer primary key, marker text not null);" '
        f'-c "INSERT INTO proofstorm_acceptance VALUES (1, \'{marker}\') '
        'ON CONFLICT (id) DO UPDATE SET marker = EXCLUDED.marker;"'
    )
    kubectl(namespace, "exec", "statefulset/database", "--", "sh", "-c", script)


def restart_database(namespace):
    if not enabled():
        return
    kubectl(namespace, "rollout", "restart", "statefulset/database", capture=False)
    kubectl(
        namespace,
        "rollout",
        "status",
        "statefulset/database",
        "--timeout=180s",
        capture=False,
    )


def verify_sentinel(namespace, marker):
    if not enabled():
        return
    persisted = kubectl(
        namespace,
        "exec",
        "statefulset/database",
        "--",
        "sh",
        "-c",
        'PGPASSWORD="$POSTGRES_PASSWORD" psql -At -U "$POSTGRES_USER" -d "$POSTGRES_DB" '
        '-c "SELECT marker FROM proofstorm_acceptance WHERE id = 1;"',
    ).strip()
    if persisted != marker:
        raise RuntimeError(
            f"PostgreSQL sentinel did not survive restart: expected {marker!r}, got {persisted!r}"
        )
