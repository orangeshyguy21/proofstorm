#!/usr/bin/env bash

# Parse the whole run before executing it: repository edits during a long agent
# session must not change the shell's continuation after wait.
main() {

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_CONFIG="$ROOT/examples/opencode/proofstorm-only.json"
SCENARIO_CATALOG="$ROOT/scripts/agent-usability-scenarios.json"
OPENCODE_BIN="${OPENCODE_BIN:-opencode}"
MODEL="kimi-for-coding/k3"
TOOLSET=""
SCENARIO="routing-fee-matrix"
VARIANT="0"
RUN_ID=""
MAX_STEPS=100
MAX_SECONDS=2700
MAX_EQUIVALENT_PLANS=4
LIST_SCENARIOS=false
PRINT_PROMPT=false

usage() {
  printf '%s\n' \
    "usage: $0 --run-id ID [--scenario ID] [--variant N|auto] [--model PROVIDER/MODEL] [--toolset TOOLSET] [--max-steps N] [--max-seconds N] [--max-equivalent-plans N]" \
    "       $0 --list-scenarios" \
    "" \
    "Runs one scenario from the versioned Proofstorm agent-usability corpus in" \
    "a fresh OpenCode session, database, and workspace. Concurrent runs are" \
    "rejected. Results are written below" \
    "dev/agent-usability-runs/ID/."
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-id)
      RUN_ID="${2:-}"
      shift 2
      ;;
    --model)
      MODEL="${2:-}"
      shift 2
      ;;
    --scenario)
      SCENARIO="${2:-}"
      shift 2
      ;;
    --variant)
      VARIANT="${2:-}"
      shift 2
      ;;
    --toolset)
      TOOLSET="${2:-}"
      shift 2
      ;;
    --max-steps)
      MAX_STEPS="${2:-}"
      shift 2
      ;;
    --max-seconds)
      MAX_SECONDS="${2:-}"
      shift 2
      ;;
    --max-equivalent-plans)
      MAX_EQUIVALENT_PLANS="${2:-}"
      shift 2
      ;;
    --list-scenarios)
      LIST_SCENARIOS=true
      shift
      ;;
    --print-prompt)
      PRINT_PROMPT=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$LIST_SCENARIOS" == "true" ]]; then
  jq -r '.scenarios[] | [.id, .family, .novelty, (.prompt_variants | length)] | @tsv' \
    "$SCENARIO_CATALOG"
  exit 0
fi

if [[ -z "$RUN_ID" || ! "$RUN_ID" =~ ^[a-zA-Z0-9][a-zA-Z0-9._-]{0,79}$ ]]; then
  printf '%s\n' "--run-id must be 1-80 safe filename characters" >&2
  exit 2
fi
if [[ ! "$MODEL" =~ ^[^/[:space:]]+/[^[:space:]]+$ ]]; then
  printf '%s\n' "--model must use provider/model form" >&2
  exit 2
fi
if [[ ! "$SCENARIO" =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]]; then
  printf '%s\n' "--scenario must be a lowercase kebab-case identifier" >&2
  exit 2
fi
if [[ "$VARIANT" != "auto" && ! "$VARIANT" =~ ^[0-9]+$ ]]; then
  printf '%s\n' "--variant must be a zero-based integer or auto" >&2
  exit 2
fi
if [[ -n "$TOOLSET" && ! "$TOOLSET" =~ ^[a-zA-Z0-9_-]+$ ]]; then
  printf '%s\n' "--toolset contains unsupported characters" >&2
  exit 2
fi
if [[ ! "$MAX_STEPS" =~ ^[1-9][0-9]*$ || "$MAX_STEPS" -gt 500 ]]; then
  printf '%s\n' "--max-steps must be an integer from 1 through 500" >&2
  exit 2
fi
if [[ ! "$MAX_SECONDS" =~ ^[1-9][0-9]*$ || "$MAX_SECONDS" -gt 14400 ]]; then
  printf '%s\n' "--max-seconds must be an integer from 1 through 14400" >&2
  exit 2
