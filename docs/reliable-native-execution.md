# Reliable native execution checkpoint

The CDK fuzzer's [preplanned smoke report](cdk-wallet-preplanned-smoke-2026-09-05.md)
established that native funding, payment, restart and wallet isolation work. It
also exposed three execution problems: background commands lacked durable exit
and cleanup evidence, a format-specific redactor leaked a payment preimage, and
the prompt's cleanup boundary did not prevent further experiments.

This build adds a shared execution contract for existing components. Wallet
images need no wallet-specific supervisor or extra Python/process-inspection
packages. Cocod and the private ecash exchange remain separate checkpoints.

## Agent contract

`proofstorm_component_exec_live` still requires the experiment, lease, component,
operation ID and idempotency key. Supply exactly one command form:

```json
{
  "instance_id": "lab",
  "experiment_id": "experiment",
  "lease_id": "lease",
  "operation_id": "wallet-help",
  "idempotency_key": "wallet-help",
  "component": "wallet",
  "argv": ["cdk-cli", "--help"],
  "timeout_seconds": 10,
  "output": {"mode": "public"}
}
```

Use `argv` to retain the invoked process's exit status. `script` executes through
`/bin/sh -c`; its receipt says `exit_scope: "shell"`. For example, a shell pipeline
can return zero even when its first command failed. Process success also does
not establish payment settlement or issuance: keep independent wallet/mint and
recipient observations.

Submission returns an operation handle. Use `proofstorm_operation_wait` to read
its eventual receipt, or `proofstorm_action_cancel` followed by a wait to request
and verify cancellation. Do not start a separate `nohup` job. The deadline is
1–300 seconds and continues when the MCP client disconnects or the controller
restarts. A timeout on the *wait tool* only bounds that observation call.

Receipts distinguish `exit_code` from `exit_signal`, `timed_out`, `cancelled`,
`cleanup_verified`, stream completeness and truncation. An operation can be
`succeeded` because execution and observation completed even when its command
exited nonzero or reached its deadline. Always inspect the receipt. Cancelled
and failed operations preserve supervisor receipts when available.

## Output handling

| Mode | Ordinary artifact |
| --- | --- |
| `private` (default) | Exit/cleanup status and byte/hash metadata; empty stdout/stderr |
| `public` | Explicit opt-in to bounded stdout/stderr; no automatic secret filtering |
| `json_fields` | Selected, typed receipt values; empty stdout/stderr |
| `bolt11` | Validated invoice-only text and derived hash/amount/currency/expiry; empty stdout/stderr |
| `lnd_invoice` | Validated LND invoice JSON with matching payment hash; empty stdout/stderr |

The two invoice modes deliberately expose a small Lightning invoice, not Cashu
notes. They require complete capture and a successful producer, preserve native
exit evidence separately from extraction, and accept no `fields` argument.
See [structured invoice relay](structured-invoice-relay.md) for limits, failure
semantics and the separate native payment/settlement steps.

For example, `{"mode":"json_fields","fields":["status","value_sat"]}`
accepts those fields from the last document of a completely valid JSON stream.
Allowed fields are fixed in `crates/proofstorm-core/src/native.rs`: enumerated
status/failure values, selected booleans, and numeric amount/balance/fee values.
Objects, arbitrary strings, missing fields, malformed/table output or
truncated capture produce a static projection error. There is no raw-output
fallback. Preimages, proofs and tokens are not selectable fields.

Cocod lifecycle observations use direct `argv: ["cocod", "status"]` with
`{"mode":"json_fields","fields":["seedAccess.state","seedAccess.requiresPassphrase","cocoSession.state"]}`.
These exact leaf paths have fixed enum/boolean validation. Selected keys retain
their dotted names. A native `seedAccess: null` produces null for its selected
children, indicating an uninitialized wallet; missing fields and unexpected
values fail closed. Entire objects and `lastFailure` messages cannot be selected.
For `cocod health`, select `status`; its allowed healthy value is `ok`.
This keeps native command exits intact without an agent-written status parser.

