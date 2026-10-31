# Create this operator's main and ephemeral databases on the linked PostgreSQL
# server, then bind the main database to the operator's identity key, once.
# Arguments: host, port, base database name. The base name was validated as a
# PostgreSQL identifier with room for the suffixes before publication.
# PROOFSTORM_POD_NAME is the StatefulSet pod name <component>-<ordinal>. A database holding
# state without the seal, or sealed to another identity, is never adopted.
host=$1
port=$2
base=$3
ordinal=${PROOFSTORM_POD_NAME##*-}
case "$ordinal" in ''|*[!0-9]*) echo 'Spark operator ordinal is not a number' >&2; exit 1 ;; esac
identity=$(cat "/spark-operators/operator-$ordinal.pub")
case "$identity" in ''|*[!0-9a-f]*) echo 'Spark operator identity is missing or malformed' >&2; exit 1 ;; esac
[ "${#identity}" = 66 ] || { echo 'Spark operator identity is missing or malformed' >&2; exit 1; }
main=${base}_${ordinal}
export PGCONNECT_TIMEOUT=3
export PGOPTIONS='-c statement_timeout=5000'
for _attempt in $(seq 1 120); do
    if pg_isready -q -h "$host" -p "$port" -U proofstorm -d postgres; then break; fi
    sleep 1
done
pg_isready -q -h "$host" -p "$port" -U proofstorm -d postgres || { echo "PostgreSQL was not available before operator initialization" >&2; exit 1; }
for database in "$main" "${main}_ephemeral"; do
    exists=$(psql -X -h "$host" -p "$port" -U proofstorm -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$database'")
    [ "$exists" = 1 ] || psql -X -q -h "$host" -p "$port" -U proofstorm -d postgres -c "CREATE DATABASE \"$database\""
done
# The seal is the database's comment, not a schema: atlas refuses a first
# migration into a database holding any schema besides an empty public one.
expected="proofstorm-spark-operator/v1 $identity"
seal() {
    psql -X -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U proofstorm -d postgres -tAc "SELECT coalesce(shobj_description(oid, 'pg_database'), '') FROM pg_database WHERE datname = '$main'"
}
if [ -z "$(seal)" ]; then
    tables=$(psql -X -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U proofstorm -d "$main" -tAc "SELECT count(*) FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema')")
    [ "$tables" = 0 ] || { echo 'Spark operator database has state but no identity seal; restore or explicitly reset it' >&2; exit 1; }
    psql -X -v ON_ERROR_STOP=1 -q -h "$host" -p "$port" -U proofstorm -d postgres -c "COMMENT ON DATABASE \"$main\" IS '$expected'"
fi
[ "$(seal)" = "$expected" ] || { echo 'Spark operator database belongs to another identity' >&2; exit 1; }