fi
if [[ ! "$MAX_EQUIVALENT_PLANS" =~ ^[2-9][0-9]*$ || "$MAX_EQUIVALENT_PLANS" -gt 20 ]]; then
  printf '%s\n' "--max-equivalent-plans must be an integer from 2 through 20" >&2
  exit 2
fi

for command in "$OPENCODE_BIN" jq git shasum sqlite3 python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'required command is unavailable: %s\n' "$command" >&2
    exit 1
  fi
done

SCENARIO_JSON="$(jq -c --arg id "$SCENARIO" '.scenarios[] | select(.id == $id)' "$SCENARIO_CATALOG")"
if [[ -z "$SCENARIO_JSON" ]]; then
  printf 'unknown scenario: %s\navailable scenarios:\n' "$SCENARIO" >&2
  jq -r '.scenarios[].id | "  " + .' "$SCENARIO_CATALOG" >&2
  exit 2
fi
VARIANT_COUNT="$(jq '.prompt_variants | length' <<<"$SCENARIO_JSON")"
if [[ "$VARIANT" == "auto" ]]; then
  RUN_HASH="$(printf '%s' "$RUN_ID" | shasum -a 256 | awk '{print $1}')"
  VARIANT_INDEX="$(( 16#${RUN_HASH:0:8} % VARIANT_COUNT ))"
else
  VARIANT_INDEX="$VARIANT"
fi
if [[ "$VARIANT_INDEX" -ge "$VARIANT_COUNT" ]]; then
  printf 'scenario %s has %s variants; requested zero-based variant %s\n' \
    "$SCENARIO" "$VARIANT_COUNT" "$VARIANT_INDEX" >&2
  exit 2
fi
if [[ -z "$TOOLSET" ]]; then
  TOOLSET="$(jq -r '.toolset' <<<"$SCENARIO_JSON")"
fi
SCENARIO_FAMILY="$(jq -r '.family' <<<"$SCENARIO_JSON")"
SCENARIO_NOVELTY="$(jq -r '.novelty' <<<"$SCENARIO_JSON")"
EXPECTATIONS="$(jq -c '.gates' <<<"$SCENARIO_JSON")"
MANUAL_GATES="$(jq -c '.manual_gates' <<<"$SCENARIO_JSON")"
EXECUTION_SURFACE="$(jq -r '.execution_surface // "mixed"' <<<"$SCENARIO_JSON")"
INPUTS="$(jq -c --argjson index "$VARIANT_INDEX" '.input_sets?[$index] // {}' <<<"$SCENARIO_JSON")"
PROMPT_BASE="$(jq -r --argjson index "$VARIANT_INDEX" '.prompt_variants[$index]' <<<"$SCENARIO_JSON")"
PROMPT="$PROMPT_BASE Use run identifier $RUN_ID for names that must be unique."
if [[ "$EXECUTION_SURFACE" == "typed-contract" ]]; then
  PROMPT="$PROMPT This run checks the typed-operation contract: use advertised typed operations rather than native execution."
fi

if [[ "$PRINT_PROMPT" == "true" ]]; then
  jq -n \
    --arg run_id "$RUN_ID" \
    --arg scenario "$SCENARIO" \
    --arg family "$SCENARIO_FAMILY" \
    --arg novelty "$SCENARIO_NOVELTY" \
    --argjson variant_index "$VARIANT_INDEX" \
    --arg toolset "$TOOLSET" \
    --arg prompt "$PROMPT" \
    --argjson inputs "$INPUTS" \
    --argjson gates "$EXPECTATIONS" \
    --argjson manual_gates "$MANUAL_GATES" \
    '{run_id:$run_id, scenario:$scenario, family:$family, novelty:$novelty,
      variant_index:$variant_index, toolset:$toolset, prompt:$prompt, inputs:$inputs,
      gates:$gates, manual_gates:$manual_gates}'
  exit 0
fi

LOCK_ROOT="${TMPDIR:-/tmp}/proofstorm-agent-usability-benchmark.lock"
if ! mkdir "$LOCK_ROOT" 2>/dev/null; then
  printf 'another headless Proofstorm benchmark is active (lock: %s)\n' "$LOCK_ROOT" >&2
  exit 1
