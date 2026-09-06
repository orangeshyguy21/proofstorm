# Cocod lifecycle fuzzer checkpoint

**Lifecycle behavior held; the agent checkpoint did not fully pass.** The single
bounded run completed its lifecycle observations, verified lab teardown and
returned a report within its cap. Review found output-policy and reporting
errors, so the funded smoke remains undispatched. No concrete cocod runtime
defect was reproduced.

Run: `cocod-lifecycle-01-20260905`, scenario `cocod-lifecycle-preplanned`, model
`kimi-for-coding/k3`. Planning was assisted: the operator verified the exact
six-component topology before dispatch. Only wallet-1 was exercised; wallet-2
was left untouched. There was no funding or fault injection.

## Observed behavior

| Check | Evidence |
| --- | --- |
| Healthy daemon before wallet initialization | `w1-health-1`, `w1-status-2`: health good, wallet/seed access absent, session stopped |
| Protected initialization | `w1-status-3`: initialized, seed access locked, passphrase required, session stopped |
| Lab mint configured before session startup | `w1-config-1`, `w1-restart-1`, `w1-sstart-1`: native config changed to `http://mint:3338`, restart, explicit unlock |
| Usable session and passive observation | `w1-sstart-1`, `w1-bal-1`: session running; balance, reserved, total-ready and inflight all zero |
| Session stop preserves daemon health and passive reads | `w1-sstop-1`, `w1-bal-2`: daemon healthy, session stopped, seed locked, balance zero |
| Normal restart preserves identity and locks session | `w1-restart-2`, `w1-verify-restart`, `w1-bal-3`: same recovery-text hash, healthy daemon, stopped/locked session, passive zero before unlock |
| Explicit unlock after restart | `w1-sstart-3`, `w1-bal-4`: available/running and passive zero |
| Cleanup | All 19 native executions had complete, untruncated streams and verified cleanup; agent observed closed receipt at `events.jsonl:151`; independent cluster audit idle |

There were 25 journaled actions: 19 native executions, two component restarts and
four passive observations. Native exits were seventeen zeroes, one shell-quoting
failure (1) and one initialization-output parser failure (2). Operation success
was not substituted for the receipt's actual exit status.

## Why the agent checkpoint failed review

1. **Initialization and parsing shared a receipt.** The CLI initialized the
   wallet successfully, but the wrapper assumed JSON and exited 2 when it found
   human-readable output. That replaced the native mutation's exit in the shell
   receipt. The agent correctly checked status rather than initializing again.
2. **Public status filtering was too broad.** An early recursive walker accepted
   arbitrary short strings based on their characters and length. Later status
   projections selected relevant fields but did not validate their finite enums.
   Inspected outputs contained lifecycle data, but this does not satisfy the
   requested fail-closed output policy.
3. **The final report overstated evidence.** It claimed files were shredded,
   while commands used `rm`. It also claimed `PASS_MODE=600` was confirmed, but
   the initialization parser exited before that permission-observation command.
   The script issued `chmod 600`; an independent permission result was absent.

The evaluator therefore records execution `failed`, target property `held`,
evidence `insufficient`, and proficiency `failed`. These are agent-contract
failures; the observed daemon/session behavior is preserved as a separate result.

No raw credential or recovery material was observed in inspected public outputs.
The raw mnemonic was not retrieved for an exhaustive known-secret scan. Identity
comparison used a hash of heuristically extracted native recovery text, rather
than an independently validated structured mnemonic field.

## Changes and next checkpoint

Added the exact-version plan fixture and lifecycle scenario. The revised brief
now explicitly separates private initialization from read-only observations,
documents the human-readable CLI output, requires explicit field/type/enum
validation for public status, and requires reports to distinguish issued commands
from verified results. It also prohibits claiming secure erasure from `rm`.
No runtime, controller, wallet image or MCP API was changed for this fuzzer round.

The revised brief has **not** been rerun. The next checkpoint is one bounded
lifecycle retry of these agent-contract fixes, followed by review. Funding should
wait for that result. No longer cap, model ladder or wider fault campaign is
indicated. Native CLIs/API remain the wallet surface; Proofstorm supplies
lifecycle, execution receipts, passive observations, evidence and teardown.

The scenario/evaluator validation passed 14 evaluator tests and seven cleanup
proxy tests. The handoff's 215 Rust tests and deterministic money gate were not
rerun in this agent-only round.

## Budget and reproducibility

The run used 534.5 agent seconds, 47 model steps and 56 tool calls under unchanged
600-second/50-step limits. Cleanup latched at 40 completed steps, about 454.6
seconds, before the wall-time boundary. Usage was 1,777,233 processed tokens,
including 1,697,536 cache-read tokens; peak context was 58,277. Enabled ceilings
were 3,000,000 processed and 100,000 context tokens. Coordinator usage is excluded.
The agent returned its final report after verified closure; no operator rescue
or budget extension was used.

- Wallet: `cocod-wallet` `0.0.17-dev.44e5101c`, source
  `44e5101cbea370132af6e68f88e01b47e39431c4`.
- Wallet image digest:
  `sha256:88dc907f64530788280b0ba603b1bd7f361c58281171e74ca25b0676fadfcdc7`.
- Controller image digest:
  `sha256:e9d3238b7ba216bef7afea1623f8d63fdc722545c050d06383c6b6072cc127a7`.
- Release MCP digest:
  `sha256:358678f801f79998007173547ea9295eb3b4f34e41b79a15660dbbff1a4f4764`.

Retained evidence is under
`dev/agent-usability-runs/cocod-lifecycle-01-20260905/`, particularly
`review.json`, `reviewer-audit.json`, `scorecard.json`, `events.jsonl`,
`cleanup-phase.json`, `cluster-after.json`, the manifest and the operator
preflight plan/audit. No agent funding, multi-wallet isolation, fee behavior,
crash recovery, ecash exchange or broader compatibility is claimed.