Prefer direct `argv` with private output when recipient settlement and passive
wallet observations can establish the payment outcome. A native text parser is
not required just to reproduce that evidence. For native JSON or an actually
needed parsed receipt, use `json_fields`, including when a shell wrapper first
translates text into JSON. Parse an enumerated status and numeric amounts, then
let the projection validate those values. Do not publish lines selected by words
such as `paid` or `fee`, or remove lines containing `preimage`: a changed native
format can put sensitive material on another matching line. Keep raw output
component-local and fail closed on an unrecognized format. Capture the native
exit code before parsing and preserve it; emit no text prefix into the JSON
stream. An unknown format is a parsing error, not a fabricated payment failure
or zero amount. Independent recipient settlement and passive wallet observations
still establish the money outcome.

The supervisor drains both streams and retains at most 16 KiB per stream in its
private directory, with mode-0600 files. Hashes describe retained bytes; observed
byte counts may be larger. Each public stream is additionally limited to 12 KiB
of encoded JSON so escaping cannot overflow the journal's 32 KiB artifact limit
and prevent recording an exit/cleanup receipt. Truncation is explicit.
A capture error is explicit and never a public raw
fallback. Private files remain in the component's `/tmp` until that container
is replaced or its lab is removed. The operation ID is recorded as the private
output reference; this release does not add an agent payload-download tool.

Command requests are still journaled. Read sensitive inputs from existing
component-local files instead of embedding them in `script` or `argv`. This
protects ordinary transcripts and evidence from accidental command output; it
is not an isolation boundary against arbitrary native code with the same UID.
It also does not change the separate forensics or component-log contracts.

The upcoming ecash exchange must provide recipient authorization, streaming and
retention controls. A private execution-output directory does not implement
peer-to-peer token delivery or prove recipient redemption.

## Runtime and failure semantics

The controller streams a statically linked Linux helper into a mode-0700 private
directory in the selected container. Bootstrap needs `/bin/sh`, `mktemp`, `cat`,
`chmod`, executable writable `/tmp`, and Linux `/proc`. The local deployment is
arm64; the helper must match the component architecture. Its digest and pinned
pod UID are included in durable action state and execution evidence.

The controller claims and persists the handle with a conditional resource-version
write before the single start attempt. Competing controllers and cancellation
invalidate stale claims. The helper
also uses exclusive creation of its request file as a replay fence. An uncertain
start is polled through the existing handle, never automatically retried. A
crash between recording the handle and launching the command may therefore
leave an unknown outcome even when the command never ran.

The supervisor is a Linux child subreaper. It handles deadline/cancellation with
TERM then KILL, adopts orphaned descendants including session escapes, and
reaps them. Signals target its own direct children, avoiding process-name
matching and PID-reuse races. A terminal `cleanup_verified: true` requires that
no children remain. Normal command completion also cleans up lingering children.

Controller restart preserves the running supervisor and its eventual receipt.
Pod replacement, a killed supervisor, or missing terminal evidence yields an
explicit unknown outcome with no replay. Uninterruptible children can produce
`cleanup_verified: false`; inspect/close that lab before considering another
mutation. Cancellation cannot reverse a completed payment or wallet mutation.

Command failure is also not a rollback. CDK CLI 0.18 can complete a fee-bearing
preparation swap before a melt is rejected. Check balances and settlement/quote
state before a new native attempt. Reusing an existing operation ID and
idempotency key retrieves that execution; choosing a new ID starts new work
that may incur another fee.

## Enforced cleanup reserve

The benchmark now adds `_benchmark_budget` to tool response documents, including
the current phase and absolute cleanup/hard deadlines. It checks the boundary
again when a response returns, so a wait that spans the transition announces
cleanup immediately. Observation waits are shortened to the boundary during
work and to at most ten seconds during cleanup. Native execution deadlines are
unchanged. Call `lab_close`, then `lab_wait` with `target_phase=closed`; allow time
for both the teardown receipt and the final report.

Optional runner flags `--max-context-tokens` and `--max-processed-tokens` add
ceilings over observed completed-step usage (zero disables each). Cleanup also
latches at 80% of either enabled ceiling. The watchdog stops a run at the full
ceiling, and a final check records an overrun even if the model exits before the
watchdog sees it. Cached input counts toward these totals. They are observation
budgets, not provider billing limits: an in-flight model step can overshoot before
its usage is reported. The budget metadata includes observed usage so the agent
can steer toward evidence, teardown and reporting early.

