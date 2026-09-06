# Private ecash foundation: bounded adversarial review

Scope: read-only review of `crates/proofstorm-transfer/src/lib.rs` and `tests.rs`, 2026-09-05. No implementation edits, native wallet work, cluster access or scenarios. These are static interleaving findings, not claims of an executed reproducer. The seven initial tests were read; their passing result was supplied by the coordinator, not rerun in this review.

The implementation was changing during review. The latest inspected snapshot had lib.rs SHA-256 `d490cf6a65234a6f5144d8895bff0c683983fdf82b1a3c1bc58c09e3b25f0473` and tests.rs SHA-256 `69a8667958082913115be97175b79b4d8cd82d7b2ba9cee8b4bcca5b77045f88`. Locations below identify functions and that snapshot; line numbers can move. Late producer completion callbacks and failed-input interruption were already the coordinator's work and are not claimed as discoveries here.

## Findings

### P1 — Capture can drain valid producer output before discovering a stale revision

In `capture`, source receipt metadata is saved first (approximately lines 440–450). The method then acquires an IMMEDIATE transaction, checks admission using the earlier `Transfer`, consumes the supplied `Read` into the blob, and only afterward attempts its final metadata CAS (lines 456–481).

Concrete interleaving:

1. Capture records the successful native receipt at revision R and pauses before acquiring its body transaction.
2. Another connection calls authorized `observe` on that transfer and commits revision R+1. No custody or native execution state has changed.
3. Capture acquires its transaction. `admission` checks close/expiry, but does not reload/compare the current row revision. It drains a non-rewindable producer reader and writes the complete valid token into the transaction.
4. Final `save` uses R and conflicts. The transaction rolls back the body; error handling changes the still-started record to Unknown.

This does not automatically replay the producer, and the caller receives an error rather than fabricated success. It nevertheless can discard the only available token bytes solely because an unrelated metadata observation won a race. A CAS after destructive input is too late for custody.

Required correction: after acquiring the write transaction, reload and validate the current transfer/operation/receipt/phase before the first reader call. Preserve benign observation changes or reject a conflicting state without touching the stream. Once the current row is selected under that lock, commit body and current metadata together. A failed custody operation must not authorize a replacement native export.

Status: sent to the coordinator; this was still present in the snapshot identified above. No post-fix test result is claimed here.

### P1 — Release could bypass its active-receiver guard

The initial `release` read and tested receiver activity outside a transaction, then called `erase`. `erase` acquired an IMMEDIATE transaction, loaded the newest metadata, and deleted both blobs without repeating the guard.

Interleaving: release reads Ready/delivered/no receiver; a second connection commits `begin_receive`; release's erase reads that new receiver but unconditionally deletes its input and frees capacity. The receiver has been admitted, yet its body is removed by a release that should have returned Phase. The same pattern could race producer admission from Reserved. CAS did not prevent it because erase loaded the latest revision itself.

Required correction: authorize and check the release-specific source/receiver guards in the same transaction that removes payloads and capacity. Keep trusted finalizer/expiry erasure semantics distinct from user-authorized release.

Status: coordinator confirmed the defect and changed `erase` to receive an optional grant and recheck the guards under its IMMEDIATE transaction (latest inspected lines 686–705). Static re-read shows the intended fix. A deterministic regression test is still required; the inspected test file had not yet added one.

### Close admission — initial gap addressed during review

Initially only `prepare` inspected the durable closed flag. Existing-transfer capture/receive admission could still succeed after `close` latched closed but before it retired each row. The current `admission(tx, grant, t)` check now inspects closed, lease expiry and retention under the admission write lock, and is called from begin-capture, capture, delivery, begin-receive and consume. This addresses the identified storage admission gap; do not list it as an outstanding defect in a later version without rechecking.

The embedding runtime must still distinguish a previously committed native claim from a new launch, and reconcile already-owned work during close. A foundation admission receipt does not stop a daemon-side wallet operation by itself.

## Meaningful missing tests

Use synthetic readers/writers and separate SQLite connections. No Cashu mint or funded wallet is needed. Deterministic barriers or narrow test hooks are preferable to timing sleeps.

