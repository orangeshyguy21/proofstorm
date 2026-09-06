# Private ecash transport protocol

Status, 2026-09-06: the connected custody/runtime checkpoint passed both native wallet directions, populated controller restart and verified teardown. The [single-principal Kimi K2.5 checkpoint](private-ecash-kimi25-run04-20260906.md) subsequently held the technical transfer contract; its reporting proficiency still failed review. Earlier foundation, runtime and request-contract failures remain in their original checkpoint records.

[Cross-principal handoff](private-ecash-cross-principal-handoff.md) now uses independent private access grants with an approved native receive command. Its [deterministic live gate passed](private-ecash-cross-principal-checkpoint-20260906.md); separate-model validation remains the next checkpoint. The source remains a trusted whole-lab owner, and recipients use independently configured identities against the shared authority database.

Proofstorm owns private byte transport, authorization, execution fences and evidence references. The installed wallet owns Cashu send, receive, proof-state checks and recovery. The [pinned native capability audit](private-ecash-wallet-capabilities.md) records the actual exposed interfaces and their limits. The design does not add another wallet SDK, mint proof-state client or Proofstorm spent-proof ledger.

## Agent contract and custody

The intended agent flow is: reserve a transfer to an authorized peer wallet, run a native producer into private capture, deliver the opaque reference, invoke the native recipient through private input, and inspect separate native and wallet evidence. Token bodies never belong in MCP arguments/results, ordinary stdout/stderr, action specifications, journal exports or model context. An opaque reference identifies custody; it does not grant access.

The current Rust API implements these internal steps:

| Step | Durable result | What it does not establish |
| --- | --- | --- |
| `prepare` | Same-lab endpoint identities, idempotency key, retention deadline and two preallocated payload slots | Native export has started |
| `begin_capture` | Accepted source operation ID committed before native launch | Export succeeded or notes exist |
| `capture` | Successful complete native receipt, source length/hash manifest, exact private bytes verified and committed | Recipient received or redeemed notes |
| `handoff` | One-time binding of ready custody to an authorized recipient principal and private access grant | Native import or financial settlement occurred |
| `deliver` | Integrity-checked private inbox; repeated delivery verifies the same inbox | A receiver process was launched |
| `begin_receive` / `consume` | Unique accepted receiver operation; one private input attempt persisted before writing | Native receive completed or a daemon mutation stopped on client cancellation |
| `finish_source` / `finish_receive` | Immutable matching supervisor receipt, including late completion | Cashu financial success solely from exit 0 |
| `observe` | Bounded references to authorized native evidence | A derived spent/unspent conclusion |
| `release` / `expire` / `close` | Payload retirement and retained metadata fences | Notes expired, were reclaimed, or ceased to be spendable |

The implemented transport path is explicitly `infrastructure_relay`: agents address a peer, while the trusted runtime holds and delivers the bytes. This is not an end-to-end encrypted peer networking protocol. Payloads remain sensitive inside the runtime trust boundary.

`Grant` is deliberately not deserializable. The embedding runtime must construct it from freshly checked workspace capabilities, private access grants, principal identities and the exact same lab. It must validate destination authority rather than trusting agent-supplied fields. Existing accepted native completion callbacks have a separate trusted path so their receipts can survive access revocation without restoring byte access. Evidence references must resolve to actual authorized operations before `observe`; the library cannot authenticate an arbitrary operation string.

## Storage and execution guarantees

Each lab has one runtime-owned vault directory, mode 0700, containing a mode-0600 SQLite database. Insecure existing modes and symlink paths are refused. The vault persists its workspace/lab identity, closed admission latch, endpoint identities, idempotency records, native operation IDs and payload slots. Metadata serializes no token bytes or private paths; diagnostics are fixed strings rather than embedded parser/IO errors.

