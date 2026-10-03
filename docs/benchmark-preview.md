# Operate benchmark development pilot

This Rust pilot runs selected models through headless OpenCode, Codex or Claude Code CLIs on two real regtest tasks: O1
(build, mint, pay, report and remove) and O5 (investigate an unroutable payment,
report honestly and remove). O1 mints 1,000 sat and melts 100 sat to an independently
observed LND recipient. It uses existing Bitcoin Core, LND, CDK, and Nutshell
components. It is an opt-in acceptance command, not yet an installed `storm`
benchmark product or a model leaderboard.

For fixed-order multi-model runs with retained receipts and explicit continuation,
see [local benchmark campaigns](benchmark-campaign.md).

O1 0.9 and O5 0.5 keep schema discovery and composition in scope. The prompt supplies
component IDs, roles, implementations, versions and semantic link requirements,
not a complete cell document. The agent discovers configuration versions, control
settings and bindings from the catalog. Component grading checks IDs,
implementations and versions; bindings are checked semantically. The reference
control alone uses the complete document retained in `Task`. This replaces 0.4's
ready-to-submit document; no model attempts used the 0.4 contract.

These versions separate workflow outcomes from autonomy and checkpoint ordering.
O5 explicitly requires an empty recipient invoice list at the funded checkpoint;
creating the invoice early fails `checkpoint_order`, while independently correct
unpaid-state and accounting checks can still pass. Report `success` describes the
observed workflow and cleanup, so a procedural failure alone does not make the
financial report false. Strict benchmark completion still requires every
operational check, and incomplete contracts still score zero. The 70/15/15
weights and 240-second full-credit target are unchanged. Every model now gets a
3,600-second execution allowance. Speed credit reaches zero at 1,200 seconds; a
correct later completion can still earn up to 85/100. Local and hosted models use
the same ranked contracts. Older task versions retain their original grading
and transcript interpretation.

Codex's `list_mcp_resources` and `list_mcp_resource_templates`, scoped to
`proofstorm` or without a server, are neutral discovery. Their completed outcomes
are retained in `harness-outcome.json` under `usage.neutral_discovery`, without
tool-score credit or penalties. Incomplete discovery still means incomplete
telemetry. Resource reads and other servers remain forbidden; forbidden MCP
calls violate autonomy whether they succeed or fail. A completed forbidden call
is not incorrectly represented as pending.

New results expose `workflow_success`, `failed_requirements`, autonomy and
checkpoint-order fields, plus checkpoint diagnostics and timing intervals.
`timing_breakdown` reports the union of completed MCP call intervals, separately
including checkpoint and cleanup time. Categories may overlap: do not sum them
or interpret remaining wall time as pure model reasoning. No time is subtracted
from the end-to-end score.

## Run

Prerequisites: the normal checkout build/runtime dependencies, Docker, a working
installation of the selected harness, and a configured account for the explicitly selected
model. The pilot uses that account and can incur model charges. Never put
credentials in command arguments or committed configuration.

```sh
just dev-build
CARGO_TARGET_DIR=.proofstorm-dev/target cargo build --locked -p proofstorm-acceptance
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --checkout-home "$PWD/.proofstorm-dev/state" \
  --root "$PWD" \
  --work-dir "$PWD/dev/benchmark-o1-kimi-v06-01" \
  --timeout 4200 \
  --benchmark-model kimi-code-plan-global/kimi-for-coding \
  --benchmark-opencode /absolute/path/to/opencode \
  benchmark-o1
```

Choose a **new** work directory for every attempt. Use the exact provider/model
ID reported by your OpenCode installation. The runner rejects an unavailable
model instead of substituting one. It audits the effective configuration for
extra plugins, MCP servers, and tool permissions. The agent gets only the
restricted Proofstorm MCP tool set; direct host shell, filesystem, delegation,
and web tools are denied. Component-native commands remain available through
`cell_exec`.

For current contracts, OpenCode's `invalid` rejection wrappers are classified
using the original tool name and the task's allowlist. A rejected permitted call
counts as one tool failure, even if the wrapper itself completed successfully;
it does not violate autonomy. Foreign or unidentified tool attempts still do.
The retained wrapper keeps the original tool name and error as evidence.

For Codex, select the harness and exact Codex model ID instead:

```sh
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --checkout-home "$PWD/.proofstorm-dev/state" \
  --root "$PWD" \
  --work-dir "$PWD/dev/benchmark-o1-codex-01" \
  --timeout 4200 \
  --benchmark-harness codex \
  --benchmark-codex /absolute/path/to/codex \
  --benchmark-model gpt-6-astra \
  benchmark-o1
```

