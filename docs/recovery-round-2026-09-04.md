# Native wallet crash recovery round — 2026-09-04

This round exercises an actual Nutshell CLI process interruption after observing
genuine reserved proofs, followed by exact-quote reconciliation, payments,
restart, and teardown. Wallet databases are inspected read-only; no reservations
are manufactured and the wallet implementation is not modified.

The scenario is `native-reservation-crash-recovery`. Its mixed tool profile keeps
the typed refresh operation under test, with native CLI/process control for the
interrupted payment. This does not expand the default native MCP toolset.
Every session is fresh Kimi K3 and runs serially. Local artifacts and evidence
reviews live under `dev/agent-usability-runs/<run-id>/` (ignored by Git).


The recovery mechanism worked in the observed UNPAID cases, and the corrected
receipt was validated against native state. Agent reliability is not yet proven:
only session 2 passes all strict gates. Session 4 completed execution and stronger
proof-identity checks, but its final report contains unsupported claims. The
three-consecutive-clean-pass criterion remains unmet. Work is paused; the final
cluster audit reports verified idle, and a host process check found no remaining
benchmark session.

| Session | Reserved value released | Execution and review |
| --- | ---: | --- |
| 1 | 4,041 sat | Recovery observed; wrong receipt database and unsupported accounting explanation; fail |
| 2 | 1,314 sat | Correct receipt, settled payments after heal/restart, clean teardown; pass |
| 3 | 5,253 sat | Recovery and payments observed; timed out before export/teardown; operator cleanup; fail |
| 4 | 8,788 sat | Recovery, partial proof-identity consumption, restart payment and cleanup completed; report-fidelity gate failed |

## Defects and corrections

The previously corrected NULL predicate was insufficient: `auth.sqlite3` also
has a proofs table and sorts before the actual wallet database. The refresh
reader now selects the database containing the exact melt quote. The regression
fixture contains an unrelated authentication quote and balance, NULL and false
unreserved flags, and actual quote-linked reservations.

Refresh also exposed Nutshell's legacy local `fee_paid` field as an authoritative
Lightning fee. Neither that field nor the refreshed wallet object's default
zero establishes the actual fee. Refresh now emits an unknown fee instead.
Catalog guidance directs agents to mint/backend evidence and distinguishes
Lightning fees from wallet input and preparatory swap fees.

Repeated planning errors exposed another boundary problem: agents placed
platform fault actions in endpoint runtime requirements, or assigned wallet
controls to mint endpoints. The planning descriptions now say to copy advertised
endpoint controls, use `component_exec_live` for native CLI commands, and keep
platform faults separate. No new tools or CLI-specific wrappers were added.

## First session: recovery-crash1-k3-20260904

This session used controller image digest
`sha256:763d027d77221e0da0488bc9e7b96ed593d1a778a4146ba08c2d77c02eea1320`,
which had the NULL correction but still selected the wrong database.

The receipt preflight failed: native state and the balance tool both showed
97,999 sat, while refresh showed zero. The agent preserved the discrepancy.
Four native crash attempts then exercised distinct outcomes:

1. The payment settled despite partitioning. Killing its CLI left 3,031 sat
   reserved for a PAID quote. That is not an UNPAID release result.
2. A prefix query matched the old reservation and killed the next process before
   it created a new melt. This was a correlation error, not a successful crash
   at a new reservation.
3. After restarting the Lightning peer under partition, the payment failed and
   the observer sampled no new reservation. This cannot rule out a transient
   reservation between samples.
4. A faster observer caught eight proofs totaling 4,041 sat for a new quote.
   The CLI was killed; its reservation persisted. A direct mint query established
   UNPAID state. Native state before and after typed refresh showed reservation
   4,041 → 0 sat and available balance 90,927 → 94,968 sat. The receipt still
   returned zeros, demonstrating the observation defect independently of the
   actual release.

Payments of 2,500 sat after healing and 1,500 sat after restarting both wallet
and mint settled. The last payment had a 1 sat discrepancy in the conservation
oracle; the report's attribution to a local fee value and invented msat unit was
unsupported. Preparatory swap fees are a hypothesis, not established by this
session's evidence. The PAID quote's separate 3,031 sat reservation remained.

Overall review: fail. Native UNPAID recovery was observed, but the typed receipt
failed and the report contained an unsupported accounting explanation. Agent
teardown and the independent idle audit both succeeded; no rescue was required.

Evidence: events 91/111 (receipt discrepancy), 201/204 (correlation error),
213/216 (real crash), 222/228/235 (exact quote and release), 261/282 (settled
payments), 286/292/316 (accounting limitation), and 310 (verified teardown).

## Corrected controller

Subsequent runs use
`sha256:2180514827b814a255f1fef5148b72cff18a8752193edd11c943050a4a7baab7`.
Build and rollout records are in `dev/agent-usability-runs/recovery-round-20260904/`.
The original pinned Docker recipe was retained; an isolated anonymous Docker
configuration bypassed a stalled credential lookup during the build.