fi
cleanup_benchmark_lock() {
  rmdir "$LOCK_ROOT" 2>/dev/null || true
}
trap cleanup_benchmark_lock EXIT

KUBECTL="$ROOT/.tools/bin/kubectl"
if [[ ! -x "$KUBECTL" ]]; then
  printf 'pinned kubectl is unavailable: %s\n' "$KUBECTL" >&2
  exit 1
fi
if [[ ! -x "$ROOT/target/release/proofstorm-mcp" ]]; then
  printf '%s\n' "release MCP binary is missing; run make build first" >&2
  exit 1
fi

RUN_ROOT="$ROOT/dev/agent-usability-runs/$RUN_ID"
if [[ -e "$RUN_ROOT" ]]; then
  printf 'benchmark run already exists: %s\n' "$RUN_ROOT" >&2
  exit 1
fi
mkdir -p "$RUN_ROOT"
python3 "$ROOT/scripts/agent-usability-cluster.py" --output "$RUN_ROOT/cluster-before.json"

BEFORE_NAMESPACES="$RUN_ROOT/namespaces-before.json"
AFTER_NAMESPACES="$RUN_ROOT/namespaces-after.json"
"$KUBECTL" --context k3d-proofstorm get namespaces \
  -l proofstorm.dev/instance -o json >"$BEFORE_NAMESPACES"
# The idle guard above requires no existing lab or candidate workloads.

DATABASE="$RUN_ROOT/proofstorm.sqlite3"
CONFIG="$RUN_ROOT/opencode.json"
EVENTS="$RUN_ROOT/events.jsonl"
LOG="$RUN_ROOT/opencode.log"
EXPORT="$RUN_ROOT/session.json"
EXPORT_LOG="$RUN_ROOT/export.log"
METRICS="$RUN_ROOT/metrics.json"
SCORECARD="$RUN_ROOT/scorecard.json"
MANIFEST="$RUN_ROOT/manifest.json"
STOP_REASON="$RUN_ROOT/limit-reason.txt"
CANDIDATE_BUILDS="$RUN_ROOT/candidate-builds.json"
REVISIONS="$RUN_ROOT/revisions.json"
WORKSPACE="agent-usability-$RUN_ID"

jq \
  --arg binary "$ROOT/target/release/proofstorm-mcp" \
  --arg database "$DATABASE" \
  --arg workspace "$WORKSPACE" \
  --arg toolset "$TOOLSET" \
  '.mcp.proofstorm.command = [$binary]
   | .mcp.proofstorm.environment.PROOFSTORM_DB = $database
   | .mcp.proofstorm.environment.PROOFSTORM_WORKSPACE = $workspace
   | .mcp.proofstorm.environment.PROOFSTORM_PRINCIPAL = "benchmark-agent"
   | .mcp.proofstorm.environment.PROOFSTORM_TOOLSET = $toolset' \
  "$BASE_CONFIG" >"$CONFIG"

SOURCE_COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
SOURCE_DIRTY="$(git -C "$ROOT" status --porcelain=v1 | wc -l | tr -d '[:space:]')"
BINARY_DIGEST="$(shasum -a 256 "$ROOT/target/release/proofstorm-mcp" | awk '{print $1}')"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
STARTED_EPOCH="$(date +%s)"