SQLite uses FULL synchronous rollback-journal transactions and secure deletion. Both source and inbox slots are allocated in one committed transaction before producer admission. Default bounds are 1 MiB per payload, 32 MiB of reserved payload capacity per lab including both copies, eight active transfers and one-hour retention. Configurable hard ceilings are 16 MiB per payload, 256 MiB per lab, 32 active transfers and 24-hour retention. A lab retains at most 4,096 metadata records, each with at most 32 evidence references. Runtime configuration must remain consistent for a vault's lifetime.

Private copy operations use bounded buffers. The source supplies an independently computed length/hash manifest before transport; destination storage is compared against it. A short source stream cannot silently become a successful shorter payload. Malformed manifests, oversize/partial input, unsuccessful native exit and incomplete/truncated native streams withhold usable custody. A successful transport does not interpret or validate a Cashu token.

Immediate write transactions and revision checks fence competing admissions, close, release and delivery. Capture reloads current state under its write lock before the first read, preserving concurrent benign evidence updates without consuming and discarding one-shot producer output. Release rechecks active producer/receiver guards inside the transaction that erases bytes. Native operation identities remain unique across source/receiver roles and retained tombstones.

Native export/import is never automatically retried after ambiguous completion. Restart interruption marks accepted work unresolved and preserves operation IDs. The input fence commits before writing to a consumer, so a partial write or final flush failure cannot authorize another input attempt. A crash between that fence and the first byte is intentionally conservative: receipt/recovery evidence must resolve it. Late native receipts remain attachable after retention or cleanup without reviving payload access.

Storage close first latches admission closed, then retires payloads. Its receipt reports remaining payload bytes, retained metadata records, unresolved native receipt count and `storage_cleanup_verified` separately. Only the embedding supervisor/finalizer can verify owned process cleanup; client termination alone cannot stop a cocod daemon-side mutation. Runtime expiry/close must coordinate previously accepted work. The vault does not provide a process scheduler.

Preallocated logical capacity cannot guarantee all later filesystem writes or rollback-journal allocation will succeed. SQLite secure deletion is not a cryptographic or SSD physical-erasure promise, and this checkpoint does not introduce encryption at rest. The trusted host/storage boundary remains material. Outer runtime IO deadlines and cancellation are required: synchronous arbitrary Rust readers/writers cannot be preempted by this library. SQLite lock waiting is bounded to five seconds.

## Reuse the wallets

CDK 0.18.0 has native `send`, positional `receive TOKEN`, `check-pending` and `burn`. Startup already recovers incomplete sagas; pending checks consult the mint and mutate local wallet records. There is no exposed arbitrary-token spent-check CLI, stdin/token-file import or exact-send operation lookup. Recovery must not be mislabeled a passive observation. Send stdout can contain a startup recovery summary before the token, which the future private producer binding must handle deliberately.

Cocod at `44e5101c` exposes native authenticated `/send/cashu` and `/receive/cashu` HTTP routes. Private JSON request bodies avoid process argument limits. Existing daemon watchers and startup/session recovery already reconcile proofs; `/history` provides sensitive native operation evidence that must be privately correlated and selectively projected. Richer core refresh/reclaim methods are not exposed daemon routes at this pin. HTTP 200 can contain a wallet error, and client cancellation does not establish daemon cancellation.

The first real binding should use CDK native export to cocod native HTTP import. Reverse transfer needs an explicit CDK input bound checked before the source export is admitted. Expanding a file into a positional argument is still bounded argv, not streaming; oversized CDK imports need an upstream native input enhancement with a separately pinned image. The vault's payload limit is not a promise that every wallet can consume that size.

Native proof-state SPENT does not identify which recipient redeemed a note. Correlate native receiver evidence and balances with the original private token/proof identity. Preserve unknown outcomes when the pinned exposed interfaces cannot resolve them. Native reconciliation can be an active wallet mutation and needs ordinary ownership/admission, not a hidden transport retry.

## Checkpoints and remaining build

