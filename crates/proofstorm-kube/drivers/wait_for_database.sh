# The Service exposes only ready PostgreSQL endpoints. Wait before running any
# configuration/database mutation; do not retry a failed initializer itself.
for attempt in $(seq 1 120); do
    if nc -z -w 2 "$1" "$2"; then
        exit 0
    fi
    sleep 1
done
echo 'PostgreSQL service did not become ready before component initialization' >&2
exit 1