jq -n \
  --arg run_id "$RUN_ID" \
  --arg scenario "$SCENARIO" \
  --arg scenario_family "$SCENARIO_FAMILY" \
  --arg scenario_novelty "$SCENARIO_NOVELTY" \
  --argjson variant_index "$VARIANT_INDEX" \
  --argjson expectations "$EXPECTATIONS" \
  --argjson manual_gates "$MANUAL_GATES" \
  --arg model "$MODEL" \
  --arg toolset "$TOOLSET" \
  --arg workspace "$WORKSPACE" \
  --arg prompt "$PROMPT" \
  --argjson inputs "$INPUTS" \
  --arg execution_surface "$EXECUTION_SURFACE" \
  --arg source_commit "$SOURCE_COMMIT" \
  --argjson source_dirty_files "$SOURCE_DIRTY" \
  --arg binary_digest "sha256:$BINARY_DIGEST" \
  --arg started_at "$STARTED_AT" \
  --argjson max_steps "$MAX_STEPS" \
  --argjson max_seconds "$MAX_SECONDS" \
  --argjson max_equivalent_plans "$MAX_EQUIVALENT_PLANS" \
  '{run_id:$run_id, scenario:$scenario, scenario_family:$scenario_family,
    scenario_novelty:$scenario_novelty, variant_index:$variant_index,
    expectations:$expectations, manual_gates:$manual_gates,
    model:$model, toolset:$toolset, workspace:$workspace,
    prompt:$prompt, inputs:$inputs, execution_surface:$execution_surface, source_commit:$source_commit,
    source_dirty_files:$source_dirty_files, binary_digest:$binary_digest,
    started_at:$started_at,
    limits:{max_steps:$max_steps,max_seconds:$max_seconds,
      max_equivalent_plans:$max_equivalent_plans}}' >"$MANIFEST"

git -C "$ROOT" diff --binary HEAD >"$RUN_ROOT/source-diff.patch"
shasum -a 256 "$SCENARIO_CATALOG" "$0" "$ROOT/scripts/evaluate-agent-usability.py" "$ROOT/scripts/agent-usability-cluster.py" >"$RUN_ROOT/harness-digests.txt"
mkdir "$RUN_ROOT/harness"
cp "$SCENARIO_CATALOG" "$0" "$ROOT/scripts/evaluate-agent-usability.py" "$ROOT/scripts/agent-usability-cluster.py" "$RUN_ROOT/harness/"

printf '[proofstorm-benchmark] run=%s scenario=%s variant=%s model=%s toolset=%s\n' \
  "$RUN_ID" "$SCENARIO" "$VARIANT_INDEX" "$MODEL" "$TOOLSET"
set +e
OPENCODE_CONFIG="$CONFIG" "$OPENCODE_BIN" run \
  --model "$MODEL" \
  --format json \
  --print-logs \
  --log-level INFO \
  --title "Proofstorm usability benchmark $RUN_ID" \
  "$PROMPT" \
  > >(tee "$EVENTS") \
  2> >(tee "$LOG" >&2) &
OPENCODE_PID=$!
(
  while kill -0 "$OPENCODE_PID" 2>/dev/null; do
    ELAPSED="$(( $(date +%s) - STARTED_EPOCH ))"
    STEPS="$(grep -c '\"type\":\"step_finish\"' "$EVENTS" 2>/dev/null || true)"
    EQUIVALENT_PLANS="$(
      jq -rs '
        [.[]
         | select(.type == "tool_use" and (.part.tool | endswith("lab_plan")))
         | .part.state.input
         | del(.plan_id, .idempotency_key)]
        | group_by(.)
        | map(length)
        | max // 0
      ' "$EVENTS" 2>/dev/null || printf '0\n'
    )"
    if [[ "$ELAPSED" -ge "$MAX_SECONDS" ]]; then
      printf 'max_seconds:%s\n' "$MAX_SECONDS" >"$STOP_REASON"
      kill -TERM "$OPENCODE_PID" 2>/dev/null || true
      sleep 10
      kill -KILL "$OPENCODE_PID" 2>/dev/null || true
      exit 0
    fi
    if [[ "$STEPS" -ge "$MAX_STEPS" ]]; then
      printf 'max_steps:%s\n' "$MAX_STEPS" >"$STOP_REASON"
      kill -TERM "$OPENCODE_PID" 2>/dev/null || true
      sleep 10
      kill -KILL "$OPENCODE_PID" 2>/dev/null || true
      exit 0
    fi
    if [[ "$EQUIVALENT_PLANS" -ge "$MAX_EQUIVALENT_PLANS" ]]; then
      printf 'repeated_equivalent_lab_plan:%s\n' "$EQUIVALENT_PLANS" >"$STOP_REASON"
      kill -TERM "$OPENCODE_PID" 2>/dev/null || true
      sleep 10
      kill -KILL "$OPENCODE_PID" 2>/dev/null || true
      exit 0
    fi
    sleep 2
  done
) &
WATCHDOG_PID=$!
wait "$OPENCODE_PID"
OPENCODE_STATUS=$?
kill "$WATCHDOG_PID" 2>/dev/null || true
wait "$WATCHDOG_PID" 2>/dev/null || true
set -e

