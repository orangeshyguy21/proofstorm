#!/usr/bin/env bash
# Continue independent cases after a failure, retain only redacted receipts.
set -uo pipefail
[[ $# == 4 ]] || exit 2
plan=$1 cases=$2 work=$3 receipts=$4
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || exit 1
mkdir -p "$work" "$receipts" || exit 1
ids=$(jq -er 'select(type=="array" and length>0 and all(.[]; type=="string")) | .[]' "$cases") || exit 1
failures=()
completed=0
while IFS= read -r id; do
  "$root/target/check/debug/proofstorm-qualification" case "$plan" "$id" >/dev/null || exit 1
  started=$SECONDS
  status=0
  reason=''
  "$root/target/check/debug/proofstorm-acceptance" --root "$root" \
    --checkout-home "$root/.proofstorm-dev/state" --work-dir "$work/$id" \
    --qualification-plan "$plan" --qualification-case "$id" --timeout 1200 qualification || status=$?
  if (( status != 0 )); then
    reason="acceptance exited $status"
  fi
  if [[ -f "$work/$id/qualification-receipt.json" ]]; then
    if ! cp "$work/$id/qualification-receipt.json" "$receipts/$id.json"; then
      reason="${reason:+$reason; }receipt copy failed"
    fi
  else
    reason="${reason:+$reason; }receipt missing"
  fi
  if [[ -n "$reason" ]]; then
    failures+=("$id: $reason")
    printf '::error::Qualification %s: %s\n' "$id" "$reason"
  fi
  completed=$((completed+1))
  printf '%s completed in %s seconds\n' "$id" "$((SECONDS-started))"
done <<< "$ids"
if (( ${#failures[@]} > 0 )); then
  printf '\nQualification shard failed: %s of %s cases failed.\n' "${#failures[@]}" "$completed"
  printf ' - %s\n' "${failures[@]}"
  exit 1
fi
printf '\nQualification shard passed: %s cases.\n' "$completed"
