# CDK wallet hardening

Status: **ready to begin the next wallet's deterministic build checkpoint**.
The CDK wallet/runtime baseline passed; the funded agent benchmark has not passed
all gates together. That distinction is deliberate: the remaining agent parsing
and reporting friction is recorded below, not relabeled as a successful fuzzer
run. Work stops here; no next wallet has been started.

Funding, independent recipient settlement, restart persistence, isolation,
unpaid-quote cancellation/resumption, nonzero fees, definite rejection and
idempotent replay have passing deterministic evidence. Native CLIs remain the
primary surface. No wallet-specific mutation tools were added. The final cluster
audit is idle, with no remaining lab resources.

## Reliability checkpoint

`reliable-exec-fuzzer-02-20260905` passed independent review under the unchanged
600-second/50-step cap. All 12 accepted native operations had terminal receipts
with verified cleanup, complete streams and no truncation. Private/selected
output, direct/shell exit status, malformed projection, started cancellation and
execution deadline cases held. Cleanup was announced at 480 seconds. The agent
cancelled its timer, exported evidence, obtained verified teardown and finished
its report before the hard cap. The independent cluster audit was idle.

The harness fix exposes absolute budget/phase metadata in allowed tool responses
and shortens observation waits at cleanup; it does not alter command deadlines.
Five cleanup-proxy tests and 14 evaluator tests passed. The run prompt contained
an erroneous `lab_close_wait` hint, but the agent used the advertised close/wait
tools correctly; that hint was corrected before subsequent runs.

This run used 34 steps, 40 tool calls and 913,845 processed tokens, including
867,328 cache-read tokens; peak context was 39,952. Coordinator usage is excluded.
The canary check used request/receipt structure and matching private hashes,
without retrieving the random plaintext for an exhaustive scan. Descendant
absence beyond supervisor receipts and controller restart were not retested.

Evidence: `dev/agent-usability-runs/reliable-exec-fuzzer-02-20260905/`, especially
`review.json`, `scorecard.json`, `events.jsonl` and `cluster-after.json`.

## Money hardening checkpoints

The deterministic gate now interrupts a live CLI with a real unpaid mint quote,
checks passive balance while it is running, cancels with verified cleanup, pays
and resumes that exact quote, and verifies actual issuance. Existing funding,
payment, restart and isolation checks remain. Additional cases test already-paid
invoice and insufficient-funds rejection with exact fee accounting. Funding now
uses supervised LND execution with selected JSON output. Every public native
helper checks exit, timeout, stream completeness, truncation and cleanup.

`make e2e-cdk-wallet-fees` selects an explicit 100-ppk mint input fee. Its exact
fixture expectations separate total wallet debit from native melt fee reporting.
This local-image gate is excluded from the default suite, like the zero-fee gate.

`cdk-hardening-zero-02-20260905` passed with 5,000 → 4,300 → 4,000 sats, settled
recipients, independent wallet identity/volume state, no residual reservations,
and verified teardown. The first attempt stopped on a fixture cancellation call
missing its idempotency key; normal cleanup succeeded before retry.

`cdk-hardening-fees-03-20260905` passed at 100 ppk (0.1 sat per input proof, rounded per operation):
5,000 → 4,298 after the 700-sat payment → 4,297 after a deliberately fresh retry
of the paid invoice → 3,995 after the 300-sat payment. Each successful payment
reported a 1-sat native melt fee and debited two sats beyond its recipient amount.
The rejected fresh attempt cost one sat for its completed preparation swap.
Insufficient-funds rejection caused no further debit. Both rejected-operation
idempotent replays preserved their exact terminal receipts and caused no second
execution/charge. All final reserved, pending and pending-spent amounts were zero;
the other wallet remained empty, and teardown was verified independently.

