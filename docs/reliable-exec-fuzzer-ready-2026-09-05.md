# Reliable native execution: fuzzer ready, dispatch paused

Prepared scenario: `reliable-native-execution`. No agent session or laboratory
has been launched. Dispatch is paused as explicitly requested in the build handoff.

The scenario seeds the deterministic checkpoint's small topology: Bitcoin Core
30.0, LND 0.20.0-beta, and CDK CLI wallet 0.18.0, with the LND chain binding.
The fixture uses explicit native-exec runtime requirements and validates through
the release MCP binary. It is a planning preview only; the deterministic gate's
earlier materialization remains the live evidence for this topology.

The agent receives lifecycle, evidence, cancellation and native-exec capabilities.
It cannot author plans, fund wallets, mine, open channels or invoke wallet
mutation tools. No controller restart or pod replacement is part of this run.

Independent review gates cover:

- Command versus shell exit scope and preserved known nonzero exits.
- Default private output, selected safe JSON fields, and no canary disclosure.
- Malformed projection failing closed while retaining exit evidence.
- Cancellation after a verified start marker, plus a separate native deadline;
  both require terminal supervisor cleanup receipts.
- A new-exec refusal after the cleanup boundary, without model-step padding.
- Terminal owned operations, evidence export, verified teardown and a final
  report within the hard cap.

Operation success phase alone is insufficient. Expected command failures and
admission refusals are scored against their intended contracts, separately from
agent mistakes and supervisor failures. The scenario explicitly limits claims
about actual descendant absence, private canary inspection and restart coverage.
Public output is limited to safe CLI help/version. Synthetic canary values are
generated inside a component and must not appear in requests or exports.

Proposed single-run command, **not executed**:

```sh
OPENCODE_BIN=/Users/admin/.opencode/bin/opencode \
  bash scripts/run-agent-usability-benchmark.sh \
  --scenario reliable-native-execution \
  --run-id reliable-exec-fuzzer-01-20260905 \
  --max-seconds 600 --max-steps 50 --max-equivalent-plans 2
```

This retains Kimi K3 as the configured runner default. Cleanup latches at
480 seconds or 40 completed steps, leaving 120 seconds and 10 steps. The agent
may use at most two harmless supervised timers to observe the real time boundary;
it must not waste model steps on redundant discovery. No subsequent money-flow
run is authorized by this preparation checkpoint.

Preparation verification:

- Seed fixture accepted by the real MCP planner without materialization.
- Three host cleanup-proxy tests passed, covering time/step boundaries,
  reconnect latching and refusal before forwarding.
- Shell syntax, prompt preview and whitespace checks passed.
- The release MCP digest matches the handoff:
  `sha256:c834de155676c6a1da0972557251fe3ce792863a492bd305a993c56a5cbc31ac`.
- The current controller matches the handoff image digest:
  `sha256:f42a7d9ab93ae4920f8cbf5b3d9933ecec03b19ee406b55de09ba2cff9dfb5c0`.
- Independent cluster audit reports verified idle with no blockers.

The prior deterministic checkpoint's 212 Rust, 17 host Python and 5 Linux test
results were reviewed as supplied evidence, not rerun here. The newly repeated
test set consists only of the three proxy tests above.

Prepared fixture: `scripts/fixtures/reliable-native-execution-plan.json`.
Scenario: `scripts/agent-usability-scenarios.json`.
Preview, local plan receipt, binary digest and cluster audit:
`dev/agent-usability-runs/reliable-exec-fuzzer-prep-20260905/`.
