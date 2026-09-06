# Offline cross-principal fuzzer campaign

> Superseded coordination design: leases are replaced by nonblocking [session tracking](session-tracking-2026-09-06.md); private-transfer permissions are separate. Historical results below describe their recorded version.


Status: **serial runner and offline validation completed; exactly one campaign authorized after gate02 and cluster handback**. The dedicated runner checks passing deterministic evidence and immutable pins, then consumes a one-campaign authorization record. Generated proposal configs remain disabled; default runner invocation does not launch. The existing single-session benchmark runner is not used for this campaign.

The first campaign transfers **70 sats cocod→CDK once**, from assisted initial balances 5,000/0 to 4,930/70. Deterministic coverage owns the reverse direction, controller restart and revocation before input. Run04's technical result stands, and its report-proficiency failure remains retained. No funded replay is planned merely to fix that report.

## Identities and assistance

Use the existing `openrouter/moonshotai/kimi-k2.5` provider. Create exactly two independent OpenCode model contexts, source and recipient. Resume each only for its own later stages; run one model invocation at a time. Do not fork histories, share one session ID or paste one role's report into the other role's prompt.

Both MCP processes use the same **canonical absolute authority SQLite path** and workspace, with different operator-configured `PROOFSTORM_PRINCIPAL` values: `benchmark-source` and `benchmark-recipient`. The source keeps the existing trusted lab-owner capabilities. The recipient receives exactly `catalog.read,component.exec_live,wallet.control,experiment.read,artifact.read,lease.release,action.cancel`, with no root lease acquisition or lab lifecycle grants. The coordinator confirmed `catalog.read` is needed to select the locked passive observation adapter; it does not widen child execution scope. Disable host shell, file, task and network tools as in the existing benchmark profile. This tests MCP principal separation under operator-controlled configuration; it is not OS isolation against someone able to change that configuration.

After pins and cluster handoff, initialize both configured MCP identities before delegation, so the recipient's independent grants actually exist. MCP startup performs principal/grant provisioning; naming a recipient in a request does not. Keep recipient grants unchanged across reconnects. The seeder's temporary capability profile must be replaced by source startup before setup. Reuse the verified exact topology and private protected setup/prefunding fixture under the source identity. The setup handoff now derives its principal from configuration instead of hard-coding `benchmark-agent`.

The source owns the exclusive root lease created by setup. Delegate one child for `wallet-b`, mint `mint`, and the exact ready reference. Neither lease has a time limit or action quota; explicitly release authority at cleanup. The approved native contract is CDK receive with private **argv index 8**, 60-second timeout, and private output. It is explicitly supplied as tested assistance and approved by the source through `lease_delegate`. The child may not substitute command, binding, timeout, target or reference. The source remains a trusted owner of the entire lab; do not claim mutual sender-wallet isolation.

## Fixed budget

The shared model campaign clock starts after successful assisted setup. It includes model startup, role changes and metadata coordination. It has an absolute **600-second / 50-completed-step cap**, **100,000 peak context tokens per context** and **3,000,000 processed tokens summed across both contexts**, including cache reads. Reconnects and stage continuations do not reset any total. Stop after two equivalent failures without a changed hypothesis; never retry an ambiguous accepted mutation.

| Serial stage | Context | Maximum seconds | Maximum new steps |
| --- | --- | ---: | ---: |
| Prepare, capture, delegate and handoff | Source | 150 | 12 |
| Scope refusals, native receive and observation | Recipient | 180 | 16 |
| Verify receipt and revoke child | Source | 60 | 5 |
| Verify revoked admission and report | Recipient | 60 | 5 |
| Final observations, cleanup and report | Source | 150 | 12 |

Stage ceilings are additional stops, not extensions to the campaign cap. Unused stage steps are not borrowed to repeat experiments. Begin source finalization by **450 seconds or 38 completed steps**. A shared disk latch prohibits new work at 480 seconds, 40 steps, 80,000 peak context tokens or 2,400,000 aggregate processed tokens, whichever arrives first. Both proxies must observe one append-only aggregate event stream and the same latch. Once latched, skip unperformed negative cases and report them missing. Reserve at least the final 30 seconds for reporting when bounding observation waits.

The cleanup proxy now permits only `private_transfer` **status/release** in addition to its existing cleanup tools. Prepare, deliver, handoff, delegation and native execution remain work and are refused after the latch. MCP still checks role ownership; this exception does not grant a child custody-release authority. Host fake-server tests verify which calls reach MCP. A campaign watchdog must enforce stage and aggregate hard caps independently of either model.

## Stage instructions and evidence

