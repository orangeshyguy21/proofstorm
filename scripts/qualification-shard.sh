#!/usr/bin/env bash
# Continue independent cases after a failure, retain only redacted receipts.
set -uo pipefail
[[ $# == 4 ]] || exit 2
plan=$1 cases=$2 work=$3 receipts=$4
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || exit 1
mkdir -p "$work" "$receipts" || exit 1
ids=$(jq -er 'select(type=="array" and length>0 and all(.[]; type=="string")) | .[]' "$cases") || exit 1
failed=0
while IFS= read -r id; do
  "$root/target/check/debug/proofstorm-qualification" case "$plan" "$id" >/dev/null || exit 1
  started=$SECONDS
  "$root/target/check/debug/proofstorm-acceptance" --root "$root" \
    --checkout-home "$root/.proofstorm-dev/state" --work-dir "$work/$id" \
    --qualification-plan "$plan" --qualification-case "$id" --timeout 1200 qualification || failed=1
  if [[ -f "$work/$id/qualification-receipt.json" ]]; then
    cp "$work/$id/qualification-receipt.json" "$receipts/$id.json" || exit 1
  else
    failed=1
  fi
  printf '%s completed in %s seconds\n' "$id" "$((SECONDS-started))"
done <<< "$ids"
exit "$failed"