SESSION_ID="$(jq -rs '[.[] | .sessionID? // empty][0] // ""' "$EVENTS")"
if [[ -n "$SESSION_ID" ]]; then
  set +e
  "$OPENCODE_BIN" export "$SESSION_ID" >"$EXPORT" 2>"$EXPORT_LOG"
  EXPORT_STATUS=$?
  set -e
else
  EXPORT_STATUS=1
  : >"$EXPORT"
  printf '%s\n' "no session ID found in OpenCode events" >"$EXPORT_LOG"
fi

set +e
"$KUBECTL" --context k3d-proofstorm get namespaces \
  -l proofstorm.dev/instance -o json >"$AFTER_NAMESPACES"
KUBECTL_STATUS=$?
set -e
FINISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
FINISHED_EPOCH="$(date +%s)"
WALL_TIME_SECONDS="$((FINISHED_EPOCH - STARTED_EPOCH))"
LIMIT_REASON=""
if [[ -f "$STOP_REASON" ]]; then
  LIMIT_REASON="$(tr -d '\r\n' <"$STOP_REASON")"
fi

sqlite3 "$DATABASE" \
  "SELECT coalesce(json_group_array(json(build_json)), '[]') FROM candidate_builds" \
  >"$CANDIDATE_BUILDS"
sqlite3 "$DATABASE" \
  "SELECT coalesce(json_group_array(json(revision_json)), '[]') FROM revisions" \
  >"$REVISIONS"

