# Private ecash MCP contract checkpoint

The failed agent run exposed a public request-contract defect: `prepare` needed `destinationComponent` and `maximumBytes`, but discovery advertised those fields as optional/default-null. Incomplete requests became controller operations with a generic refusal. The method-specific MCP schema now requires both fields and rejects incomplete requests before admission. The original [failed agent checkpoint](private-ecash-agent-checkpoint-2026-09-05.md) remains failed; no further funded or model session was run for this repair.

## Repair

`PrivateTransferInput` is a discriminated public input with four methods. Prepare requires source, destination and capacity; status/deliver/release require component and reference. Null, missing, unknown and method-inapplicable fields fail decoding. Static checks reject empty identifiers, self-delivery and capacities outside 1–1,048,576 bytes. Revision checks reject unknown/non-wallet endpoints and CDK recipient capacities above its 65,536-byte native argv limit before operation creation.

The valid request converts to the existing Kubernetes action format; the controller still independently checks authority, capacity and custody state. Existing native send/receive, proof-state reconciliation, execution fences and transfer storage are unchanged. No wallet wrapper or Cashu implementation was added.

The real rmcp server reports deserialization errors as `result.isError: true` with textual content, for example `failed to deserialize parameters: missing field destinationComponent` (field name backticks omitted here). This is not a JSON-RPC `-32602` envelope. Callers must accept both RPC errors and textual tool errors instead of trying to parse all error text as JSON.

## Verification

- `cargo test -p proofstorm-mcp`: **57 unit + 4 stdio tests passed**. The stdio regression checks the actual advertised `$ref`/`$defs`/`oneOf` schema, omitted/null/wrong-method fields, immediate size refusal, and a complete synthetic request reaching the missing-instance boundary. The store contains no operation for these requests.
- Unit checks cover valid conversion for all four methods, native capacity boundaries, endpoint identity/kind, and same-wallet refusal. Existing capability and discovery-budget checks pass.
- Strict all-target MCP Clippy and formatting/diff checks pass.
- The rebuilt release MCP was started without Kubernetes using an isolated temporary database. Its actual stdio response immediately rejected the observed incomplete prepare. No wallet command ran.

Evidence: `dev/agent-usability-runs/private-ecash-contract-20260905/`, including test/lint logs, the advertised schema and release stdio receipt. Release MCP SHA-256: `32375df6ea1dc3b7dd0f57857aa966554c03a748a7930d262f57b90c8b691624`. Controller and runner remain at the [deterministic runtime checkpoint](private-ecash-runtime-checkpoint.md) pins; this repair does not require redeploying them. Changes remain uncommitted.

## Installed client check

The fuzzer separately [replayed the exact adapter functions extracted from installed OpenCode 1.18.28](private-ecash-opencode-offline-contract-20260905.md). The new nested `$defs`/`oneOf` schema survived both K3 and kimi-ID provider transformations and Anthropic tool preparation. A supplied complete request reached actual debug MCP stdio intact and returned the expected missing-instance error. Missing-field requests returned immediate tool errors. The local database audit found zero instances, actions or operations.

This was offline function replay, not a full OpenCode process, plugin or provider-inference run. A complete supplied request also survived the old schema path. Accordingly the replay does not establish where the failed model run's omissions originated. Evidence is separate under `dev/agent-usability-runs/private-ecash-opencode-offline-20260905/`; no failed score was changed.

## Scope and next checkpoint

The separate deterministic runtime gate already passed large private capture, persistent controller restart, cocod→CDK and CDK→cocod native transfers and verified teardown. The model checkpoint exercised none of those transfer operations. Its recorded omissions do not prove serialization stripped originally complete arguments; raw provider arguments were not retained.

A new funded agent checkpoint is still required to establish agent usability after this repair. Keep it bounded with the existing retry/cleanup limits and retain argument evidence at the relevant boundaries. Cross-principal/lease ownership transfer remains a later build: current custody works under the same principal and exclusive whole-lab lease.
