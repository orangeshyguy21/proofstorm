# Native-first validation — 2026-09-04

This is the first execution round of the
[native-first plan](native-first-experiments.md). Runs use fresh, serial Kimi K3
sessions. The experiment agent has no host shell or operator access; native
commands run inside the actual lab components through Proofstorm.

## Changes exercised

- Supplied OpenCode profiles now default to `native`. The existing `experiment`
  toolset remains available for typed-contract regressions.
- The final native profile exposes 34 tools versus 47 in `experiment`. It keeps
  lab/candidate lifecycle, bounded execution, faults, capability discovery,
  evidence, and useful observations. Wallet mutations, routing policy, the
  two-LND bootstrap and its dependent peer/channel helpers, authentication
  helpers, and the typed-wallet-dependent conservation helper are hidden.
- CLI guidance identifies POSIX `/bin/sh`, warns about partial mutations, and
  corrects the Nutshell wallet invocation. The bootstrap helper is documented
  as optional and specific to its two-node workflow.
- Candidate receipts disclose the configured Nutshell dependency adjustment.
- The evaluator separates execution, target findings, and evidence sufficiency.
  Native command counts and nonzero exits are observations. Manual review gates
  need evidence references; a completed transcript alone cannot earn a pass.
- Idle checks cover lab namespaces, resources, actions, candidate jobs/pods and
  storage. Terminal candidate resources are retired after preserving receipts.
  Operator rescue cleanup cannot count as agent teardown success.

## Reviewed runs

Artifacts are under `dev/agent-usability-runs/<run-id>/`. That directory is local
and ignored by Git. Each reviewed run has `review.json`, `scorecard.json`, the
original transcript, and a final cluster audit.

| Run | Result | Evidence and limitation |
| --- | --- | --- |
| `native1-k3-20260904` | Pass, original 47-tool profile | Native wallet send/receive, typed balance cross-check, offline inspection and restart persistence. Balance 2,000 → 1,936 → 1,999 sat; 1 sat mint fee accounted for. CLI invocation and POSIX shell issues recovered. |
| `native2-k3-20260904` | Fail | Native invoice/payment minted 2,100 sat without wallet mutation wrappers. However, misleading bootstrap guidance caused an unnecessary rebuild, and the final report incorrectly called the helper the only safe funding path. |
| `native3-k3-20260904` | Pass, 33-tool profile | Native Bitcoin/LND funding, peer connection, channel opening and confirmation, and 25,000 sat settled payment. One lab; no bootstrap/peer/channel/payment wrappers. Zero routing fee; 166 sat funding fee and matured coinbases accounted for. |
| `candidate-negative1-k3-20260904` | Pass | Malformed PR number and unsupported repository rejected; zero candidate builds and zero materializations. Recovery guidance reported without substituting another PR. |
| `native-overlap1-k3-20260904` | Execution valid; report fails review | CLN mesh isolated two overlapping paths, retained a working control, selectively healed one path, then restored both affected peerings. The final report incorrectly attributed established-peer teardown to policy/timers despite successful native `disconnect` calls. |
| `candidate-native-round1-k3-20260904` | Pass | Exact candidate selected; 2,048 sat funded and independently checked with wallet balance. Commit/image and dependency adjustment disclosed. One positive run, not the three-run stability criterion. |
| `recovery-native1-k3-20260904` | Release inconclusive; explanation fails review | A payment succeeded under partition, then native disconnect established actual interruption. Two UNPAID refreshes observed zero reserved proofs. Healing restored a 2,000 sat payment; balance 89,997 sat persisted after mint restart. The claim that proofs were never reserved exceeded the observations. |

The native profile evolved during the round: 47-tool baseline, 36 tools, then
33 tools after the bootstrap detour, then 34 including fault-capability
discovery. Prompts and guidance also changed. These are development iterations,
not an isolated causal comparison of model speed or toolset size.

The first run's shell script was edited while its agent ran, disrupting harness
postprocessing. Only export/grading was replayed from the completed saved
session, with no agent continuation or lab intervention. The runner now parses
its complete execution body before starting and snapshots the evaluator and
scenario corpus. `harness-repair.json` preserves the baseline limitation.

