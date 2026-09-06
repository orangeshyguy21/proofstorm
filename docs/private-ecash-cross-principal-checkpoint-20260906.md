# Cross-principal native handoff checkpoint

Status: **deterministic gate passed; separate-model campaign pending**. The repaired single-principal Kimi K2.5 run previously held the technical transfer contract, while its report proficiency failed. This checkpoint adds independently configured recipient principals without changing who owns Cashu send, receive or proof recovery.

The source retains the trusted exclusive lab lease. A child lease authorizes one recipient, destination wallet, mint, reference and source-approved native receive command/input/timeout. Store admission shares the parent's action budget; the controller rechecks current authority before new execution. Payload bytes remain in private runtime custody. See the [protocol and trust boundaries](private-ecash-cross-principal-handoff.md).

## Verified behavior

`private-handoff-02-20260906` used separate MCP processes for `experiment-agent`, `recipient-cdk` and `recipient-coco` against one authority database. It first repeated the existing 576,000-byte capture/restart/private-consume check and native 700/300-sat baseline. It then:

- Transferred 200 sats cocod→CDK under the CDK recipient's child lease. Its own passive observation showed 600 sats, with zero pending/reserved/pending-spent.
- Transferred 100 sats CDK→cocod under the cocod recipient's child lease. Its own passive observation showed 4,500 sats, with zero reserved/inflight.
- Refused recipient attempts to release the parent, observe the sender wallet, run an unbound command, substitute an otherwise valid receive command, and observe after revocation. Eleven total refusals were checked by exact error code; scope refusals also required the explicit recipient-scope reason. None entered the complete journal.
- Replaced the controller while a third recipient binding held a 4,096-byte synthetic capture. The new controller preserved its exact reference/hash/recipient. Source revocation then blocked delivery before native input; no recipient operation fence existed, and source retirement succeeded.
- Ran CDK's native `check-pending` privately, then obtained fresh final balances of cocod 4,500 / CDK 500, with all supported unsettled categories zero.
- Retired custody, released authority, closed the experiment/lab and obtained verified namespace absence. An independent cluster read found zero labs/actions and confirmed the namespace absent and controller ready.

The complete journal contains **51 actions**: 22 custody, 19 live-exec requests, one wallet restart, one liquidity bootstrap and eight balances. Eighteen live executions produced terminal native receipts; the remaining live-exec action is the deliberate synthetic replay refusal. All 18 receipts had complete, untruncated streams, verified cleanup and the pinned runner digest. All 11 executions carrying private payload bindings had empty ordinary streams and private-file retirement. Principal counts are source 44, CDK recipient 3 and cocod recipient 4. Counts are derived from retained receipts, not agent estimates.

The audit reproduced both approved-command digests and checked actual recipient identity, lease and input binding: CDK private argv index 8; cocod private stdin. Final observations were admitted after native reconciliation completed. A bounded encoded-token scan of retained JSON found no match; this is not an exhaustive secret scan, and raw private bodies were not fetched for comparison.

## Review and verification

An independent `gpt-5.6-luna` source reviewer found two issues before deployment: a recipient could substitute an arbitrary command while retaining a valid payload binding, and omission of runtime lease scope could bypass delegated validation. Both were fixed, covered by store/controller tests, and re-reviewed with no remaining confirmed bypass. The live gate additionally rejects valid-shaped command substitution. Existing owned executions retain completion/cleanup handling after revocation; new admission is refused.

The workspace suite passed **254 Rust tests**, with zero failures. The ignored subprocess crash fixture is explicitly run by its passing parent test. **Nine Linux runner contracts**, strict workspace Clippy, generated schema/CRD contracts, actual MCP stdio contracts, formatting and diff whitespace checks passed. The final live-gate helper changes added catalog permission and exact refusal assertions; they were rebuilt, checked with strict Clippy and exercised in the passing gate.

Gate01 remains **failed**. Its private CDK receive returned success/cleanup, but the next balance observation lacked `catalog.read`, which the locked CDK/cocod adapter lookup requires. Its sender-balance negative also failed for that missing capability, so it is not evidence of scope enforcement. Gate02 provisions the documented seven-capability recipient set and checks exact refusal reasons. Gate01 finalized normally and was independently verified absent before Gate02; no ambiguous mutation was replayed in that lab.

## Pins and retained evidence

- Controller: `sha256:3398ef33dddef3a3c5f9e1492fab6e4b667f003bf0f985258ea829cbf2f2aa38`.
- Linux runner: `sha256:9368f0dd88aff029369f4af20fd6b67477b347ca89aa09c3d882886bea21006f`.
- Release MCP for the next campaign: `sha256:57bbad343d6d890a6fb23f5414bc55e888e4ba557d6a98af74bd423542c2e4b5`.
- Debug MCP used by this deterministic gate: `sha256:3df6b616c0b0f8c9d43cfccaf2500f10790a4ecf20accb69ee4b5ff563c983c0`.
- Source baseline: `b834ee8e77526e49229ff32f94de1856cb1714a7`; this is an **uncommitted workspace build**.

Evidence: `dev/wallet-integration-runs/private-handoff-02-20260906/`, including complete export, individual receipts, `receipt-audit.json`, `independent-review.json`, source diff, build pins, verification logs, final balances, restart identities, closed receipt and cluster audit. Gate01 retains the controller source-hash snapshot and its original failed outcome/review.

## Next checkpoint and limits

The next bounded campaign uses two independent Kimi K2.5 contexts, one source and one recipient, exchanging validated metadata through a serial coordinator. It will test 70 sats cocod→CDK, exact scope refusals, child revocation and source-owned finalization under one shared 600-second budget. Its offline runner/tests must pass before dispatch; see the [campaign plan](private-ecash-cross-principal-campaign-plan.md).

This is operator-configured principal separation on a shared authority database and local lab. The trusted root still owns the whole lab. It does not establish mutually isolated source wallets, remote identity federation, daemon rollback, reclamation of spent notes, every CSI driver, or live revocation during an already-running native import. Unit/source evidence covers additional race and late-receipt boundaries; those are not claimed as separate live fault experiments.