| Test | Required assertion |
| --- | --- |
| Capture versus benign observation | Pause after source receipt persistence; update observations from another connection; resume a one-shot reader. The complete body and both metadata changes survive, or rejection occurs before any bytes are read. Never consume and discard valid bytes merely because revision changed. |
| Release versus receiver/producer admission | Pause release after its initial read, admit the receiver or producer through another connection, then resume. Guarded release refuses and preserves body/capacity. The opposite ordering must refuse admission after release. |
| Concurrent execution/input fences | Competing begin-capture, begin-receive and consume calls must yield exactly one native-launch permit or one writer invocation. Reopen after the successful claim and verify no second permit/input stream is possible. |
| Close and expiry while waiting for a write lock | Claim attempts that began before the wait must recheck current close/lease/retention state inside the transaction, return without invoking a reader/writer or native producer, and retain earlier native receipts. |
| Native operation identity reuse | The new `claim_operation` must reject reuse across transfers and across source/receiver roles, including concurrent claims and reopened vaults. Verify tombstones still fence old operation IDs. |
| Partial write and flush failure | Distinct writers fail after a prefix and at final flush. Preserve input_started/interrupted and the accepted operation ID, refuse another consume, retain available private body and accept a later matching native receipt. Include failures before blob opening once input admission was persisted. This extends the coordinator-owned stream fix. |
| Interrupted source and late callbacks | After reopen, interrupt and expire/close, accept the exact late producer/receiver receipt, reject a conflicting receipt, keep erased custody terminal, and never revive byte access or native admission. This extends the coordinator-owned callback fix. |
| Storage transaction failure | Inject blob allocation/write/commit failures. Prepare failure must leave no launch permit or partially admitted capacity; capture failure must preserve the native operation identity and explicit unknown outcome; failed erase must not report capacity freed while body retirement failed. |

The existing tests cover important sequential behavior: preallocation, same-key deduplication across renewal of the same lease, recipient identity checks, blob integrity, basic restart, oversize/partial input, expiry, and private path permissions. Opening a second connection and making sequential calls does not exercise the interleavings above.

## Boundaries that should remain explicit at wiring

- Grant construction, fresh endpoint lease resolution and callback authenticity belong to the trusted embedding runtime. This review does not treat public Rust constructors as an agent authorization bypass; the crate is not an agent JSON API.
- Payload bodies are separate from serialized Transfer metadata; errors are static. Preserve that separation in logs, journal export, callbacks and any future MCP surface. `observations` must resolve to actual authorized native evidence at the integration layer; accepting an identifier here is not proof of redemption.
- Capacity reservation commits source and inbox blobs before producer admission. That is useful custody preparation, not a guarantee that every later filesystem write or rollback journal allocation can succeed. Test disk/storage failure without pretending native work rolled back.
- `Unknown`, interrupted native stages and released/expired custody must not become “unspent,” “reclaimed” or “redeemed.” Wallet-native reconciliation owns those conclusions.
- Native operation replay fences must remain durable after failures and tombstone cleanup. Delivery idempotency is separate from receiver execution, and successful input streaming is separate from native receipt and wallet redemption.

No additional distinct replay defect was established in the inspected final admission/CAS code. The capture race above remains the principal unresolved custody finding at this review snapshot. Finish its fix and the focused concurrency/failure tests before treating the storage foundation as ready for runtime integration; no payment rerun is needed for these cases.

---

## Follow-up review — fixed custody races and source manifest

Read-only follow-up, 2026-09-05. The earlier snapshot findings above are preserved as historical findings; their original “unresolved” status is superseded by this section. No implementation edits, test execution, cluster access, native mutations or scenario runs were performed. The coordinator reported 15 passing targeted tests plus the ignored subprocess helper invoked by the kill test, and passing targeted Clippy. Additional tests were added during this read; their execution result is not independently claimed here.

Latest inspected file identities: lib.rs SHA-256 `6fe348d901322f86e8f809604ef28ed9772821bdb42a4dce2e78763e68488e27`; tests.rs SHA-256 `1f0c0ad4c41d50e2731b4726f27335f17a7386063383c80dfc99b4d6d6b7eb5b`. References identify function names because active edits move line numbers.