## Second session: recovery-crash2-k3-20260904

The corrected receipt passed preflight: refresh, native SQLite inspection, and
the balance tool all reported 42,999 sat. Five real proofs totaling 1,314 sat
were observed during a native CLI payment of a 1,300 sat invoice. The process was
killed and reaped with exit status -9, leaving that exact reservation intact.
Refresh reported UNPAID and released all five proofs: reserved amount 1,314 → 0,
available balance 41,685 → 42,999. Native state independently agreed.

The agent first observed an established Lightning session surviving a bounded
partition poll. It then used native `lncli disconnect` and checked empty peer
lists and inactive channels on both sides before starting the payment. A
1,500 sat payment after healing and a 1,700 sat payment after wallet/mint restart
both settled. The final accounting oracle had zero delta. Teardown and the
independent idle audit succeeded without rescue.

Overall review: pass for UNPAID reservation recovery, with reporting limits:
receipt state-before is wallet-local and state-after is the mint response; no
independent pre-refresh mint query was made. The native post-check established
zero reserved proofs, without separately enumerating cleared melt-id fields.
One successful crash capture does not establish retry robustness.

Evidence: events 92/103 (preflight), 140/154/161 (fault and crash), 168/176
(receipt and native verification), 206/233/237 (settlement and conservation),
and 265 (verified teardown).

## Third session: recovery-crash3-k3-20260904

This session used a 5,200 sat invoice and a suspend-then-kill interruption.
The first crash attempt sampled no reservation and was retained as a miss.
The retry caught five proofs totaling 5,253 sat, confirmed process state `T`,
observed the reservation persisting through a stopped interval, and killed the
CLI with exit status -9. A fresh read correlated the exact quote, and a direct
mint request reported UNPAID. Refresh reported reserved amount 5,253 → 0 and
available balance 34,746 → 39,999; native state agreed.

Stopped-state timestamps span about 5.010 seconds. The script's separate
`MEASURED_SUSPEND_INTERVAL_SECONDS=5.0` line printed its configured sleep;
the timestamp observations support a roughly five-second pause, not an exact
5.000-second timing claim.

Payments of 6,000 sat after heal and 4,000 sat after wallet/mint restart settled.
However, the session reached its 1,500-second budget before evidence export and
teardown. Operator cleanup deleted only this run's lab; its normal finalization
then reached verified idle. Overall result: fail. The underlying recovery was
observed, but this is not a clean benchmark completion or a consecutive pass.

Native friction included guessed SQL table/column names, `cashu` absent from
PATH, an unsupported invoice flag, and an invoice waiter reaching the exec
timeout. The agent recovered using the catalog's Python CLI entrypoint and
explicit waiter supervision. A timeout does not prove the remote process exited;
the exec description now makes this limitation explicit. No runtime watchdog
or new process-control API was implemented in this round.

Evidence: events 75/89 (SQL errors), 126/132 (entrypoint), 144/150/156 (invoice
recovery), 163/169 (miss), 181/187 (stopped process and exact quote), 193/200
(release), 233/260 (payments), plus `limit-reason.txt`, `operator-cleanup.json`
and `cluster-after.json`.

## Fourth session: recovery-crash4-k3-20260904

Receipt preflight agreed across native inspection and typed tools at 94,999 sat.
The agent partitioned two independent Lightning backends and explicitly
disconnected their peer session. Both peer lists became empty and channels
inactive before the interrupted payment.

The first observer attempt sampled no reservation. The second observer had a
Python syntax error after launching the CLI; the agent checked that the payment
process had exited and no reservation remained before trying again. These are
different failures, and neither establishes a reservation-window duration.

The third attempt captured seven genuine proofs totaling 8,788 sat for the exact
8,700-sat melt quote `01a06f82-408d-741f-8e5c-21e7d7638698`. The agent stopped the
native CLI, confirmed process state `T`, retained timestamped reservation samples,
and killed it after a measured 3.527-second interval from stopped-state confirmation.
The process was dead but unreaped (`Z`) at the immediate post-kill observation.
A direct mint request established UNPAID state, while native wallet inspection
still showed all seven proofs reserved.

Refresh released all seven: reserved value 8,788 → 0 sat and available value
86,211 → 94,999 sat. Native state and typed balance independently agreed. A
successful 6,000-sat post-heal payment consumed two of those exact released proofs,
identified by hashed secrets in the used-proof table for that payment's quote:
513 sat of the released set. Five proofs totaling 8,275 sat remained available;
their consumption was not tested. After restarting both wallet and mint, a
7,000-sat payment settled, with final balance 81,997 sat and conservation delta zero.

Evidence export, lease release, experiment closure and agent teardown completed.
The final independent cluster audit found no blockers. Restart and namespace
teardown establish final process cleanup; the post-restart process scan matched
only its own inspection shell.

Execution is valid and the target property held, but strict overall review is
fail because the final report exceeds its evidence:

- It says the missed reservation completed within the 50 ms poll granularity.
  A polling miss does not establish that timing.
- It calls all 88 sat of reservation overhead the fee reserve, while the mint
  quote explicitly reports 87 sat. Total input overhead and the advertised
  Lightning fee reserve must remain distinct.

The successful observer performed more than 800,000 busy-loop queries. Its average
loop rate is not a maximum sampling gap, and the observer itself changes workload.
This is functional recovery evidence, not a production timing measurement.
An earlier broad invoice-row inspection also unnecessarily included a test signing
key in local evidence; future inspection should select only required columns.

Evidence: events 123/131 (preflight), 165/173 (disconnect), 210 (miss),
230/236 (script failure and recovery), 251/254 (actual crash), 260/266/273
(mint state and release), 287/306 (proof identities), 313/333/336
(restart and settlement), 343/352/358 (cleanup/export/teardown), and 361
(final report). The independent decision is saved in `review.json` and applied
by the evaluator to `scorecard.json`.

## What this says about Proofstorm's surface

Native CLIs were sufficient for peer disconnection, real payment interruption,
process observation and read-only forensic checks. There is no evidence here
that these need individual MCP wrappers. Proofstorm's useful responsibilities
were topology/lifecycle management, fault policy, execution in the right component,
evidence capture, cleanup and portable observations that can be checked against
native state. The refresh receipt defect illustrates why those observations need
independent validation.

Boundary confusion was real: agents guessed endpoint controls, assigned controls
to the wrong component, assumed executables/utilities existed, guessed CLI flags
and database schemas, and sometimes treated a successful exec wrapper as evidence
that every shell command succeeded. Description and catalog corrections address
some discovery friction; they do not establish that it has been eliminated.

The default native profile remains 34 tools. These sessions deliberately used the
47-tool experiment profile to test the typed refresh contract. They therefore do
not establish performance with only the default native profile. No new MCP tools
or wallet-specific CLI wrappers were added.

## Cost and the stopping decision

| Session | Observed agent minutes | Model steps | Tool calls | Peak context | Processed tokens, including cached input |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 24.4 | 97 | 108 | 96,297 | 5,293,725 |
| 2 | 20.3 | 79 | 95 | 86,851 | 4,289,651 |
| 3 | 24.8 | 80 | 91 | 88,170 | 4,114,037 |
| 4 | 29.0 | 109 | 121 | 120,558 | 7,587,461 |

The four benchmark sessions total roughly 98.5 observed agent minutes, 365 model
steps, 415 tool calls and 21,284,874 processed tokens. Of these, 20,596,480 were
reported cached input; uncached input was 539,949, output 99,493 and reasoning
48,952. These are harness/provider token counters, not a dollar-cost estimate.
They exclude the coordinating Codex task, build/test time and time between sessions.

Continuation had declining value once receipt correctness, genuine release and
post-restart settlement were established. The final proof-identity check added
useful evidence, but further repetitions should wait for a narrower question and
an explicit budget. A report is now more valuable than another run.

Proposed operating policy for the next authorized round: define the question and
stop conditions first; reassess every 10 minutes or 10,000 new tokens; reserve
20% of the budget for evidence and cleanup; stop repeating equivalent failures
after two attempts without a changed hypothesis; report at the cap rather than
automatically extending. These checkpoints are proposed, not implemented in the
benchmark harness. Budgeting should cover both the coordinator and benchmark
agents and track cached input and peak context separately.

## Validation, remaining work and final state

The runtime corrections passed 205 workspace tests, including the six wallet
quote driver tests. Subsequent MCP description changes passed all 52 MCP tests
and existing payload budgets. Workspace Clippy with warnings denied, formatting,
and whitespace checks passed. Earlier evaluator/audit checks passed 14 Python
tests. Existing validation logs are under the round artifact directory; reporting
was completed without launching another experiment.

Remaining questions, in priority order:

1. Add bounded execution/checkpoint accounting before another long benchmark.
   The exec timeout currently does not prove remote process exit and may lose
   partial output; documenting this does not fix it.
2. Investigate the separate PAID-quote crash residue (3,031 sat) and the unexplained
   1 sat accounting delta from session 1 as distinct, bounded questions.
3. Tighten agent report fidelity: separate sampled observations from timing
   inferences and distinguish fee categories. Preserve functional outcomes even
   when the overall agent benchmark fails.
4. Resume a bounded stability round only after selecting its question and budget.
   Broader implementation coverage and wallet expansion remain untested here.

All four final cluster audits report verified idle. Session 3 required recorded
operator cleanup; sessions 1, 2 and 4 completed their own teardown. The final audit
is `dev/agent-usability-runs/recovery-crash4-k3-20260904/cluster-after.json`, checked
at Unix time 1788577713. A subsequent host check found no active benchmark process.
The shared controller remains available on image digest
`sha256:2180514827b814a255f1fef5148b72cff18a8752193edd11c943050a4a7baab7`.
Changes are in the working tree and are not committed. No new experiment is running.