Two earlier fee attempts exposed fixture assumptions: first, treating the native
melt fee as zero; second, assuming a failed fresh native attempt must leave the
balance unchanged. Both were retained as failed attempts with verified cleanup.
The latter is native protocol behavior, not unexplained loss: the pinned
[melt preparation code](https://github.com/cashubtc/cdk/blob/d3dec24c784e8fec1fd65f853241c7a2261c7abd/crates/cdk/src/wallet/melt/saga/mod.rs#L693)
performs a swap before submitting the melt, while compensation releases proof
reservations. The pinned source was inspected from the local Git object database.
CDK discovery guidance now explains that failure is not rollback and distinguishes
new native attempts from replaying an existing Proofstorm operation handle.

These are exact fixture expectations, not a general fee oracle. Evidence lives
under `dev/wallet-integration-runs/<run-id>/`; passing runs retain gate source,
binary digests, native receipts, passive balances, recipient observations and
closed/cluster-idle receipts.

## Legacy regression and agent follow-up

`cdk-hardening-cross-regression-02-20260905` passed its corrected scope: both
mint implementations funded and paid through Nutshell wallets, Nutshell's
anchored conservation oracle passed, cache/controller restart checks passed,
and teardown/cluster idleness were verified. The old round-trip treatment was
replaced with a distinct balance baseline and `wallet_pay` treatment. Failure
cleanup is now part of the fixture.

This does **not** establish CDK typed-oracle parity. The existing authoritative
mint-fee reader supports Nutshell SQLite; for CDK it deliberately withholds the
fee. The regression now requires that explicit refusal instead of inventing a
zero fee or treating the legacy wallet-local fee as authoritative. The initial
repaired attempt exposed this boundary and was cleaned up normally. Native CDK
money accounting is covered by the separate CDK CLI gate above.

`cdk-wallet-hardened-smoke-01-20260905` was incomplete despite clean teardown and
reporting: funding/issuance and payer-reported 700-sat payment succeeded, but the
step cleanup boundary prevented independent settlement confirmation, restart,
the second payment and final isolation checks. The agent spent many calls
guessing node invocation details before reading available catalog hints, used
the misleading mint-pending command, and initially created a channel whose mint
side balance was below its reserve. It recovered those issues but consumed
76 steps and 5,251,891 processed tokens (5,097,728 cached), peak context 118,087.
Its report also overstated unobserved post-melt reserved/pending amounts and
misidentified the cleanup trigger; independent review records the failure.

The follow-up guidance requires catalog hints/subcommand help first, at least
100,000 sats of usable outbound liquidity in both directions before wallet
funding, exact paid-quote resumption, preserved native exit status, and selected
parsed payment output. The MCP output-field description now enumerates supported
fields while staying inside its existing discovery-size budget. Optional context
and processed-token limits enforce an additional cleanup reserve; they count
completed-step observations, including cached input, and cannot cap an in-flight
provider generation. Seven proxy tests cover time/step/token boundaries, replay
latching, safe response annotation and refusal before forwarding.

`cdk-wallet-hardened-smoke-02-20260905` completed every financial milestone:
5,000 → 4,300 → 4,000 sats, both recipients independently settled, normal restart
preserved identity/balance, and the second wallet stayed empty. All 27 native
execution receipts verified cleanup. It finished in 482.5 agent seconds with
61 steps, 76 tool calls, 2,973,228 processed tokens and peak context 81,044.
Cleanup latched on processed tokens at 2,419,566, before the time/step boundaries;
verified teardown and its report finished within the hard cap. The cluster was
independently idle.

That run nevertheless **failed the output-policy gate**: payment wrappers
published grep-selected lines instead of using the required typed projection.
No raw preimage value was detected in inspected events, but that does not make
the filter robust. Its report overstated safe parsing. The final retry makes
`json_fields` mandatory for payments and recipient lookups and explicitly rejects
line selection/redaction as a passing strategy. This is agent guidance for the
existing output contract, not a new wallet API or a public-output sandbox.

`cdk-wallet-hardened-smoke-03-20260905` kept payment output behind typed
projection, but its handwritten parser synthesized `FAILED` on an unrecognized
status. Independent settlement and passive observations correctly established
the 700-sat payment and 4,300-sat balance; normal restart preserved both identity
and balance. Cleanup refused the 300-sat payment. It latched at peak context
80,740 and 2,259,643 processed tokens. The lab closed with verified absence, then
the hard cap ended the session at 3,017,428 processed tokens before a final report.
The completed-step overshoot was recorded rather than hidden. Peak context was
85,891; 58 steps and 75 tool calls consumed 638.1 agent seconds. No operator
rescue or budget extension was used. Independent review marks this run failed.

This exposed an unnecessary requirement in the scenario: a native text parser
was mandatory even though settlement and passive balances already proved the
money outcome. The default guidance now prefers **direct CLI invocation with
private output**, retaining typed JSON projection for native JSON or receipts
that are actually needed. It forbids invented status/amount defaults. The
zero-fee deterministic gate now tests that simpler path on its post-restart
payment, including command-scope exit status, empty public streams, verified
cleanup, actual recipient amount and the passive balance. No additional model
round is being launched. The revised prompt is not claimed as an agent pass.

`cdk-hardening-private-03-20260905` passed that final gate with exit 0. Its direct
private 300-sat payment had command exit 0, empty stdout/stderr, complete streams,
verified process cleanup, independently verified 300-sat recipient settlement
and a passive 4,000-sat final balance with zero reserved/pending/pending-spent
amounts. The native melt-fee field is explicitly null for this private receipt;
the exact zero additional debit is established separately. Quote cancellation,
resumption, rejected-payment replay and isolation also passed again. Normal
teardown returned `verified_absent: true`, followed by an independent idle audit.

The agent-usability frontier remains explicit: all financial milestones were
observed together in smoke-02, but no funded run met every output-policy,
efficiency and final-report gate together. Short repeated teardown waits also
consumed too much of the token reserve in smoke-03. These are recorded follow-ups
for benchmark ergonomics; the runtime and money-flow results are assessed
separately. Readiness for the next wallet does not certify general agent
proficiency.

## Validation and reproducibility

The final runtime build passed 212 workspace tests, strict workspace Clippy,
formatting, the existing MCP discovery-size budget, seven cleanup-proxy tests,
14 evaluator tests and release MCP doctor. Logs and a compact validation record
are retained in `dev/wallet-integration-runs/cdk-hardening-validation-20260905/`.
The controller and native supervisor were unchanged during this hardening round;
the earlier isolated Linux supervisor tests were not rerun here.

| Pin | Value |
| --- | --- |
| CDK source | `d3dec24c784e8fec1fd65f853241c7a2261c7abd` |
| CDK wallet image digest | `sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8` |
| Controller image digest | `sha256:f42a7d9ab93ae4920f8cbf5b3d9933ecec03b19ee406b55de09ba2cff9dfb5c0` |
| Native supervisor digest | `sha256:a503353d7e028fc3794d72a1229767e0586ef4e384030c99a637994536da75b9` |
| Agent-run release MCP digest | `sha256:53b007b0cc4218d7692bf5161ae9064f0054ce9e06e1fed941acd650fb6928a9` |

Images reside in `proofstorm-registry.localhost:5000`; the CDK CLI image is local
Linux arm64, not a published multi-platform distribution. Source remains an
uncommitted workspace build. Each agent run retains its source diff, exact
manifest, binary/harness digests and observed controller identity.
Deterministic gates use the debug MCP executable; its digest is recorded beside
the final gate source and acceptance executable in `gate-digests.txt`.

## Scope of the next-wallet decision

Readiness here means the existing CDK CLI 0.18.0/CDK mint 0.18.0/LND 0.20.0-beta,
SQLite, unauthenticated BOLT11/sat pairing has a defensible native execution and
money-flow baseline. It does not mean exhaustive concurrency, payment-in-flight
crash recovery, every mint/backend, production security or distribution, or mixed
wallet/ecash-transfer support. The next wallet should earn its own deterministic
vertical slice before those matrices grow.

Native CLIs own setup, funding and payments. Proofstorm's retained responsibilities
are component identity and lifecycle, leases, bounded execution and receipts,
passive observations, evidence and teardown. The rounds justified better native
discovery and output guidance, not wallet-specific top-level mutation APIs.
