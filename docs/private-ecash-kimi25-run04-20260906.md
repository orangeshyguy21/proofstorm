# Private ecash run04: technical target held; report needs correction

Run `private-ecash-agent-04-20260906` completed the bounded native 70-sat cocod→CDK and 30-sat return transfer. Successful private CDK reconciliation preceded both final passive observations: **cocod 4,960 sats / CDK 40 sats**, with all supported reservation, inflight and pending categories zero. This resolves run03's evidence-ordering gap without establishing a recovery defect in that earlier run.

The exact reviewed score is **execution valid / target property held / evidence insufficient / proficiency failed**. The remaining failed gate is `claims_supported_by_observations`: the agent's final report incorrectly calls CDK's private token input stdin. The durable request used private **argv index 8**; cocod used stdin. Its absolute secret-absence statement also exceeds the bounded evidence review. These reporting defects do not invalidate the independently retained transfer, balance or teardown observations. Run03 remains failed under its original review.

## Verified observations

| Observation | Evidence/result |
| --- | --- |
| Disclosed assisted setup | Real LND funding `SUCCEEDED`, value `5000`; issuance and passive initial balances 5,000/0 |
| First transfer | `agent-send-cocod-70`, `agent-receive-cdk-70`; independent CDK balance 70 |
| Return transfer | `agent-send-cdk-30`, `agent-receive-cocod-30`; independent cocod balance 4,960 |
| Reconciliation | `agent-check-pending` terminal exit 0 at Unix 1788710731, private output, empty ordinary streams |
| Fresh final observations | `agent-final-balance-a` accepted 1788710739; `agent-final-balance-b` accepted 1788710741; final 4,960/40 and zero supported unsettled categories |
| Private custody | Four native capture/consume receipts: empty ordinary streams, retired private files and verified cleanup; both references released |
| Native execution | All 13 setup/model receipts: exit 0, no signal/timeout/cancellation, complete untruncated streams and verified cleanup |
| Argument boundaries | All six custody actions' restricted fields matched OpenCode before-hook, proxy receipt/forward and durable MCP request |
| Teardown | Agent observed `verified_absent: true` before its final report; independent audit confirmed idle with no blockers |

No operator intervention, cleanup rescue, accepted mutation replay or extra transfer occurred. Public output was limited to two safe help invocations; reconciliation remained private. The disclosed help-only guard refused no requests in this run. Cleanup admission latched at 40 completed steps, after both custody releases; the model completed export, close and reporting within the original cap. This run did not separately attempt a forbidden new command after the latch.

The encoded Cashu-token scan found no matches in the documented public transcript, session, log, audit and setup-artifact scope. It does not prove absence of every secret representation. No private wallet or execution-output files were fetched. Argument snapshots begin after client parsing and do not reconstruct raw provider output.

## Accounting and discovery

There are **27 journal actions**: setup has six native executions, one restart, one liquidity bootstrap and two balances; the agent has seven native executions, six custody actions and four balances. Native receipts total **13 = 6 setup + 7 agent**. There were zero tool refusals and no repeated failed plan.

Kimi 2.5 used **498 runner wall seconds, 45 model steps, 1,709,582 processed tokens and 57,886 peak context tokens**. OpenCode reported cost **0.18758254**; billing was not independently verified. Caps remained 600 seconds, 50 steps, two equivalent attempts, 100,000 context tokens and 3,000,000 processed tokens. Assisted setup completed before the model timer.

The model used catalog discovery, two native help calls, generic native execution, custody and passive observation tools. No new wallet mutation wrapper was needed. Fourteen individual operation-status calls and four todo updates consumed part of the budget; this is a possible efficiency improvement, not a reason to repeat a successful funded flow. The input-binding error was in the final report, not the submitted command.

This was assisted planning and prefunding under one principal and a whole-lab lease. It establishes neither independent-principal authorization nor unassisted topology/setup discovery, fault recovery, nonzero fees or general interoperability.

## Pins and handback

- Model: `openrouter/moonshotai/kimi-k2.5`, using existing configuration.
- Controller: `sha256:e502c83e2570540a9a12ac9792eb102003ceff8d00af60d2cbbd0d3b71ee1d81`.
- Runner: `sha256:8acb76b8b194d7f679c3f448ca9c05d298e070f1aafc82631f5452a3fd6eb2d0`.
- Release MCP: `sha256:32375df6ea1dc3b7dd0f57857aa966554c03a748a7930d262f57b90c8b691624`.
- Evidence lock: `sha256:58be851dc39381f2ce12f223e96a71a799ab34c799e190baf4a10878abbfb0d5`.

Evidence is retained locally under `dev/agent-usability-runs/private-ecash-agent-04-20260906/`, including the reviewed scorecard, review, journal/argument/privacy/cost audits, unchanged agent final report, setup receipts, transcript, harness copies/digests and independent `operator-cluster-after.json`.

The Wallets coordinator accepted cluster handback and the technical result. No further live/model run is authorized automatically, and this funded flow will not be repeated merely to correct report prose. The coordinator now owns integration and the deterministic gate for separately scoped principal handoff; another fuzzer round requires its explicit handoff and new pins.