1. **Source prepare:** consume the disclosed setup receipts, reserve 65,536-byte custody for 70 sats from wallet-a to wallet-b, and capture with the native cocod send command privately. Observe terminal native success, cleanup and ready custody before delegation. Approve the exact receive contract, delegate to the configured recipient, then hand off the ready reference to that child. Return only operation IDs and handoff metadata. Do not deliver or import on behalf of the recipient.
2. **Recipient receive:** read the child lease and check principal, reference, wallet/mint, active scope and command digest. Once each, attempt (a) parent-lease release, (b) passive balance for source wallet-a, and (c) consume using the approved argv with only argv[0] changed to `unapproved-receive`, retaining the otherwise valid binding/timeout. Each must be an authorization/scope refusal, not merely a malformed-argument rejection, and must create no action. If any is admitted, stop and report the returned handle; do not continue importing. Then deliver the exact reference and consume using the approved CDK argv index 8. Await the actual native terminal receipt and independently observe wallet-b balance 70 with reserved/pending/pending-spent zero. Return only operation IDs and observed safe result fields.
3. **Source revoke:** read the recipient's actual operation receipts, check that its native work is terminal and that the root lease survived the forbidden release. Release the child and retain the release receipt. Do not replay or attempt a second import. Notify the recipient only after release is observed.
4. **Recipient revoked:** once, attempt a fresh wallet-b balance observation using the revoked child. Require an authority refusal before journal admission. Report only its own observed scope, refusal outcomes, native receipt and balance IDs. It must not claim source teardown, setup funding or privacy guarantees it did not observe. Historical receipts remain readable under the existing evidence grants; that is not continued mutation authority.
5. **Source finalize:** after recipient native completion, request fresh passive observations of both wallets under the root. Require 4,930/70 and zero supported unsettled categories. Retire custody while the root remains active, release any remaining child, release root, close experiment, export evidence, close lab and observe verified absence. Return the observation IDs, release/closed receipts and missing criteria. The operator independently audits cluster idleness and computes totals and cost.

Successful native exit, transport delivery and financial observations remain separate claims. This first direction does not need CDK `check-pending`, since CDK only receives; unexpected unsettled state is reported instead of adding an improvised repair campaign. No arbitrary help command is available under the child scope; the source supplies its exact approved receive contract. Any safe help is source-owned and counts against the stage budget.

## Metadata coordination and failure ownership

The host scheduler passes only validated lab/experiment/lease IDs, wallet/mint labels, opaque reference, the fixed approved command template and native/observation operation IDs. It obtains milestones from durable receipts rather than believing a model's prose. Native token bytes stay exclusively in Proofstorm private custody and input handling. No private output download, shared transcript, ecash note or credential enters the coordination packet. The offline helper's packet builder accepts only a validated opaque reference and constructs the remaining values; it does not itself verify custody or implement live coordination.

The operator records request principal, exact pins, command digest, admission/error category, action absence for refusals, terminal native receipts, supported balance categories and teardown. Preserve raw agent reports separately. Describe CDK's input as private argv injection, not stdin; restrict privacy conclusions to the actual evidence/scan scope. Aggregate metrics come from the harness, not either role's narrative. Parent-release refusal also requires unchanged root authority, not only a tool error.

Any source/recipient stage timeout, two equivalent failures, unexpected admission or uncertain mutation skips further money work and switches to source-owned cancellation/observation and finalization. Cancel only owned operations; revocation does not cancel admitted work or reverse wallet mutations. If the source cannot complete cleanup inside the hard cap, stop both models, perform separately recorded operator cleanup of this campaign's resources, mark agent cleanup/reporting incomplete and do not launch a replacement session. Assisted setup has its own existing bounded failure cleanup before model dispatch.

## Prepared artifacts and remaining dispatch work

`scripts/prepare-private-handoff-campaign.py` writes a proposal and two disabled role configs without launching processes or creating an authority database. It refuses to overwrite an existing evidence directory. Its tests cover shared canonical authority, distinct identities, restricted recipient grants, fixed metadata-only receive packets, budget arithmetic and offline-only output.

`scripts/run-private-handoff-campaign.py` now implements five serial stages, two resumed session IDs, immutable gate/pin prerequisites, per-stage and aggregate watchdogs, shared proxy budget/latch, durable-receipt coordination, source cleanup priority and a mandatory independent finalizer. An unknown existing session cannot create a replacement context. Fake-process tests cover stage timeouts, cumulative token limits, role/session collision, repeated failures, cleanup transition and finalizer failures. Scope refusals must match the intended diagnostic; missing CatalogRead cannot pass. The native verifier checks the actual runner pin and CDK argv index8. Campaign elapsed time is retained separately from finalizer duration; reports require subsequent manual review. Source and harness copies/hashes are retained by dispatch.

The coordinator supplied passing gate02 evidence and explicit cluster handback. The new entry requires `--dispatch-approved-campaign --run-id <fresh-id>`, verifies the exact gate02 pins and live idle controller, and consumes `dev/agent-usability-runs/private-handoff-authorization-20260906.json` using exclusive creation. It cannot dispatch another campaign using this authorization. The explicit dispatch runs assisted setup before the shared model clock. No reverse transfer, provider fallback, extra model context or automatic live repetition is authorized.

## Dispatch review block

Automatic approval review rejected the same launch command three times before process execution. The final rejection did not accept the specific user instruction relayed by the coordinator as trusted authorization for the transfer and OpenRouter destination. No model, setup, lab or campaign run directory was created; the one-campaign authorization marker remains unconsumed. Offline validation remains passed (42 tests). Further retries are stopped pending direct user confirmation. Review evidence is retained under `dev/agent-usability-runs/private-handoff-dispatch-review-20260906/`.