The agent-usability runner wraps its MCP server with
`scripts/native-execution-proxy.py`. At 80% of either wall time or observed
completed model steps, it latches into cleanup mode on disk. Reconnecting does
not reopen admission. New experiments, native commands and other work are
refused before reaching MCP; status, cancellation, evidence export, lease
release and experiment/lab close remain available. Existing hard caps remain.

This is enforcement at the benchmark's MCP boundary. It cannot force a model
to produce a final report, cancel already-running commands by itself, or revoke
an unrelated server connection. The run's `cleanup-phase.json` records why the
boundary activated. The final 20% is reserved for completing cleanup and reporting.

## Validation and partner handoff

Status on 2026-09-05: **deterministic checkpoint passed; paused before fuzzer
dispatch**. Run `reliable-exec-07-20260905` passed on the final controller image,
including normal lab teardown and an independent `verified_idle: true` audit.
The local evidence directory contains `validation.json`, operation receipts,
the private-output-safe evidence export, `closed.json`, the cluster audit and
build/test logs. No new fuzzer run was launched by this build.

| Check | Result |
| --- | --- |
| Shared runner compatibility | LND/musl and CDK/glibc both passed |
| Native status | Direct exit 7 preserved; shell scope explicit |
| Deadline and cancellation | Real started processes stopped; cleanup verified |
| Controller restart and idempotency | Running command completed; exactly one marker byte |
| Private and selected output | Synthetic preimage absent from receipts and export; malformed output failed closed |
| Escaped public output | 40,000 bytes on each stream; bounded terminal journal receipt retained |
| Automated checks | 212 Rust tests, 17 host Python tests and 5 isolated Linux tests passed |
| Toolchain and release | Strict workspace Clippy, formatting and release MCP doctor passed |

Controller image:
`proofstorm-registry.localhost:5000/proofstormd@sha256:f42a7d9ab93ae4920f8cbf5b3d9933ecec03b19ee406b55de09ba2cff9dfb5c0`.
Runner digest:
`sha256:a503353d7e028fc3794d72a1229767e0586ef4e384030c99a637994536da75b9`.
The source remains an uncommitted workspace build. This checkpoint does not
claim a funded money-flow rerun, a multi-controller fault campaign, or an agent
run that successfully finishes its report; those are distinct observations.

The deterministic gate is `make e2e-reliable-exec` on an idle local cluster with
the pinned CDK wallet image available. It uses a small Bitcoin/LND/CDK lab with
no funding. Evidence is retained in `dev/native-execution-runs/<run-id>/`.

The isolated Linux contract tests are in `tests/native_supervisor_contract.py`:
session-escaping descendants, cancellation isolation, direct/shell exit status,
normal-exit background cleanup, large stream draining and fail-closed output.
The MCP contract tests also exercise escaped output against the journal budget
and preservation of native receipts across cancellation/failure. The host tests
in `tests/test_native_execution_proxy.py` verify both cleanup
boundaries, reconnect latching and that refused calls never reach an MCP server.

Pause before launching the partner fuzzer. Its next scenario should exercise
execution reliability only, with a verified small lab plan and no funding:

1. Discover direct invocation and private/selected output. Use a synthetic canary
   stored inside a component; keep it out of requests and exported evidence.
2. Start a bounded long operation, observe its handle, cancel it, and retain the
   native cleanup receipt. Let a separate operation reach its own deadline.
3. Observe a nonzero command exit and a malformed JSON projection without losing
   the exit status or disclosing output. Use public output only for safe help.
4. Reach the cleanup reserve and confirm a new execution is refused. Release
   the lease, close the experiment, export evidence, close the lab, verify
   teardown, and return a final report within the
   existing hard cap. Log discovery friction separately from execution failures.

Controller restart and actual descendant absence are operator assertions in the
deterministic gate; the agent must stay within its authorized MCP surfaces.
After this bounded fuzzer checkpoint passes, rerun the CDK money-flow scenario
with the new execution/output contract. Then resume the cocod vertical slice
and the scoped ecash exchange before mixed-wallet transfers.