1. **Custody foundation — implemented.** Synthetic private payloads, restart/interruption, authorization, capacity and replay fencing; adversarial static review with regression fixes. This is the current checkpoint.
2. **Runtime integration — implemented in checkpoint 2.** The shared controller owns per-lab vaults on a PVC and checks the current private access grant before admission. Native capture/input bindings carry bounded bytes through private helper streams and preserve pod/operation identity. The MCP surface below exposes only metadata. Native evidence-reference attachment remains runtime-only; this checkpoint does not add an agent `observe` or proof-recovery wrapper.
3. **Deterministic native transfer — next live gate.** CDK to cocod first, then bounded cocod to CDK. Use a lab mint, exact source/destination evidence and balances, native error envelopes and interrupted/restart cases. Verify source and destination cleanup independently. Large synthetic transport success is not evidence that real Cashu token imports work.
4. **Agent fuzzer laboratory — after the native gate.** Bounded scenario with absolute cleanup/hard deadlines, opaque references only, exact receipt accounting, independent teardown audit and a private-material leakage check. Hand off directly to the existing fuzzer when deterministic runtime evidence supports it. Static review already occurred; no model/funded transport run has been dispatched.

## Validation and evidence

Nineteen focused foundation tests cover a 560,000-byte private fixture; incorrect principals, wallets, labs and access grants; capacity/idempotency; competing reservations, producer and receiver admissions/input; source/inbox tampering and source-manifest mismatch; incomplete native captures; both review races; partial write and flush failure; allocation/erase rollback; late receipts; private path modes; explicit cleanup accounting; and subprocess SIGKILL during an uncommitted capture.

The kill test inspects both preallocated blobs immediately after reopening, before interrupt/release can erase evidence, and requires their original zero state. It also verifies SIGKILL, retained source receipt and refusal of another export. Its ignored child helper is invoked explicitly by the parent test; it is not an untested skipped scenario. Storage trigger failures exercise transactional allocation/erase rollback, not every possible real disk-full or hardware fault.

The [adversarial review](private-ecash-foundation-review.md) preserves its initial findings and later review status. Evidence, exact native help/binary checksum, test logs, source diff and digests are retained under `dev/wallet-integration-runs/private-ecash-foundation-01-20260905/`. No cluster deployment, native money flow or production image changed for this checkpoint.

Final workspace validation: `cargo test --workspace --all-targets` passed 238 tests with zero failures and one explicitly invoked subprocess helper marked ignored in the ordinary test list. `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` and `git diff --check` passed. The tests ran on the development host; this new storage-only checkpoint did not rerun Linux supervisor contracts or live funded scenarios.


## Connected MCP flow

`proofstorm_private_transfer` takes the ordinary instance, experiment, optional session, operation and idempotency scope plus a nested `transfer` object. It is an asynchronous journaled action; read its terminal operation artifact for `transfer.id` and custody metadata.

Reserve capacity before starting an export:

```json
{"transfer":{"transferMethod":"prepare","component":"wallet-a","destinationComponent":"wallet-b","maximumBytes":65536}}
```

The MCP `transfer` schema is method-specific. `prepare` requires all three endpoint/capacity fields shown above; `status`, `deliver` and `release` require `component` and `reference`; `handoff` also requires `recipientGrantId` (see the linked cross-principal flow). Omit fields belonging to other methods instead of sending nulls. Missing, null or unexpected fields fail request decoding immediately. Empty identifiers, equal source/destination and out-of-range capacity fail pre-admission; endpoint validation and the CDK recipient's 65,536-byte limit also run before journal operation creation. The controller retains its existing action format and independently enforces custody authority.

Run the selected wallet's native send command through `component_exec_live`, with private output and this additional binding:

```json
{"private_payload":{"kind":"capture","reference":"<transfer.id>","format":"cashu_token"}}
```

`cashu_token` selects exactly one token-shaped `cashuA` or `cashuB` line, permitting native startup diagnostics on other lines. Ambiguous output fails closed; it does not parse proofs or infer a mint/amount. `bytes` is the generic whole-stdout format for synthetic or other private payloads. The source manifest covers the selected payload; the ordinary private-output manifest separately describes the original stdout including whitespace/diagnostics.

Deliver the reference to the configured wallet inbox:

```json
{"transfer":{"transferMethod":"deliver","component":"wallet-b","reference":"<transfer.id>"}}
```

Run native receive with `private_payload.kind=consume`. `input:{"kind":"stdin"}` feeds the verified body directly to the native process's stdin. Cocod's native authenticated HTTP route can consume JSON built privately from this stdin. A shell heredoc uses stdin for the program itself; use a direct `python3 -c`/equivalent command when the same stdin carries the token.

CDK's pinned receive CLI uses `input:{"kind":"argv","index":8}` for an argv layout such as `cdk-cli --work-dir /wallet/cdk --unit sat --non-interactive receive --allow-untrusted @proofstorm-private-input`. The runtime replaces only the exact placeholder at the specified nonzero index. The trust flag is an explicit wallet-native choice, used in the deterministic lab for the known lab mint; it is not silently added by transport. Tokens never appear in the public command request or persisted action. They do appear transiently in the recipient process's private argv, which remains an OS argument-size/trust boundary.

Status and release use the same metadata action with `transferMethod` set to `status` or `release`. Release requires the original source role and refuses active capture/receive. The reference stays tied to the same workspace, lab, principal, wallets and lease. Lease renewal with the same identity is distinct from transferring ownership to another lease/principal; the latter is not exposed.

## Runtime persistence, retries and limits

The controller stores vaults under `PROOFSTORM_PRIVATE_ROOT`, default `/var/lib/proofstorm/private`. The chart mounts a 1 GiB PVC at `/var/lib/proofstorm`, uses one controller with Recreate, and `fsGroupChangePolicy: OnRootMismatch` to preserve strict inner 0700/0600 modes on supporting volume drivers. The initial upgrade from an existing RollingUpdate deployment needs an explicit removal of `spec.strategy.rollingUpdate` when changing strategy; the local deployment required a merge-patch migration before Helm's server-side apply succeeded. Normal fresh installs use the chart's Recreate strategy directly. Different storage drivers need their own remount validation.

Lease acquisition mirrors the owned lease into a CAS-protected lab annotation. A conflicting current private access grant is refused. Release removes that runtime admission before marking the local access revocationd. Controller admission re-fetches the lab and checks exact lease phase and identity. Agent request JSON never supplies a trusted Grant or a token body.

The first connected runtime allows up to 1 MiB of selected payload. It rejects reservations above 64 KiB when CDK is the destination, before native export. Source stdout retention permits the reservation plus 16 KiB for startup diagnostics; stderr retains 16 KiB. All ordinary output remains private for both capture and consume, including unsuccessful operations. File/helper reads are bounded independently of stored size metadata. Helper network operations have a 20-second deadline, native commands retain their existing 1–300 second deadline, and SQLite waits remain bounded to five seconds.

A transient completion/retirement failure keeps the original native handle and terminal native receipt in a Running action. Reconciliation retries custody only, up to the original start plus command timeout plus 60 seconds. Once known, native completion is read from that persisted receipt rather than depending on another status call. A changed pod cannot turn a known exit back into unknown execution or execute a helper in the replacement pod. Exhaustion fails the action while preserving known native facts. Existing handles reconcile owned work without reapplying new-command readiness admission.

Successful capture retires the source runner's private stdout/stderr/payload; successful consumption retires recipient input/captures. Receipts report `private_files_retired` separately. Vault expiry is swept during ordinary lab reconciliation, normally within the next roughly 40-second pass while the controller is healthy; access expiry is checked at admission. Failed/uncollected runner captures may remain in the original component's temporary filesystem until component/lab teardown. Finalization closes vault admission, retires its bodies, deletes lab resources and removes the closed vault only after namespace absence. The shared controller PVC remains part of the idle infrastructure.

Tool discovery adds one generic custody tool plus native binding schemas. The explicit budgets increase by 8 KiB for the full/experiment/native/runtime surfaces; design and evidence profiles retain their prior budgets. This is bounded protocol metadata, independent of token size. No wallet-specific send/receive tools or second Cashu implementation were introduced.
