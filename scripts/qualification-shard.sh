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
    if [[ -f "$work/$id/gate-failure.json" ]]; then
      # Read only the public diagnostic schema, never error text/native output.
      detail=$(jq -er '
        (.reason // .native.reason // "gate-failed") as $reason |
        (if (["channel-request-rejected", "insufficient-funds", "native-command-failed", "image-pull-failed", "image-pull-backoff", "invalid-image-name", "container-config-error", "container-crash-loop", "container-start-error", "container-exited", "pod-unschedulable", "cell-readiness-blocked", "native-observation-timeout", "operation-failed", "operation-container-failed", "operation-deadline-exceeded", "operation-runtime-lost", "tool-rpc-error", "tool-error-result"] | index($reason)) != null
         then $reason else "gate-failed" end) as $category |
        [.locations[]? | strings | select(test("^crates/proofstorm-acceptance/src/([A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+\\.rs:[1-9][0-9]*(:[0-9]+)?$"))] as $locations |
        ([$locations[] | select(contains("/gates/") or contains("/qualification/"))][0] // $locations[0]) as $location |
        ((.operation.termination_reason | strings | select(test("^[A-Za-z]{1,32}$"))) // null) as $termination |
        ((.operation.exit_code | numbers | select(. == floor and . >= 0 and . <= 255)) // null) as $exit |
        ((.tool.tool | strings | select(length <= 64 and test("^[a-z]+(_[a-z]+)+$"))) // null) as $tool |
        ((.tool.code | strings | select(length <= 64 and test("^[a-z]+(_[a-z]+)+$"))) // null) as $code |
        ((.tool.http_status | numbers | select(. == floor and . >= 100 and . <= 599)) // null) as $http |
        $category + (if $location then "; at " + $location else "" end)
          + (if $termination then "; " + $termination else "" end)
          + (if $exit then "; exit " + ($exit | tostring) else "" end)
          + (if $tool then "; tool " + $tool else "" end)
          + (if $code then "; code " + $code else "" end)
          + (if $http then "; http " + ($http | tostring) else "" end)
      ' "$work/$id/gate-failure.json" 2>/dev/null) || detail=''
      if [[ -n "$detail" ]]; then
        reason="$reason; $detail"
      fi
    fi
  fi
  if [[ -f "$work/$id/qualification-receipt.json" ]]; then
    if ! cp "$work/$id/qualification-receipt.json" "$receipts/$id.json"; then
      reason="${reason:+$reason; }receipt copy failed"
    fi
    if [[ -n "$reason" ]]; then
      stage=$(jq -er '.stage | select(type=="string" and test("^[a-z][a-z0-9-]{0,63}$"))' "$work/$id/qualification-receipt.json" 2>/dev/null) || stage=''
      if [[ -n "$stage" ]]; then
        reason="$reason; stage=$stage"
      fi
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
