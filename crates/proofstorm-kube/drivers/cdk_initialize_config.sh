set -eu
cdk-mintd config validate --file /config/config.toml
work_dir=${CDK_MINTD_WORK_DIR:-/app/data}
marker="$work_dir/.proofstorm-config.sha256"
digest=$(sha256sum /config/config.toml | cut -d ' ' -f 1)
if cdk-mintd config show >/dev/null; then
    # A process/pod restart must not undo management RPC mutations. Apply only
    # when the controller's authored document changes (or on first adoption).
    if [ ! -f "$marker" ] || [ "$(cat "$marker")" != "$digest" ]; then
        cdk-mintd config apply --file /config/config.toml
    fi
else
    # --new-mint refuses to overwrite an existing database after a failed read.
    cdk-mintd config init --new-mint --file /config/config.toml
fi
# Record only successful applications, atomically, on the persistent volume.
printf '%s\n' "$digest" > "$marker.tmp"
mv "$marker.tmp" "$marker"
