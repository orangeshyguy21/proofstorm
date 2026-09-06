# Private ecash agent checkpoint — 2026-09-05

**Outcome: failed usability checkpoint; no model-operated ecash transfer.** The independent post-run audit verified an idle cluster, and ownership was returned directly to the Wallets coordinator. Product/controller/runner code and deployed pins were unchanged throughout the run. No second model or funded round was launched.

## Attempt and assistance

The exact six-component plan used cocod wallet-a, CDK CLI wallet-b, CDK mint with zero input fees, Bitcoin and two LND nodes. Planning and prefunding were explicitly assisted. The operator initialized protected cocod, configured only the lab mint, restarted/unlocked its session, funded 5,000 sats through real Lightning, verified issuance, initialized empty CDK state and independently observed 5,000/0 passive balances. The model continued that same experiment, principal and exclusive whole-lab lease.

An earlier unfunded setup attempt, `private-ecash-agent-01-20260905`, failed after submitting protected initialization. The fixture called `operation_wait`, which the experiment profile does not expose. Offline reproduction retained the exact `-32602 tool not found` response. No invoice, payment or model call occurred; the initialization's terminal native receipt was not retained, so its completion is not claimed. The owned finalizer removed the lab and an independent audit verified absence. The failed attempt remains recorded.

With coordinator authorization, the fixture was corrected and one fresh setup attempted. Corrections use the advertised batch wait, preflight required tools, handle buffered JSON-RPC notifications with a bounded byte reader, and match LND's actual `value_sat: "5000"` projection. Six offline fixture tests and fifteen harness tests passed. The idle audit now distinguishes Bound PVCs mounted by the controller from residual lab storage; other claims still block idleness.

## Single model result

`private-ecash-agent-02-20260905` used the default configured Kimi K3 model, 600 seconds, 50 steps, two equivalent attempts, and the existing 100,000-context/3,000,000-processed-token ceilings. Cleanup was scheduled at 480 seconds or 40 steps. The objective was 70 sats cocod→CDK, 30 sats CDK→cocod, native reconciliation and final balances 4,960/40.

All eleven preparation actions were refused before native execution. OpenCode event inputs omitted `destinationComponent` and `maximumBytes`; the persisted MCP requests contain nulls for those fields. The frozen advertised schema contains both fields but marks them optional/default-null even for `prepare`. Consequently incomplete preparations become journaled controller actions with the same generic refusal instead of an actionable request error.

The model repeatedly stated it was supplying the fields, then retried beyond the two-equivalent-attempt limit. **That prose does not prove serialization dropped them.** Raw provider arguments before tool validation and proxy stdin frames were not retained. The available boundary comparison cannot distinguish model omission from an upstream integration transformation. The coordinator owns the method-specific schema and pre-admission validation repair.

After eleven refused preparations and twenty completed steps, the operator explicitly latched the existing cleanup gate. This assistance is recorded in `operator-stop.json`; the run does not claim autonomous compliance with the retry limit. No transfer reference, native export or native import was established. Final independent passive balances remained 5,000/0 with zero reserved/inflight/pending categories as applicable.

## Accounting and closure

The retained journal contains exactly **23 actions**:

| Scope | Actions | Native receipts |
| --- | --- | --- |
| Operator setup | 6 live executions, 1 restart, 1 liquidity bootstrap, 2 balances | 6 |
| Model | 11 refused private preparations, 2 balances | 0 |
| Operator finalizer in run02 journal | 0 | 0 |

All six setup native receipts show exit zero, complete/untruncated streams and verified cleanup. Custody refusals and typed observations are not native receipts. The operator's cleanup admission latch is separate from the experiment journal.

The model released the lease, closed the experiment, exported a complete journal, closed the lab, observed `verified_absent: true` and returned a final report inside the cap. That report correctly admits missing money flow, but overstates serialization as the established cause and gives an unqualified native receipt count of zero despite six setup receipts. The reviewed scorecard therefore remains **execution failed / target inconclusive / evidence insufficient / proficiency failed**.

The session used **304 seconds, 28 steps, 648,582 processed tokens and 33,889 peak context tokens**. Reported token breakdown: input 36,340; output 4,747; reasoning 3,847; cache read 603,648. The provider reports cost zero; actual billed dollars are unverified. The first setup attempt used no model tokens.

No complete encoded Cashu token matched the retained public-transcript/setup scan pattern. This is a limited scan, and private transport was never exercised; no transport-privacy success is inferred. Private wallet files were not downloaded for review.

## Evidence and next step

Evidence is under `dev/agent-usability-runs/private-ecash-agent-02-20260905/`: `review.json`, `scorecard.json`, `journal-audit.json`, `argument-boundary-audit.json`, advertised tool schema, events/session export, model final report, setup receipts, operator stop, cleanup latch, privacy/cost assessments, harness diff/digests and `operator-cluster-after.json`. The earlier failed setup retains its own receipts, diagnosis, original setup client and finalizer audit in run01.

Frozen pins: controller `e502c83e2570540a9a12ac9792eb102003ceff8d00af60d2cbbd0d3b71ee1d81`; runner `8acb76b8b194d7f679c3f448ca9c05d298e070f1aafc82631f5452a3fd6eb2d0`; MCP `6f56b36f2b65fc17701a102a15a812441d6052966c17a77f7f3c0322dfe65273`.

Next: repair and test method-specific prepare validation, then verify the actual client argument boundary with an unfunded synthetic contract check before another funded agent dispatch. Keep the existing native wallet commands and generic custody surface. This failed run provides no new wallet-defect evidence and does not invalidate the separate deterministic bidirectional pass.