The Codex adapter requires `exec --json --strict-config --ephemeral --ignore-rules`
plus `debug models --bundled` and `debug prompt-input`. Its CLI contract was exercised with
`0.158.0-alpha.2.1` against a local fake model and MCP server. Subsequent real
O1/O5 attempts with `gpt-6-astra` exercised settlement, failed-payment evidence,
scoring and cleanup; this does not establish access to other models. Unknown model IDs fail
before a model request. Bundled catalog membership does not prove account access;
provider failures remain failed attempts, and no replacement model is selected.

Each Codex attempt owns `agent/` and a private `codex-home/`. Authentication comes
from an explicit `--benchmark-codex-auth /absolute/path/to/auth.json`, otherwise
`CODEX_API_KEY`, otherwise the original `$CODEX_HOME/auth.json` (default
`~/.codex/auth.json`). Only authentication is copied, never user config, MCP
servers, plugins, trust entries, or keyring state. Keyring-only login requires
file-based authentication or `CODEX_API_KEY`. The source login is never written;
the copy is removed on return and by parent cleanup after cancellation. If the
entire runner is forcibly killed, use the retained run's `--cleanup` recovery.

The controlled Codex profile preserves model identity and base instructions but
disables host shell, file patching, delegation, plugins, personal skill discovery and experimental
extra tools. It retains the CLI's code-mode MCP dispatch and uses a read-only
sandbox. Both the original catalog entry and controlled entry are retained in
`codex-model.private.json`; the exact configuration is in
`codex-config.private.toml`. CLI-bundled skill descriptions may still appear;
`codex-prompt.private.json` retains the diagnostic rendering of model instructions
and the task prompt. These are benchmark settings, not stock Codex defaults.
Codex may also expose MCP resource discovery and a user-input tool that cannot
accept assistance in `exec`. Extra MCP servers or changed proxy commands fail
preflight. Preflight checks configuration and connects through Codex's diagnostic
prompt renderer using a separate proxy capture; it is not a model turn and cannot
consume the scored attempt's proxy identity.

Codex MCP item events are joined to the independent proxy trace, counting each
call once. Missing or conflicting events cannot earn complete telemetry credit.
Its JSON stream does not expose every wrapper-only code-mode error, so the tool
ratio covers observed MCP attempts, not every script evaluation. Token usage is
retained; unknown cost stays null. Requested model ID is recorded separately from
provider-resolved identity, which this CLI does not verify.

For Claude Code, select the harness and an exact model ID. By default the attempt
uses this machine's normal Claude Code login (`--benchmark-claude-auth login`);
run `claude auth status` to confirm you are logged in. Only the login method and
plan are recorded, never the account email or organization.

```sh
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --checkout-home "$PWD/.proofstorm-dev/state" \
  --root "$PWD" \
  --work-dir "$PWD/dev/benchmark-o1-claude-01" \
  --timeout 4200 \
  --benchmark-harness claude-code \
  --benchmark-claude /absolute/path/to/claude \
  --benchmark-model claude-opus-5-5 \
  benchmark-o1
```

