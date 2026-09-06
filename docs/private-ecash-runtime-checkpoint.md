# Private ecash runtime checkpoint

Status 2026-09-05: deterministic checkpoint **passed**. Controller, native runner, persistent custody and MCP are connected. Agent execution validation is the next handoff. Transfers currently share the existing exclusive whole-lab lease; cross-principal lease handoff remains a separate extension.

Subsequent extension: [cross-principal recipient handoff](private-ecash-cross-principal-handoff.md) is implemented; this document retains the narrower 2026-09-05 runtime checkpoint and its original pins.

## What passed

`private-transfer-03-20260905` used the pinned cocod, CDK CLI, CDK mint, Bitcoin and LND lab:

- Captured 576,000 private synthetic bytes from the CDK component, with empty ordinary streams and source-file retirement.
- Replaced the controller pod with populated custody on its PVC. The new controller reopened the vault with its strict 0700/0600 checks, preserved the exact reference/hash, delivered and consumed the bytes, and released custody. Pod UIDs and receipts are retained. This verifies the local storage driver's remount behavior; it is not a result for every CSI driver.
- Refused a second private consumer before native execution. That failed action has no native receipt.
- Funded cocod with 5,000 lab sats through the already-validated native structured invoice path.
- Exported 700 sats from cocod into private capture and imported them with the pinned CDK CLI's bounded private argv binding. CDK's independent passive observation reported 700 sats.
- Exported 300 sats from CDK into private capture and imported them through cocod's authenticated native HTTP route, with the body supplied by private stdin.
- Invoked CDK's own `check-pending`; final passive observations reported CDK 400 sats, zero pending/reserved/pending-spent, and cocod 4,600 sats, zero reserved/inflight. The 5,000-sat ready total was conserved.
- Retired both real transfer payloads, closed the experiment and lab, and received verified namespace absence with zero residual lab inventory.

There were **29 journaled actions**: 10 custody actions, 13 live-exec requests (12 terminal supervisor receipts and one pre-execution replay refusal), one wallet-component restart, one liquidity bootstrap and four wallet balance observations. Every accepted native execution had complete, untruncated streams and verified cleanup. The controller replacement is a separate harness operation, not a model/native wallet action. Counts come from the retained operation table, not a prose estimate.

Retained public JSON contains no complete encoded Cashu token matching the documented scan pattern. Manifest hashes, empty ordinary streams and retirement receipts support the transport privacy property. This is not an exhaustive scan for every possible secret representation, and raw tokens were not fetched into model context for comparison.

## Pins and reproducibility

- Controller: `sha256:e502c83e2570540a9a12ac9792eb102003ceff8d00af60d2cbbd0d3b71ee1d81`, deployed by immutable digest.
- Native runner: `sha256:8acb76b8b194d7f679c3f448ca9c05d298e070f1aafc82631f5452a3fd6eb2d0`. The independently tested Linux binary has this same checksum.
- Release MCP: `sha256:6f56b36f2b65fc17701a102a15a812441d6052966c17a77f7f3c0322dfe65273`.
- Source baseline: `b834ee8e77526e49229ff32f94de1856cb1714a7`, with the current uncommitted foundation/runtime diff.

Evidence is in `dev/wallet-integration-runs/private-transfer-03-20260905/`: individual operation artifacts, controller replacement identities, evidence export, exact receipt audit, scan scope, final balances, closed receipt, cluster audit, build logs, source diff and digests.

Verification: **242 Rust tests passed**, zero failed; the one ignored subprocess fixture is explicitly executed by its passing parent crash test. **Nine Linux supervisor contracts passed**. Strict workspace Clippy, Linux-only runner Clippy, formatting, generated CRD/schema checks and diff whitespace checks passed. The runtime grant test rejects missing, released, expired and mismatched leases; the collection tests preserve the accepted source operation across retry/reopen and retain known native receipts when status polling is unavailable.

## Earlier bounded failures retained

`private-transfer-01-20260905` failed a test assertion that expected the wrong synthetic length. Actual 576,000-byte capture, integrity and retirement succeeded; no funding occurred. Normal teardown completed.

`private-transfer-02-20260905` passed the populated controller replacement and cocod-to-CDK native transfer. It failed afterward because a shared test helper expected cocod-only balance fields from the CDK observation. CDK actually reported the expected 700 sats with zero pending/reserved. The test now checks the correct native observation categories. Normal teardown completed. Neither earlier failed score was rewritten as passed.

The bounded [runtime review](private-ecash-runtime-review.md) found and confirmed fixes for transient custody terminalization, loss of a known receipt during a subsequent status outage, and recursive fsGroup mode broadening on remount. Current custody retries retain the original native handle/receipt and never reach a new launch. OnRootMismatch preserves inner private modes in the tested deployment. Controller fault injection of a live transient collection/persistence outage remains a useful next lab; the current retry regressions are source/unit evidence, not a claimed live outage test.

## Remaining scope

The [protocol guide](private-ecash-transfer-protocol.md) describes the metadata-only tool and native bindings. Wallets retain responsibility for proof-state checks and recovery; no new NUT-07 client or wallet SDK was introduced. Delivery, successful native exit, native spent evidence and recipient balances remain separate facts.

The first connected contract does not transfer a reference to another principal or lease, provide arbitrary-token spent queries absent from the pinned native interfaces, or support oversized CDK argv. Expiry and storage deletion do not expire or reclaim ecash. Failed/uncollected runner captures can remain private until component/lab teardown. The fuzzer should test usability and fault handling within these explicit limits before expanding the protocol.
