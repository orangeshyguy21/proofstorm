# Cocod lifecycle-02 review

**Not passed.** Private initialization, independent permission observation and
structured recovery hashing improved. The raw-status policy violation repeated,
the full lifecycle sequence remained incomplete, and the agent hit its step cap
before reporting. Normal teardown and two independent cluster audits verified
idleness. No cocod runtime defect was reproduced.

Run `cocod-lifecycle-02-20260905` used the same assisted six-component plan,
experimental wallet pin and Kimi K3 as lifecycle-01. Limits remained 600 seconds,
50 steps, two equivalent attempts, 100,000 context tokens and 3,000,000 processed
tokens. Funding was not dispatched.

## Results

- Private initialization preserved native exit 0, separately from observations
  (`events.jsonl:88`, `:91`).
- A separate `stat` returned `mode=600` (`:87`, `:91`). The rendered owner name
  was `UNKNOWN`; no numeric UID was established by that observation.
- Identity hashing used the documented authenticated recovery response's
  `mnemonic` string, kept private; only its SHA-256 was published (`:95`, `:98`).
  A second hash was not reached, so persistence is unproven in this run.
- Locked/stopped protected setup, native lab-mint configuration, configured
  restart, explicit unlock, running session and passive zero balance were
  observed (`:98`, `:104`, `:116`, `:122`, `:129`).
- Native stop exited 0 (`:136`), but cleanup refused stopped-state status and
  balance observations (`:139`, `:140`). Subsequent start/restart, identity
  recheck and explicit re-unlock were not reached.
- All 27 native receipts had complete, untruncated streams and verified cleanup.
  One read-only grep returned 1 for no match; all other native exits were zero.

## Remaining failures

The agent submitted direct `cocod status` with public output at `:34`; `:39`
retained the full response, including `lastFailure`. Later code correctly
validated finite lifecycle fields, but that does not erase the early violation.
No raw recovery material or credential was observed in inspected output; this
was not an exhaustive known-secret scan.

Discovery consumed substantial steps: five help calls and nine documentation or
source reads preceded initialization. Cleanup latched at 40 completed steps,
about 340.6 seconds. Five close waits requested 60 seconds but were shortened by
the proxy to at most ten seconds. Verified closure arrived at `:167` with about
172 seconds of wall budget left. The agent then spent its last step on a todo
update at `:170`, hitting `max_steps:50` before a final report. Short polling,
prior discovery cost and the final todo all contributed; changing wait length
alone is not proven to solve the checkpoint.

The evaluator records execution `failed`, target property `inconclusive`,
evidence `insufficient` and proficiency `failed`. Runtime success, agent
proficiency and evidence sufficiency remain separate conclusions.

## Changed hypothesis for lifecycle-03

The Wallets coordinator added fixed shared `json_fields` projections for native
cocod health and lifecycle leaves, plus the exact bundled API documentation path.
Their isolated `cocod-projection-01-20260905` gate passed before releasing the
cluster. This removes the need for agent-written public status filters.

The benchmark proxy now allows a cleanup `lab_wait` targeting `closed` to retain
a valid requested wait up to 60 seconds, leaving a 30-second reporting margin
when possible. Once inside that margin, the server's valid one-second minimum
applies. Other cleanup observations remain at most ten seconds; work-phase
boundary clamping, execution deadlines, admission and hard caps are unchanged.
Invalid wait timeouts are forwarded unchanged for MCP validation. Eleven focused
proxy tests and 14 evaluator tests passed. The lifecycle-03 brief requires the
shared projections and discourages spending cleanup steps on todo updates.

This is a concrete changed-hypothesis retry, not another equivalent prompt-only
attempt. Its outcome must be reviewed separately before funded smoke.

## Evidence and cost

Artifacts: `dev/agent-usability-runs/cocod-lifecycle-02-20260905/`, especially
`review.json`, `reviewer-audit.json`, `scorecard.json`, `cleanup-waits.json`,
`events.jsonl`, `cleanup-phase.json`, `limit-reason.txt`, `cluster-after.json`
and `operator-cluster-after.json`.

The run used 429.9 agent seconds, 50 steps and 67 tool calls. Processed tokens:
2,105,750, including 2,019,840 cached; peak context: 65,213. Coordinator usage is
excluded. No operator cleanup rescue or cap extension occurred.

This run used controller digest
`sha256:e9d3238b7ba216bef7afea1623f8d63fdc722545c050d06383c6b6072cc127a7`
and release MCP digest
`sha256:358678f801f79998007173547ea9295eb3b4f34e41b79a15660dbbff1a4f4764`.
The cocod image remains
`sha256:88dc907f64530788280b0ba603b1bd7f361c58281171e74ca25b0676fadfcdc7`
from `44e5101cbea370132af6e68f88e01b47e39431c4`.