Login mode reads the normal config directory for authentication, so Claude Code
may add an entry for the attempt's project to `~/.claude.json` and write MCP logs
to its cache. Personal settings, hooks, plugins, skills, other MCP servers and
built-in tools still stay out, and sessions are not saved. For full isolation, use
`--benchmark-claude-auth environment` with exactly one of `ANTHROPIC_API_KEY` or
`CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) in the runner's environment.
That mode gives the attempt an owned `HOME` and `CLAUDE_CONFIG_DIR` under the work
directory, reads no keychain login, and never writes the credential to disk.

Each attempt runs `claude -p --output-format stream-json` with a cleared
environment (host `ANTHROPIC_*`/`CLAUDE_*` settings never reach it), an empty owned git repository
as its project, no setting sources, no session persistence and auto-memory
disabled. The only tools are the task's Proofstorm MCP tools: built-in tools are
removed with `--tools ""`, `--strict-mcp-config` loads only the owned proxy, and
`--permission-mode dontAsk` refuses anything not on the allowlist. The proxy gets
the runner's `HOME` back and blank model credentials. Claude Code's own system
prompt, default effort and thinking settings apply; they are recorded, not
replaced. Host managed (policy) settings, if installed, still apply.

Preflight runs the exact attempt configuration with a placeholder key against an
owned loopback endpoint that refuses every request. It checks the session's tool
list, MCP connection, permission mode, model and credential source, and the
would-be request's model, tool definitions and prompt delivery, before any model
call. The wire profile (version, effort, thinking, system-prompt and tool-definition
digests) is retained in `claude-preflight.private.json` and the manifest. The CLI
contract was exercised with Claude Code `2.1.281` through one real MCP round trip
against a local fake model, then real O1/O5 attempts with `claude-opus-5-5` using
the machine login. Those attempts exposed an unrecognized tool-progress heartbeat
event; the adapter now validates heartbeats against known calls without treating
them as attempts or replies. Copies of the retained transcripts were regraded
offline with that fix; the original receipts remain unchanged.

Stream-json tool uses are joined to the proxy trace, counting each call once. A
permission refusal is a failed call; an unobserved foreign tool success, subagent
traffic or a wider session tool list is unauthorized. Unknown events, missing
session initialization and unmatched proxy calls leave telemetry incomplete.
Claude Code's reported cost is retained as an estimate; subscription tokens are
not billed per call.

The adapter sets both the process directory and `PWD`, passes `--dir`, disables
ancestor project configuration, and uses OpenCode's `--pure` mode. A model-free
connection probe must reach the owned capture proxy before the attempt starts.

The run creates its own installation, runtime, and storage. It retains evidence
in a private directory and verifies protected resources and owned cleanup.
Benchmark preservation protects Proofstorm resources and checkout state. Unrelated
Docker rebuilds, restarts and removals, plus personal CLI configuration changes,
are retained as `shared_host_activity` and do not disqualify a run. Owned cleanup
is still mandatory. See the shared-host policy in [campaigns](benchmark-campaign.md).
Setup is outside the timed task. Agent discovery, provisioning, payment,
reporting, and agent cleanup are inside it. The task deadline is 3,600 seconds;
the harness step limit is 150. These are not token or spending limits.

Ctrl-C requests owned cleanup. After a crash, retry cleanup with the retained
receipt:

```sh
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --cleanup "$PWD/dev/benchmark-o1-kimi-v06-01"
```

Cleanup recovery does not grant credit for agent cleanup. A run without a final
preservation check remains unaccepted. Never delete the retained receipt to
start over. Acceptance command success describes the runner lifecycle; read
`benchmark-result.json` for the agent's outcome.

## O5: honest negative

Select `benchmark-o5` instead of `benchmark-o1` with the same model/harness
options and a fresh work directory. O5 0.5 funds a 1,000-sat wallet normally, then
attempts a 100-sat melt to a third, isolated LND node. Its recipient must never
have a channel. The agent investigates the refusal, preserves all wallet funds,
reports the result and removes the cell. Give the backend spendable outbound
liquidity above channel reserves before the melt; the funding payment alone
may not provide that. Insufficient balance does not meet the required no-route
condition. Do not repair the route to the isolated recipient.

The `funded` checkpoint precedes the attempt; `evaluated` follows its terminal
outcome. Independent observations must show one fresh failed backend payment
matching the invoice hash and amount, `FAILURE_REASON_NO_ROUTE`, an unpaid mint
and wallet quote, an open unpaid recipient invoice, no consumed proofs or input
fees, and the full 1,000-sat unreserved balance. An unpaid invoice without an
attempt, a pending payment, a timeout reason, or missing evidence cannot pass.
Recipient channels, pending/closed channels and invoice history are checked too.

Recovery may retry the same recipient invoice. The checkpoint selects the
explicit melt quote ID, verifies its wallet invoice, and matches it to the quote
returned by the expected mint and the backend payment record. It never selects
the newest quote. Unknown IDs, mismatched invoices/mints and duplicate records
for the selected ID fail. Nutshell may omit its local quote mint field; the
expected mint API must still return that exact quote. Driver observation errors
include their sanitized failure reason. O5 0.1 evidence and scores remain
unchanged; they must not be regraded as 0.2.

O5 adds `payment_occurred` (boolean) and `diagnosis` (string) to the common report
shape. Successful investigation reports `success:true`, `payment_occurred:false`,
`paid_sat:0`, and `diagnosis:"no_route"`, with the observed balance and cleanup.
Task success is distinct from payment success. A successful MCP receipt describing
a failed native payment is a successful tool call; actual MCP/harness errors still
count as failures, without blanket exemptions for O5.

Scorer `o5-70-15-15/0.5` keeps 70/15/15 weights with a calibrated 240-second
full-credit target, zero speed credit at 1,200 seconds, and a common 3,600-second
execution budget. Its quality weights are 10 each for components, bindings, issuance,
recorded attempt, accounting, no-route diagnosis and reporting;
5 each for unpaid recipient, checkpoint order, terminal operations, evidence, autonomy and agent cleanup. All twelve
operational assertions are required; reporting retains its separate validity and
format rules. A timeout fails completion without redefining autonomy as failure.

Run the model-free `benchmark-o5-oracle` gate to validate the negative grader.
It rejects retained counterexamples for no attempt, pending or different payments,
wrong failure reason, settlement, missing/conflicting quotes, lost/reserved funds,
consumed proofs/fees, connected recipient and replaced cell identity. Both oracles
must use separate work directories; acceptance rejects combining benchmark gates.

## Evidence and scoring


The benchmark proxy records tool starts, replies, errors, and elapsed time.
Harness events add rejected tool attempts that never reached MCP; wrappers are
counted once. An interrupted call or missing telemetry is unknown, not successful.
Tool success means the tool request succeeded, not that a payment settled.

Two immutable checkpoints read mint quote state, wallet state, and recipient
invoice state through an independent client. The grader checks payment identity,
the specified topology, 1,000 sat issued, 100 sat received, remaining balance
890–900 sat with no reserved balance, terminal operation cleanup, correct structured claims,
and verified cell removal. The final observer checks namespace absence before
the outer runner removes its runtime. Runner cleanup cannot replace agent
cleanup. Exact fee decomposition is not claimed by this task.

The grader also checks the fresh payer's successful payment and settled invoice
lists: exactly one 1,000-sat funding payment and one 100-sat receipt are allowed.
Extra mint/melt cycles cannot hide behind the same final balance. The proxy
retains terminal operation evidence immediately before the first removal request,
because cell teardown deletes those records. Either a verified `cell_wait` or a
completed `cell_remove` receipt can demonstrate closure.

For O1, scorer `o1-70-15-15/0.9` computes:

- **Quality (70):** 70% of the weighted assertion score. Nine operational
  assertions are required. Correct JSON-only reporting contributes seven points;
  a formatting error can lose those points without erasing task completion.
- **Tool reliability (15):** `15 × scored_successes / (scored_successes + failures)`.
  Every failure counts. Successful identical calls are deduplicated, repeated
  observations and discovery are capped, and request IDs do not evade the cap.
  The exact rule is retained in `benchmark-task.json`. Distinct unnecessary
  native commands can still game this measure; it is a pilot metric.
- **Time (15):** `15 × clamp((1200 − elapsed_seconds) / 960, 0, 1)`.
  The 240-second full-credit target comes from the prepared reference cohort
  below. This cutoff is independent of the 3,600-second execution deadline.
  A valid 40-minute completion receives zero time points and retains its quality
  and tool points; incomplete work or exceeding 60 minutes still fails completion.

The report schema describes shape only: required fields, types and no extra
fields. It does not prescribe success, amounts or the grading balance window.
Claims are compared with independent observations after agent cleanup has been
checked. An honest `success:false` report can be valid without earning task
completion; a false success or cleanup claim fails validation. Missing payment
evidence cannot be replaced by a claimed zero.

Results separate `task_success`, `report_valid`, `report_format`, and
`environment_valid`. A complete trailing JSON object after prose can validate
claims but earns no reporting points. The parser does not repair JSON, choose
among multiple objects, accept duplicate keys, or grade prose semantics. Missing,
ambiguous, or incorrect structured claims fail report validation.

With a valid environment, a required operational assertion failure, invalid
report, timeout, or harness/grader failure makes the accepted score zero.
Unverified runner cleanup or preservation produces `invalid_environment`, with
null accepted score and accepted success. Exclude those attempts from model
comparison denominators and report their count separately; retain every attempt.
`task_score` shows the result before environment validation and is diagnostic only.
Verified completion with incomplete scoring telemetry is unscored (`null`).
Diagnostic assertion points remain visible in every case. Tokens and provider
costs are separate metrics; unknown cost is not zero.

Primary outputs are `benchmark-result.json` and `benchmark-report.md`. Private
supporting evidence includes the prompt, task version, model alias, harness
version/configuration, runner digest, source revision/dirty flag, artifact
identities, MCP transcript, harness transcript, independent observations, and
acceptance cleanup/preservation records. These may contain credentials and
payment material: share only a reviewed, redacted derivative.

Regrade without a model or runtime:

```sh
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --benchmark-grade "$PWD/dev/benchmark-o1-kimi-v06-01"
```

Regrading verifies retained evidence hashes and requires the original task/scorer
contract. One Rust `Task` defines the prompt, cell scope, component/config versions,
links, amounts and fee bound, allowed tools, timing, assertion weights and report
schema, explicit roles, expected payment outcome and checkpoint name. The registry
looks up the retained ID/version before comparing the complete serialized task;
its hash and full equality check guard regrading.
The remaining-balance window is derived from its amounts and maximum fee.
Hashes detect accidental evidence changes; they are not a signature or
a defense against someone rewriting both evidence and its manifest.

The `benchmark-oracle` acceptance gate runs a Rust reference flow through the
same capture proxy and independent verifier. It checks all ten assertions and
an offsetting-payment counterexample with live observations. It is a grader
control, not a model attempt, and has no model score. Run it with the same
checkout/root options and a fresh work directory; omit benchmark model options.
The O1 0.1 and 0.2 attempts remain retained and cannot be regraded with the
current task contracts. The O1 0.3, 0.4 and 0.5 runners are retained separately;
no model attempts used those contracts. Keep the original runner for offline reproduction of older
results; upgrading the scorer does not rewrite them.
O1 0.6–0.8, O5 0.2–0.4 and their `-diagnostic.1` contracts remain registered for
exact offline regrading with their original hashes and timing. New ranked runs
use O1 0.9/O5 0.5 for every model. Historical scores must not be relabeled or
pooled with the current versions.

Optional `benchmark-o1-diagnostic` and `benchmark-o5-diagnostic` gates retain
separate unranked contracts for explicit diagnostics. They are not required for
local models: ordinary O1/O5 now also allow 3,600 seconds and award ranked scores.
For either profile, pass `--timeout 4200` (or greater) to leave room for verification.
Increasing the outer gate timeout alone never increases a model deadline.

Diagnostic prompts, task hashes, manifests and retained results identify the
extended allowance. Results retain completion, assertions, tool counts, actual
wall time and environment validity, but `ranking_eligible` is false and
`task_score`, `accepted_score` and `time_points` are null, including failed
attempts. Cleanup and preservation are still mandatory. These gates are invoked
directly with the acceptance CLI; the ranked campaign driver does not accept
them. Record local model/runtime identity and hardware alongside the receipts.
The 60-minute allowance is a common execution budget, not a calibrated latency percentile.

The shared MCP schema layer expands bounded, acyclic local `$ref` definitions
when the expanded tool schema is no larger than the original. This exposes
nested objects, arrays, enums and tagged variants directly to tool parsers while
preserving validation constraints and use-site annotations. Unused root
definitions are removed only after complete expansion. Schemas with unresolved
or differently scoped references, assertion siblings at a reference, excessive
expansion, or growth from repeated definitions retain their compact reference
graph. Retained references still expose direct types where safely inferable.

Union types stay on their individual branches: hoisting a type beside `anyOf`
can trigger Kimi's schema rejection. The rule is shared across models; Rust
request validation remains strict, with no conversion of strings into objects,
arrays or numbers. Provider parsers can still mishandle nested arguments, so
qualify nullable objects, nested unions, arrays, numeric fields and tool-error
recovery before starting a model campaign.

Known-tool failures use MCP results with `isError: true`. The original error
code, message and details appear in both text and `structuredContent`, so
clients that omit JSON-RPC error data still receive recovery instructions.
Unknown tools and malformed protocol requests retain protocol error handling.
Authorization still runs before handlers. Error results obey the 32 KiB
response bound; oversized details are explicitly marked omitted, with a
bounded message and the original classification where it fits. Tool errors
continue to count as failed calls. This follows MCP's distinction between
[tool execution and protocol errors](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx#error-handling).
Refresh source and binary pins after changes to the shared tool surface before
freezing a campaign; historical attempts retain their original evidence.

For model-free timing baselines, use `benchmark-o1-calibration` or
`benchmark-o5-calibration`, with a fresh work directory and no model options.
These run the same scripted task and independent assertions. O5 calibration
attempts the negative payment once; the oracle's extra two-quote retry remains
in `benchmark-o5-oracle` as a separate regression control.

All benchmark gates first prepare the selected task's pinned component images
and probe image in the run's private registry and nodes. This setup step creates
no cell and launches no model. `benchmark-image-preparation.json` retains the
`selected-task-images-v2` profile, task hash, installation identity, exact image
set, elapsed setup time and outcome. Failed or interrupted preparation stops the
run as a setup failure; it cannot consume a model attempt. The worker requires
a matching successful receipt before starting its task. Ordinary cell creation
still verifies images, creates the cell and waits for readiness inside the task
timer. Historical runs without this preparation profile must remain separate
from the prepared timing cohort.
The last image-transfer command's bounded output is retained privately in
`benchmark-image-preparation.private.json`; public errors omit that output.
Pinned registry copies request HTTP/1.1 through Go's documented
[`http2client` setting](https://pkg.go.dev/net/http#hdr-HTTP_2), scoped to the
Docker image-copy subprocess. This avoids the observed peer HTTP/2 stream
failures. HTTPS certificate checks and image digest verification remain enabled;
the Docker daemon, global configuration and other subprocesses are unchanged.

`oracle-reference.json` identifies `calibration-reference-v1` and records
monotonic task time from the first `cell_up` through report construction and
observed cell cleanup. It excludes runner setup, proxy initialization, post-run
verification, synthetic counterexamples and runner teardown. Source revision,
dirty state, binary digests, task hash and basic platform metadata are retained
in `reference-provenance.json`. A timing sample is usable only when the gate,
owned cleanup and preservation all pass in `acceptance.json`.

Scripted references measure an infrastructure baseline; they do not include
model reasoning or schema discovery and receive no model score. Use repeated
references on a recorded resource profile to propose targets before comparative
runs. The gates themselves never change task versions or previously retained
results.

The `prepared-reference-timing-v1` contract freezes O1 0.7 and O5 0.3 at
240/1200 seconds with unchanged 70/15/15 weights. The recorded policy selects
the slowest of three accepted reference durations per task, rounded upward to
a whole 60 seconds. Measurements from the 2026-09-30 cohort are:

| Task | Accepted durations (seconds) | Mean | Sample standard deviation | Target |
| --- | --- | ---: | ---: | ---: |
| O1 | 183.386, 182.500, 179.070 | 181.652 | 2.280 | 240 |
| O5 | 194.233, 186.592, 190.947 | 190.590 | 3.833 | 240 |

All six share the same source patch, executable hashes, task contracts and
Docker resource profile. Task assertions, cleanup, preservation and all O5
negative controls passed; independent inventory checks found no leftover
resources. One settings-drift sample and two tool-download setup failures are
retained separately and excluded. Earlier source/preparation cohorts are not
pooled. The machine was an Apple M4 Max with 16 logical CPUs and 128 GiB RAM;
Docker had 16 CPUs and 33,599,827,968 bytes of memory, without CPU reservation.
The task rules retain the combined evidence summary's SHA-256 digest.

Three references on a shared host establish only an infrastructure baseline;
they do not estimate tail latency or model reasoning/discovery time. The
original 20-minute deadline was a chosen execution budget. The current contracts
retain it only as the zero-credit cutoff and allow every model 60 minutes. No
contestant ordering was used to fit the target. The original calibration freeze changed timing/version metadata only;
prompts, task actions, assertions and scoring gates are unchanged. Model
comparisons still require a frozen roster, environment and campaign budget.

Preservation hashes only Claude's top-level and per-project MCP server maps,
normalized as JSON. Other agent files remain byte-exact. Two pre-run Docker
snapshots, normally five seconds apart, identify already-changing external
lifecycle fields. A container in restart backoff or observed mid-restart extends
observation up to a 90-second deadline until both start time and restart count
change; timeout fails before runtime
or model startup. Only observed changes receive exclusions, which are listed in `acceptance.json`. Stable fields, ownership, identities, mounts and
networks remain strict. No unrelated container is stopped to obtain a pass.

## Scope

This is a two-task development pilot with three CLI adapters and caller-selected models. Report verified
success first and the composite score second. One attempt does not establish a
model ranking or a reliability estimate. Model aliases and inherited provider
configuration limit exact reproducibility. Do not run timed comparisons alongside
other acceptance workloads.

Compare models first and retain harness/version/settings as metadata; running the
same model through multiple harnesses is optional. Harness differences can affect
results and must remain visible. The initial Codex/Claude live qualification
retained all four attempts: both models completed O1, while O5 exposed report
validation and failure-reason mistakes. Cleanup and preservation passed for all
four; these are qualification samples, not a comparison campaign. The release
preview still needs repeated comparable attempts, calibrated targets, and
installed CLI integration.
Bark support and the release reliability gates have separate qualification
requirements. The benchmark does not certify them.
