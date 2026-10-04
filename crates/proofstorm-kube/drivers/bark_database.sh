# The native create command alone creates an empty database. A separate seal
# binds that database to the retained native identity; restarts must match it.
mode=$1
host=$2
port=$3
database=$4
export PGCONNECT_TIMEOUT=3
export PGOPTIONS='-c statement_timeout=5000'
data=${5:-/data}
runtime=${6:-/runtime}
ready=$data/.proofstorm-bark-server
started=$data/.proofstorm-bark-server-started
for _attempt in $(seq 1 120); do
    if pg_isready -q -h "$host" -p "$port" -U proofstorm -d postgres; then break; fi
    sleep 1
done
pg_isready -q -h "$host" -p "$port" -U proofstorm -d postgres || exit 1
if [ "$mode" = check ] && [ ! -e "$started" ] && [ ! -e "$ready" ]; then
    exit 0
fi
if [ ! -f "$ready" ] || [ -L "$ready" ]; then
    echo 'Bark initialization is incomplete; restore or explicitly reset owned state' >&2
    exit 1
fi
identity=$(cat "$ready")
case "$identity" in ''|*[!0-9a-f]*) exit 1 ;; esac
[ "${#identity}" = 64 ] || exit 1
if [ "$mode" = seal ] && [ -f "$runtime/bark-created" ]; then
    psql -X -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U proofstorm -d "$database" -q <<SQL
BEGIN;
CREATE SCHEMA IF NOT EXISTS proofstorm;
CREATE TABLE IF NOT EXISTS proofstorm.identity (id integer PRIMARY KEY CHECK (id = 1), fingerprint text NOT NULL);
INSERT INTO proofstorm.identity VALUES (1, '$identity') ON CONFLICT (id) DO NOTHING;
COMMIT;
SQL
fi
actual=$(psql -X -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U proofstorm -d "$database" -tAc 'SELECT fingerprint FROM proofstorm.identity WHERE id = 1')
[ "$actual" = "$identity" ] || { echo 'Bark database does not match retained native identity' >&2; exit 1; }
# Never let native migrations recreate an erased public schema on restart.
psql -X -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U proofstorm -d "$database" -tAc 'SELECT count(*) FROM public.refinery_schema_history; SELECT count(*) FROM public.wallet_changeset' >/dev/null
