# Create this component's own database on the linked PostgreSQL server, once.
# Arguments: host, port, database. The database name was validated as a
# PostgreSQL identifier before publication, so it is safe to quote directly.
# The Service exposes only ready PostgreSQL endpoints; wait for it, then
# create idempotently. Never drop or alter an existing database.
host=$1
port=$2
database=$3
export PGPASSWORD="$PROOFSTORM_POSTGRES_PASSWORD"
for attempt in $(seq 1 120); do
    if pg_isready -q -h "$host" -p "$port" -U proofstorm -d postgres; then
        exists=$(psql -h "$host" -p "$port" -U proofstorm -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$database'") || exists=
        if [ "$exists" = 1 ] || psql -q -h "$host" -p "$port" -U proofstorm -d postgres -c "CREATE DATABASE \"$database\""; then
            exit 0
        fi
    fi
    sleep 1
done
echo "PostgreSQL database $database was not available before component initialization" >&2
exit 1
