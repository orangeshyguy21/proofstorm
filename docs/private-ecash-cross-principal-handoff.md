# Cross-principal private ecash handoff

The recipient role can now be bound to another configured principal and a restricted child lease. The existing root lease remains the exclusive lab lease. This extends the private transport protocol; native wallets still own send, receive, proof-state checks and recovery.

The source principal remains a trusted lab owner. The recipient gets access to one reference in one destination wallet, the exact native receive command approved by the source, and a passive balance observation for one mint. The first implementation uses separate MCP processes against the same authority database and lab. The operator configures their identities and workspace capabilities; a principal identifier in a request is never a recipient credential.

## Flow

1. The source reserves custody and completes native private capture using the existing protocol. Wait for a ready capture and terminal native receipt before handing off.
2. The source delegates a recipient lease. The recipient must already have the independently configured capabilities needed for receipt. Delegation does not create a principal or grant global capabilities.
3. The source binds the ready capture to that exact child lease with `private_transfer` method `handoff`. This is a one-time recipient change, before inbox delivery or native import.
4. Pass the recipient the child lease ID as ordinary metadata. It can read its scope with `lease_read`, then call `private_transfer` `deliver` and `component_exec_live` private consume under its own identity, child lease and the shared experiment.
5. Await the native receipt and observe the destination wallet balance under the child lease. Delivery and native exit remain separate from financial evidence.
6. Either the child owner or its root lease owner can release the child lease, revoking new admission. The source retires custody with `private_transfer` `release` after owned work is terminal. The root owner retains experiment/lab finalization responsibility.

Example source delegation:

```json
{
  "parent_lease_id": "source-lab-lease",
  "recipient_principal_id": "receiver-agent",
  "recipient_lease_id": "receive-transfer-one",
  "component": "wallet-b",
  "mint": "mint",
  "reference": "<ready transfer.id>",
  "receive": {
    "argv": ["cdk-cli", "--work-dir", "/wallet/cdk", "--unit", "sat", "--non-interactive", "receive", "--allow-untrusted", "@proofstorm-private-input"],
    "timeout_seconds": 60,
    "input": {"kind": "argv", "index": 8}
  },
  "duration_seconds": 120,
  "max_actions": 8,
  "idempotency_key": "delegate-transfer-one"
}
```

Call `proofstorm_lease_delegate` with this request. Then call `proofstorm_private_transfer`, using the source's ordinary instance/experiment/lease/operation/idempotency scope, with:

```json
{
  "transfer": {
    "transferMethod": "handoff",
    "component": "wallet-a",
    "reference": "<ready transfer.id>",
    "recipientLeaseId": "receive-transfer-one"
  }
}
```

The recipient uses its own configured identity and `lease_id: "receive-transfer-one"`; it does not impersonate the source. Its `lease_read` response contains the immutable `delegation` object: `parent_lease_id`, `component`, `mint`, `reference`, and `receive_command_digest`. The source supplies the approved command template as metadata alongside the lease ID. Its normalized command, input binding and timeout must match the recorded digest exactly; a recipient cannot substitute another command. Custody metadata also records the bound recipient principal/lease and deadline snapshot. These fields describe a binding; fresh authority checks establish whether access remains active.

A suitable recipient capability set for the current MCP surface is `catalog.read`, `component.exec_live`, `wallet.control`, `experiment.read`, `artifact.read`, `lease.release`, and `action.cancel`. The child scope further restricts operation admission even when a broader wallet tool is discoverable. Catalog read is needed to validate the locked CDK/cocod balance adapter; it does not grant another wallet operation. Root lease acquisition, lab lifecycle and configuration permissions need not be granted to recipients. Existing workspace evidence-read permissions remain separate from wallet execution authority.

## Admission and revocation

The store checks the principal, instance, experiment, active child and root leases, exact scope, and both action budgets in an immediate transaction. Allowed child operations are:

- `private_transfer` status/deliver for the exact reference and destination component;
- `component_exec_live` with that component/reference, a consume binding, private output, and the exact source-approved command/input/timeout;
- `wallet_balance` for the exact destination wallet and mint.

Approved command metadata is bounded to 8 KiB and a 120-second native deadline. It uses existing native command/input validation; no wallet-specific parser or command wrapper decides financial behavior.

Other operations, exports, unbound execution, command substitution, target overrides, another reference/wallet/mint and nested delegation are refused before journal admission. Independently required workspace capabilities still apply. A child cannot cancel another principal's native operation or release its parent lease.

Child leases have at most 32 actions and 900 seconds, never exceed the parent's remaining action budget or absolute expiry, and share the parent's total action budget. A parent retains at most eight active and 32 total child leases. The existing per-instance active-operation bound still applies. The root lease's exclusivity is preserved by a partial unique index; the store migrates existing root-only databases transactionally.

Runtime child annotations are updated with resource-version checks. Controller admission checks current lease identity on every new runtime action, including actions with an omitted scope. Child actions additionally require the exact active parent/child authority and immutable scope, including typed balance observations. Revoking a root atomically removes runtime recipient admission; journal child leases inherit the root's release/expiry state. Revoking a child removes only that child's admission. Lease release ownership is checked before any runtime annotation is changed.

In-flight work already admitted may complete after revocation. Accepted native execution handles retain terminal and cleanup receipts, and the input fence is never reset. Revocation is not cancellation, rollback, proof reclamation or a promise that a daemon-side mutation stopped. Historical receipts remain available under existing evidence-read permissions.

Custody rebinds its recipient identity transactionally, preserving capture bytes/hash and every native fence. The old recipient identity loses access. The write transaction rechecks current identities so an optimistic read cannot bypass a concurrent handoff. A different second recipient is refused, and the same binding cannot revive expired/released custody or a revoked child lease.

## Native boundaries and evidence

Tokens stay in the runtime's private capture/inbox/input path. Handoff requests and lease records contain references and authority metadata only. CDK's native receive uses the private argv placeholder, with its existing 65,536-byte reservation limit. Cocod's receive path consumes private stdin and calls its authenticated native HTTP endpoint. No second Cashu SDK, spent-proof ledger or automatic mutation retry is introduced.

The technical single-principal Kimi K2.5 checkpoint passed before this build's live integration. Its report still contained an incorrect CDK input-path description and an overbroad privacy claim; the retained review keeps those failures separate from the observed technical result. See `docs/private-ecash-kimi25-run04-20260906.md`.

Verification for this extension includes store/custody/controller tests, actual MCP schema tests and `private-handoff`, a live gate using separate configured principals. The gate builds on the existing bidirectional baseline, transfers another 200 sats cocod→CDK and 100 back under recipient leases, checks 4,500/500 final balances, and tests revocation before recipient input across a controller restart. The [deterministic handoff checkpoint](private-ecash-cross-principal-checkpoint-20260906.md) passed and retains exact results, image pins, the earlier failed capability gate and testing limits. Separate-model validation is the next checkpoint.
