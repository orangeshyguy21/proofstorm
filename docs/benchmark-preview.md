# O1 benchmark development pilot

This Rust pilot gives one headless OpenCode harness a real regtest task: build a
cell, mint 1,000 sat, melt 100 sat to an independently observed LND recipient,
report, and remove the cell. It uses existing Bitcoin Core, LND, CDK, and Nutshell
components. It is an opt-in acceptance command, not yet an installed `storm`
benchmark product or a model leaderboard.

## Run

Prerequisites: the normal checkout build/runtime dependencies, Docker, a working
OpenCode installation, and a configured account for the explicitly selected
model. The pilot uses that account and can incur model charges. Never put
credentials in command arguments or committed configuration.

```sh
just dev-build
CARGO_TARGET_DIR=.proofstorm-dev/target cargo build --locked -p proofstorm-acceptance
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --checkout-home "$PWD/.proofstorm-dev/state" \
  --root "$PWD" \
  --work-dir "$PWD/dev/benchmark-o1-kimi-01" \
  --timeout 1500 \
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

The adapter sets both the process directory and `PWD`, passes `--dir`, disables
ancestor project configuration, and uses OpenCode's `--pure` mode. A model-free
connection probe must reach the owned capture proxy before the attempt starts.

The run creates its own installation, runtime, and storage. It retains evidence
in a private directory and verifies that pre-existing resources were preserved.
Setup is outside the timed task. Agent discovery, provisioning, payment,
reporting, and agent cleanup are inside it. The task deadline is 1,200 seconds;
the harness step limit is 150. These are not token or spending limits.

Ctrl-C requests owned cleanup. After a crash, retry cleanup with the retained
receipt:

```sh
.proofstorm-dev/target/debug/proofstorm-acceptance \
  --cleanup "$PWD/dev/benchmark-o1-kimi-01"
```

Cleanup recovery does not grant credit for agent cleanup. A run without a final
preservation check remains unaccepted. Never delete the retained receipt to
start over. Acceptance command success describes the runner lifecycle; read
`benchmark-result.json` for the agent's outcome.

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

Scorer `o1-70-15-15/0.3` computes:

- **Quality (70):** 70% of the weighted assertion score. Nine operational
  assertions are required. Correct JSON-only reporting contributes seven points;
  a formatting error can lose those points without erasing task completion.
- **Tool reliability (15):** `15 × scored_successes / (scored_successes + failures)`.
  Every failure counts. Successful identical calls are deduplicated, repeated
  observations and discovery are capped, and request IDs do not evade the cap.
  The exact rule is retained in `benchmark-task.json`. Distinct unnecessary
  native commands can still game this measure; it is a pilot metric.
- **Time (15):** `15 × clamp((1200 − elapsed_seconds) / 900, 0, 1)`.
  The 300-second target and 1,200-second deadline are provisional, not calibrated
  comparison targets.

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
  --benchmark-grade "$PWD/dev/benchmark-o1-kimi-01"
```

Regrading verifies retained evidence hashes and requires the original task/scorer
contract. Hashes detect accidental evidence changes; they are not a signature or
a defense against someone rewriting both evidence and its manifest.

The `benchmark-oracle` acceptance gate runs a Rust reference flow through the
same capture proxy and independent verifier. It checks all ten assertions and
an offsetting-payment counterexample with live observations. It is a grader
control, not a model attempt, and has no model score. Run it with the same
checkout/root options and a fresh work directory; omit benchmark model options.
The 0.1 and 0.2 attempts remain retained and cannot be regraded with the changed
0.3 task contract. Keep the original runner for offline reproduction of older
results; upgrading the scorer does not rewrite them.

## Scope

This is one task, one harness, and a caller-selected model. Report verified
success first and the composite score second. One attempt does not establish a
model ranking or a reliability estimate. Model aliases and inherited provider
configuration limit exact reproducibility. Do not run timed comparisons alongside
other acceptance workloads.

The release preview still needs the honest-negative O5 task, a second harness,
repeated comparable attempts, calibrated targets, and installed CLI integration.
Bark support and the release reliability gates have separate qualification
requirements. The benchmark does not certify them.
