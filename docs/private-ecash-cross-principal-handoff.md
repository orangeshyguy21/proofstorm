# Cross-principal private ecash handoff

Private access grants authorize one recipient to receive one specific transfer. They are separate from [session tracking](session-tracking-2026-09-06.md). Finishing or overlapping sessions has no effect on permissions.

The source is a trusted lab operator with `lab.operate` and the operation-specific capabilities. A recipient needs `component.exec_live`, `wallet.control`, `experiment.read`, `artifact.read`, and optionally `catalog.read` and `action.cancel`. A recipient without `lab.operate` can submit only operations permitted by an unrevoked private grant. Grant metadata contains no payload bytes or credentials.

1. The source reserves custody and completes native private capture. Await its terminal receipt and ready capture.
2. Issue a grant with `proofstorm_private_access_issue`:

```json
{
  "instance_id": "lab-instance",
  "recipient_principal_id": "receiver-agent",
  "recipient_grant_id": "receive-transfer-one",
  "component": "wallet-b",
  "mint": "mint",
  "reference": "<ready transfer.id>",
  "receive": {
    "argv": ["cdk-cli", "--work-dir", "/wallet/cdk", "--unit", "sat", "--non-interactive", "receive", "--allow-untrusted", "@proofstorm-private-input"],
    "timeout_seconds": 60,
    "input": {"kind": "argv", "index": 8}
  },
  "idempotency_key": "authorize-transfer-one"
}
```

3. The source calls `proofstorm_private_transfer` handoff with the ordinary action envelope and:

```json
{
  "transfer": {
    "transferMethod": "handoff",
    "component": "wallet-a",
    "reference": "<ready transfer.id>",
    "recipientGrantId": "receive-transfer-one"
  }
}
```

4. The recipient reads `proofstorm_private_access_read` with `grant_id`, checks the supplied command against `scope.receive_command_digest`, delivers the exact reference, then consumes it through `component_exec_live` with private output and the approved input binding. Session attribution is automatic. The store resolves matching access by authenticated principal, lab and request; a caller-supplied session cannot confer access.
5. Await the native receipt and observe wallet balance. Delivery and command exit remain distinct from financial evidence.
6. The issuer or recipient explicitly revokes permission with `proofstorm_private_access_revoke` and `grant_id`. The source retires custody with private-transfer `release` once accepted native work is terminal. Repeating issue with an old identity cannot reactivate revoked access.

The scope permits status/delivery for one reference and destination component, the exact receive command/input/timeout with private output, and passive balance for one wallet/mint. Substitution, another reference or wallet, arbitrary native commands and nested authorization are refused. Existing operation capabilities still apply. The runtime checks the current grant against the accepted action snapshot before new delegated work; accepted native completion keeps its separate receipt path.

The runtime registry is bounded by metadata size. Payload retention, maximum transfer size, concurrent-operation capacity and individual command deadlines remain independent controls. There are no session lifetime or action-count budgets.

The prior deterministic live checkpoint tested the predecessor lease-based implementation. This revised grant/session separation is covered by local regressions; it has not been rerun in the live cross-principal gate.