jq -s \
  --arg run_id "$RUN_ID" \
  --arg scenario "$SCENARIO" \
  --argjson variant_index "$VARIANT_INDEX" \
  --arg model "$MODEL" \
  --arg toolset "$TOOLSET" \
  --arg session_id "$SESSION_ID" \
  --argjson opencode_exit "$OPENCODE_STATUS" \
  --argjson export_exit "$EXPORT_STATUS" \
  --argjson kubectl_exit "$KUBECTL_STATUS" \
  --argjson inputs "$INPUTS" \
  --arg limit_reason "$LIMIT_REASON" \
  --slurpfile candidate_builds "$CANDIDATE_BUILDS" \
  --slurpfile revisions "$REVISIONS" \
  --slurpfile namespaces_before "$BEFORE_NAMESPACES" \
  --slurpfile namespaces "$AFTER_NAMESPACES" \
  'def tool_events: [.[] | select(type == "object" and .type == "tool_use")];
   def step_events: [.[] | select(type == "object" and .type == "step_finish")];
   def parsed_outputs:
     [tool_events[]
      | (.part.state.output? // empty)
      | fromjson?
      | select(type == "object")];
   def terminal_outputs:
     [parsed_outputs[]
      | ., ((.operations? // [])[])];
   def durable_candidate_builds: ($candidate_builds[0] // []);
   def published_revisions: ($revisions[0] // []);
   def quote_observations:
     [parsed_outputs[]
      | ((.artifact?.content?.quote_observations? // [])[]),
        (((.operations? // [])[])
          | ((.artifact?.content?.quote_observations? // [])[])),
        ((.last_observations? // [])[])]
     | unique_by([.quote_id, .role, .state]);
   def tool_count($name):
     [tool_events[] | select(.part.tool == $name)] | length;
   def completed_tool_count($name):
     [tool_events[]
      | select(.part.tool == $name and .part.state.status == "completed")]
     | length;
   def normalized_error_input:
     del(.idempotency_key, .operation_id);
   def recovery_episodes:
     [tool_events as $calls
      | $calls | to_entries[]
      | select(.value.part.state.status == "error")
      | .key as $index
      | (.value.part.state.error // "") as $message
      | {
          tool_index: $index,
          tool: .value.part.tool,
          input: .value.part.state.input,
          message: $message,
          visible_application_code:
            ($message | test("\\[[a-z][a-z0-9_]+\\]")),
          rejected_target_visible:
            ($message | test("component|field|operation|instance|endpoint|revision"; "i")),
          mutation_disposition_visible:
            ($message | test("no (operation|plan|mutation) was (created|stored)|no plan was stored|no changes were made|already exists with different immutable identity"; "i")),
          bounded_recovery_visible:
            ($message | test("recovery:|next:|valid (component|endpoint|implementation)|choose (a|one)|workflow is infeasible"; "i")),
          next_two_calls:
            ([$calls[$index + 1:$index + 3][]
              | {tool: .part.tool, status: .part.state.status}])
        }];
   def repeated_equivalent_errors:
     [tool_events[]
      | select(.part.state.status == "error")
      | {tool: .part.tool, input: (.part.state.input | normalized_error_input)}]
     | group_by([.tool, .input])
     | map(select(length > 1) | {tool: .[0].tool, input: .[0].input, count: length});
   {
     run_id: $run_id,
     scenario: $scenario,
     variant_index: $variant_index,
     model: $model,
     toolset: $toolset,
     session_id: $session_id,
     exits: {opencode:$opencode_exit, export:$export_exit, kubectl:$kubectl_exit},
     limit_reason: (if $limit_reason == "" then null else $limit_reason end),
     tool_calls: (tool_events | length),
     tool_errors: (tool_events | map(select(.part.state.status == "error")) | length),
     recovery: {
       episodes: recovery_episodes,
       diagnostic_fidelity_failures:
         (recovery_episodes
          | map(select(
              (.visible_application_code and
               .rejected_target_visible and
               .mutation_disposition_visible and
               .bounded_recovery_visible) | not
            ))
          | length),
       repeated_equivalent_errors: repeated_equivalent_errors
     },
     tools: (tool_events | group_by(.part.tool) | map({
       tool: .[0].part.tool,
       calls: length,
       errors: (map(select(.part.state.status == "error")) | length)
     }) | sort_by(.tool)),
     steps: (step_events | length),
     tokens: {
       processed_total: (step_events | map(.part.tokens.total // 0) | add // 0),
       peak_context: (step_events | map(.part.tokens.total // 0) | max // 0),
       input: (step_events | map(.part.tokens.input // 0) | add // 0),
       output: (step_events | map(.part.tokens.output // 0) | add // 0),
       reasoning: (step_events | map(.part.tokens.reasoning // 0) | add // 0),
       cache_read: (step_events | map(.part.tokens.cache.read // 0) | add // 0),
       cache_write: (step_events | map(.part.tokens.cache.write // 0) | add // 0)
     },
     workflow: {
       lab_materializations: (
         completed_tool_count("proofstorm_proofstorm_lab_materialize")
         + completed_tool_count("proofstorm_proofstorm_lab_apply")
       ),
       whole_document_edits: tool_count("proofstorm_proofstorm_lab_edit"),
       raw_exec_calls: (
         tool_count("proofstorm_proofstorm_component_exec_live")
         + tool_count("proofstorm_proofstorm_component_forensics")
       ),
       live_exec_calls: tool_count("proofstorm_proofstorm_component_exec_live"),
       forensics_calls: tool_count("proofstorm_proofstorm_component_forensics"),
       evidence_exports: completed_tool_count("proofstorm_proofstorm_artifact_export"),
       operation_failures:
         (terminal_outputs | map(select(.terminal? == true and .phase? == "failed")) | length),
       native_exec_nonzero_exits:
         (terminal_outputs
          | map(select(
              (.kind? == "component_forensics" or .kind? == "component_exec_live")
              and (.artifact?.content?.exit_code? // 0) != 0
            ))
          | length),
       wait_timeouts:
         (parsed_outputs | map(select(.timed_out? == true)) | length),
       verified_teardowns:
         (parsed_outputs
          | map(select(.teardown_receipt?.verified_absent? == true))
          | length),
       paid_melts:
         (quote_observations
          | map(select(.role? == "payment_melt" and .state? == "PAID"))
          | length),
       unpaid_melts:
         (quote_observations
          | map(select(.role? == "payment_melt" and .state? == "UNPAID"))
          | length),
       successful_wallet_funds:
         (terminal_outputs
          | map(select(.kind? == "wallet_fund" and .phase? == "succeeded"))
          | length),
       equivalent_plan_repeats_max:
         ([tool_events[]
           | select(.part.tool | endswith("lab_plan"))
           | .part.state.input
           | del(.plan_id, .idempotency_key)]
          | group_by(.)
          | map(length)
          | max // 0)
     },
     candidate: {
       builds: (durable_candidate_builds | length),
       successes:
         (durable_candidate_builds | map(select(.phase == "succeeded")) | length),
       wait_calls: tool_count("proofstorm_proofstorm_candidate_wait"),
       wait_timeouts:
         ([tool_events[]
           | select(.part.tool == "proofstorm_proofstorm_candidate_wait")
           | (.part.state.output? // empty)
           | fromjson?
           | select(.timed_out? == true)]
          | length),
       valid_immutable_builds:
         (durable_candidate_builds
          | map(select(
              .phase == "succeeded"
              and (.commit_sha? | test("^[0-9a-f]{40}$"))
              and (.image? | test("@sha256:[0-9a-f]{64}$"))
              and (.version? | startswith("candidate-"))))
          | length),
       exact_version_published:
         ([durable_candidate_builds[] as $build
           | published_revisions[].lab.components[]
           | select(
               .implementation == $build.implementation
               and .version == $build.version)]
          | length),
       provenance_locked:
         ([durable_candidate_builds[] as $build
           | published_revisions[].lock.entries[]
           | select(
               .catalog_id == $build.implementation
               and .version == $build.version
               and .image == $build.image
               and .source.candidate_id == $build.id
               and .source.pull_request_url == $build.pull_request_url
               and .source.repository == $build.repository
               and .source.commit_sha == $build.commit_sha)]
          | length),
       requested_lightning_version_published:
         ([published_revisions[].lab.components[]
           | select(
               .implementation == "lnd"
               and .version == ($inputs.lightning_version // ""))]
          | length),
       requested_wallet_fund_calls:
         ([tool_events[]
           | select(
               .part.tool == "proofstorm_proofstorm_wallet_fund"
               and .part.state.input.amount_sat? == ($inputs.wallet_amount_sat // null))]
          | length)
     },
     remaining_instance_namespaces:
       (if $kubectl_exit == 0 then
          (($namespaces[0].items | map(.metadata.name))
           - ($namespaces_before[0].items | map(.metadata.name)))
        else null end)
   }' "$EVENTS" >"$METRICS"

jq -n \
  --slurpfile metrics "$METRICS" \
  --argjson wall_time_seconds "$WALL_TIME_SECONDS" \
  --argjson expectations "$EXPECTATIONS" \
  --argjson manual_gates "$MANUAL_GATES" \
  '{
    run_id: $metrics[0].run_id,
    scenario: $metrics[0].scenario,
    variant_index: $metrics[0].variant_index,
    wall_time_seconds: $wall_time_seconds,
    automated_hard_gates: {
      headless_session_exited_cleanly: ($metrics[0].exits.opencode == 0),
      evidence_requirement_met: (
        ($expectations.evidence_required | not)
        or $metrics[0].workflow.evidence_exports > 0
      ),
      teardown_requirement_met: (
        if $expectations.teardown_required then
          $metrics[0].workflow.verified_teardowns > 0
          and ($metrics[0].remaining_instance_namespaces | length) == 0
        else
          ($metrics[0].remaining_instance_namespaces | length) == 0
        end
      ),
      materialization_count_met: (
        $metrics[0].workflow.lab_materializations >= $expectations.materializations_min
        and $metrics[0].workflow.lab_materializations <= $expectations.materializations_max
      ),
      candidate_build_count_met:
        ($metrics[0].candidate.builds >= ($expectations.candidate_builds_min // 0)),
      candidate_success_count_met:
        ($metrics[0].candidate.successes >= ($expectations.candidate_successes_min // 0)),
      candidate_wait_count_met:
        ($metrics[0].candidate.wait_calls >= ($expectations.candidate_wait_calls_min // 0)),
      candidate_immutable_identity_met: (
        ($expectations.candidate_successes_min // 0) == 0
        or $metrics[0].candidate.valid_immutable_builds >= $expectations.candidate_successes_min
      ),
      candidate_exact_version_met: (
        ($expectations.candidate_exact_version_required // false | not)
        or $metrics[0].candidate.exact_version_published > 0
      ),
      candidate_provenance_met: (
        ($expectations.candidate_provenance_required // false | not)
        or $metrics[0].candidate.provenance_locked > 0
      ),
      requested_lightning_version_met: (
        ($expectations.requested_lightning_version_required // false | not)
        or $metrics[0].candidate.requested_lightning_version_published > 0
      ),
      requested_wallet_amount_met: (
        ($expectations.requested_wallet_amount_required // false | not)
        or $metrics[0].candidate.requested_wallet_fund_calls > 0
      ),
      wallet_fund_count_met:
        ($metrics[0].workflow.successful_wallet_funds >= ($expectations.wallet_funds_min // 0)),
      paid_melt_count_met:
        ($metrics[0].workflow.paid_melts >= $expectations.paid_melts_min),
      unpaid_melt_count_met:
        ($metrics[0].workflow.unpaid_melts >= $expectations.unpaid_melts_min)
    },
    thrash: {
      tool_errors: $metrics[0].tool_errors,
      recoverable_error_budget: 2,
      diagnostic_fidelity_failures:
        $metrics[0].recovery.diagnostic_fidelity_failures,
      repeated_equivalent_errors:
        ($metrics[0].recovery.repeated_equivalent_errors | length),
      operation_failures: $metrics[0].workflow.operation_failures,
      native_exec_nonzero_exits: $metrics[0].workflow.native_exec_nonzero_exits,
      wait_timeouts: $metrics[0].workflow.wait_timeouts,
      raw_exec_calls: $metrics[0].workflow.raw_exec_calls,
      extra_materializations: (
        [$metrics[0].workflow.lab_materializations - $expectations.materializations_max, 0]
        | max
      ),
      equivalent_plan_repeats_max:
        $metrics[0].workflow.equivalent_plan_repeats_max,
      whole_document_edits: $metrics[0].workflow.whole_document_edits
    },
    recovery: $metrics[0].recovery,
    candidate: $metrics[0].candidate,
    efficiency: {
      tool_calls: $metrics[0].tool_calls,
      model_steps: $metrics[0].steps,
      peak_context_tokens: $metrics[0].tokens.peak_context,
      processed_tokens: $metrics[0].tokens.processed_total
    },
    manual_hard_gates:
      (reduce $manual_gates[] as $gate ({}; .[$gate] = null)),
    proficiency: "needs_review"
  }' >"$SCORECARD"

TEMP_MANIFEST="$RUN_ROOT/manifest.updated.json"
jq \
  --arg finished_at "$FINISHED_AT" \
  --arg session_id "$SESSION_ID" \
  --argjson wall_time_seconds "$WALL_TIME_SECONDS" \
  --argjson opencode_exit "$OPENCODE_STATUS" \
  --argjson export_exit "$EXPORT_STATUS" \
  '. + {finished_at:$finished_at, wall_time_seconds:$wall_time_seconds,
         session_id:$session_id,
         exits:{opencode:$opencode_exit, export:$export_exit}}' \
  "$MANIFEST" >"$TEMP_MANIFEST"
mv "$TEMP_MANIFEST" "$MANIFEST"

set +e
python3 "$ROOT/scripts/agent-usability-cluster.py" --retire-builds "$RUN_ROOT" \
  --wait-seconds 30 --output "$RUN_ROOT/cluster-after.json"
CLUSTER_STATUS=$?
set -e
python3 "$RUN_ROOT/harness/evaluate-agent-usability.py" "$RUN_ROOT"

printf '[proofstorm-benchmark] artifacts=%s\n' "$RUN_ROOT"
jq . "$METRICS"
jq . "$SCORECARD"

if [[ "$OPENCODE_STATUS" -ne 0 ]]; then
  exit "$OPENCODE_STATUS"
fi

if [[ "$CLUSTER_STATUS" -ne 0 ]]; then
  exit "$CLUSTER_STATUS"
fi

}
main "$@"