## What the evidence justifies

Native CLIs can operate and mutate the deployed software while generic
Proofstorm lifecycle and evidence tools manage the experiment. The direct LND
run demonstrates that the old bootstrap restriction is not a general network
requirement. The CLN run demonstrates composition across another implementation
without adding implementation-specific MCP mutations.

Balance observations earned a place as independent checks of native wallet
changes. Reachability observations added a separate transport check beside
native Lightning peer/ping observations. Provisioning, fault administration,
restart, waits, evidence and cleanup provide platform responsibilities that the
software CLIs do not replace.

The causal-attribution failure is substantive. The CLN script explicitly
disconnected A-B and A-C, and both calls succeeded. Its later peer observations
cannot establish that NetworkPolicy alone killed established connections or
measure the timer responsible. Automated success counts missed this; evidence
review caught it. Control availability was sampled, not monitored continuously.

Candidate validation used commit
`7de5d9dd7145a4b9c3b9f15304a19a593c704ee4` and image digest
`sha256:4c85f10da624035a0452df913a122c4d43d98ed63632bb98a8aba8189ff84746`.
The configured build removes `breez-sdk-spark` from the Poetry lock and installs
version `0.17.0` with pip. This establishes viability of that adjusted candidate
image, not an untouched upstream build or correctness of PR-specific fee
estimation. Runtime identity is supported by the published immutable lock and
successful materialization/flow; no separate container image-ID attestation was
exported by the agent.

The recovery run exposed a receipt bug: refresh reported zero available balance
while the balance operation reported 91,998 sat. Nutshell 0.20.3 stores fresh
proofs with `reserved=NULL`; the receipt's `WHERE NOT reserved` excluded them.
The query now treats NULL as unreserved, and the driver contract test covers
both NULL and explicit false values. This fix was made after the run and has
not been deployed or validated in a fresh live experiment. The relevant pinned
upstream sources are [proof insertion](https://github.com/cashubtc/nutshell/blob/0.20.3/cashu/wallet/crud.py)
and [schema migration](https://github.com/cashubtc/nutshell/blob/0.20.3/cashu/wallet/migrations.py).

Zero `input_proof_count` for an UNPAID melt describes the absence of spent proofs
in the driver's accounting query; it cannot establish that no transient
reservation existed. The agent correctly withheld a release verdict, but its
explanation asserted a failure mechanism it had not observed. The evaluator
now prevents an inconclusive release gate from masking a failed evidence review.

## Acceptance still outstanding

- Deploy and live-check the corrected reservation receipt. Then exercise an
  actual wallet interruption after observing a genuine reservation, using native
  process control and read-only state inspection. Do not manufacture database
  reservations. Preserve the distinction between wallet crash effects and
  network partition effects.
- Genuine reserved-proof release, restored spendability and persistence after
  restart, with three consecutive varied passes.
- Three consecutive candidate funding passes.
- The asymmetric-fee/liquidity, restart-during-traffic and liquidity-exhaustion
  composition families, plus repeated timing-sensitive cases.
- Held-out experiments, a second wallet and the model ladder remain subsequent
  stages. No claim of production throughput or SLA fidelity follows from these
  local regtest runs.

## Validation

The workspace test run passed 205 tests; Clippy with warnings denied and Rust
format checks passed. All 14 Python evaluator/audit tests passed, and all six
wallet-driver contract tests passed again after the NULL-balance correction.
The evaluator tests cover unknown review
gates, expected native failures, duplicate observations, real reservation/value
transitions, cleanup failures, and unsupported conclusions after valid execution.
The native OpenCode profile passed the real MCP doctor handshake with 34 tools.

All seven sessions ran serially and completed agent teardown without operator
rescue. The final independent cluster audit found no lab namespaces, actions,
candidate builds/jobs/pods, or lab storage. The shared controller and Kubernetes
services remain running. Audit: `dev/agent-usability-runs/native-first-round/cluster-final.json`.
