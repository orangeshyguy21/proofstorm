# Service endpoints are published after the native node's readiness check.
# That also establishes the credentials/socket shared with the CDK mint.
for attempt in $(seq 1 120); do
    if nc -z -w 2 "$1" "$2"; then
        exit 0
    fi
    sleep 1
done
echo 'Lightning backend did not become ready before mint startup' >&2
exit 1