**Both original P1 defects are addressed in the reviewed code.** `capture_staged` acquires IMMEDIATE, reloads the current row, checks the accepted source operation, receipt and immutable source manifest, and replaces its stale metadata snapshot before calling `Read`. Benign observations are retained in the committed Ready record. A conflicting phase/admission returns before consuming the reader. `concurrent_observation_cannot_discard_a_one_shot_producer_stream` stages the old snapshot, updates observations through another connection, captures it and verifies the delivered bytes and retained observation. Although its reader is a Cursor, the method accepts only `dyn Read`, and the corrected path performs no rewind.

`erase(..., Some(grant))` now checks the source grant and active producer/receiver conditions inside the same IMMEDIATE transaction that deletes both bodies and clears capacity. `release_rechecks_the_receiver_fence_inside_the_erase_transaction` explicitly models the stale initial release read followed by another connection's receive admission, then verifies refusal and preservation of the payload rows. The test calls the guarded internal phase directly, which is an appropriate deterministic way to reproduce this interleaving without sleep-based races.

**Source-hop completeness is materially stronger.** A clean native receipt now requires complete, untruncated streams as well as exit 0, no signal/timeout/cancel and verified cleanup. Capture requires a nonempty bounded source length/hash manifest, pins it to the accepted source operation and compares both actual byte count and digest before committing Ready. Tests reject incomplete/truncated native capture before reading input, and reject early EOF and altered bytes without publishing custody. A matching manifest is evidence about the exact captured bytes; it remains the trusted supervisor/source capture's responsibility to attest the correct operation and stream. This foundation does not parse Cashu or establish redemption.

The added tests for competing receivers/consumers, flush failure, failed reservation/erase transactions and unresolved work in the storage-close receipt are meaningful extensions. In particular, failed erase retains the original metadata and capacity; competing consume calls require the loser to receive no bytes; and cleanup distinguishes absent private storage from native operations whose receipts are still missing. These preserve the separation between byte custody, native execution and wallet outcome.

### Remaining validation caveat: the kill test does not yet directly assert blob rollback

The inspected `process_kill_rolls_back_partial_bytes_without_resetting_native_admission` starts a subprocess, waits until a 4,096-byte blob write has occurred inside the transaction, kills and waits for the child, reopens, marks the stage Unknown and checks the native admission fence. This is stronger than merely dropping a connection. However, it only counts payload rows **after** calling release. It does not inspect their contents before deletion, so those assertions would also pass if the partial bytes had persisted and release subsequently removed them.

Before claiming direct proof of interrupted blob rollback, assert immediately after reopening and before interrupt/release that source and inbox blobs equal their preallocated zero state (or the previously recorded baseline digest). Also check the child's signal outcome if the report specifically claims SIGKILL. Keep the existing native receipt/admission assertions. This is a test-evidence gap, not a newly demonstrated transaction or replay defect; it was sent to the coordinator separately.

No distinct new high-priority runtime custody or native-replay defect was established in this bounded follow-up. The two original blockers can be treated as fixed at the source-review level, subject to the coordinator's final test/build results. This conclusion is limited to the runtime-only storage foundation. Trusted grant/callback construction, actual supervisor capture limits, native import binding, lease/finalizer coordination and wallet-owned reconciliation still need their own integration evidence when wired; no MCP or financial readiness claim follows from this review.


## Coordinator resolution and final validation

The final kill test now requires child signal 9 and compares both 100,000-byte payload slots with their original all-zero contents immediately after reopen, before interrupt or release. This closes the follow-up's direct-rollback evidence gap. The assertions passed in the final workspace run. Reviewer findings above remain attributed to their inspected snapshots; this section reports the coordinator's subsequent execution evidence.

Final `cargo test --workspace --all-targets`: 238 passed, zero failed, one ignored subprocess helper that the passing parent SIGKILL test explicitly launches. The foundation contributes 19 passing tests. Strict workspace/all-target Clippy, formatting and diff whitespace checks passed. No Linux supervisor or funded/live-wallet rerun was needed or performed for this unconnected storage crate. Exact final source digests and logs are retained in `dev/wallet-integration-runs/private-ecash-foundation-01-20260905/`.
