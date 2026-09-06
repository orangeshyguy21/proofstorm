# CDK lab plan verification

**Passed: the exact saved Kimi plan provisioned all six components, preserved the
zero-fee setting, and completed verified teardown.** This was operator-driven
verification, not another autonomous-agent benchmark or a wallet payment test.

Source plan: `cdk-plan-k3-01-20260905-v2`.
Plan digest: `sha256:cef394c476adbb47e9af53ffc00c28c21ff8c7f848b59259310e690b591e7601`.
Disposable instance: `cdk-plan-verified-20260905`.

The original evidence database was preserved. Verification used a SQLite backup,
the unchanged saved draft and an explicit expected digest for MCP `lab_apply`.
The verifier had provisioning/lifecycle grants; no benchmark model was launched.

Verified observations:

- Bitcoin Core 30.0, two LND 0.20.0-beta nodes, CDK mint 0.18.0 and two CDK CLI
  0.18.0 wallet components were selected in the saved draft.
- Both LND nodes bind to Bitcoin regtest; the mint binds to LND A for BOLT11/sat;
  the LND peer relationship is declared in the plan.
- The applied draft exactly matches the original saved draft, including the
  mint's authored `input_fee_ppk=0` override.
- `lab_wait` reached `ready` with six of six components. Independent workload
  inspection found running, ready containers.
- The deployed mint ConfigMap contains `input_fee_ppk = 0`.
- Both wallets ran image digest
  `sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8`.
  Each has a distinct claim mounted at `/wallet` and uses `Recreate`.
- Final teardown reports `verified_absent: true`, zero components, and an
  independent idle cluster audit with no blockers.

Infrastructure readiness does not establish funded wallets, mature blocks,
usable Lightning channels, settlement, seed isolation or restart recovery.
Those runtime properties were not exercised. Distinct volume claims establish
storage separation here, not cryptographic identity separation.

Verifier limitations are retained in the evidence: an attempted `lab_read` call
was refused because the native toolset does not expose it. The initial shell did
not stop on that read failure and proceeded to apply. The apply request still
enforced the saved plan digest; the full independent static assertions were then
completed against the copied database and original evidence, with equality verified.
The failed read receipt is preserved. Later command batches used fail-fast shell
handling. The first 30-second close wait expired during ordinary deletion; the
subsequent receipt confirmed absence without forced cleanup.

Artifacts are retained in
`dev/agent-usability-runs/cdk-plan-verification-20260905/`: `static-check.json`,
`apply.json`, `ready.json`, `deployed-check.json`, `closed-final.json`,
`cluster-after.json`, the isolated database and verifier client. No production
code was changed for this checkpoint. The next separately bounded question is
native wallet initialization, funding and payment on this verified topology.
