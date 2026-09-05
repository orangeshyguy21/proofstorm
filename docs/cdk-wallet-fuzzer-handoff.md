# CDK CLI wallet: first fuzzer checkpoint

Historical checkpoint. The first CDK fuzzer runs have since completed; see the
[preplanned smoke report](cdk-wallet-preplanned-smoke-2026-09-05.md) and the
[reliable native execution checkpoint](reliable-native-execution.md) for the
current execution contract and next handoff. The observations below describe
the original CDK build before supervised execution was added.

Status: **ready for the first CDK fuzzer handoff; build paused before dispatch**.
The CDK gate and corrected Nutshell quote-composition regression passed. No
fuzzer run has been launched. This checkpoint covers phase 1 of the
[wallet expansion architecture](wallet-expansion-architecture.md).

## Implemented surface

- `wallet` implementation `cdk-cli-wallet`, version `0.18.0`, configuration
  `cdk-cli-wallet/0.18/v1`. Empty authorable configuration; persistent, isolated
  `/wallet` volume per component. Workload replacement uses `Recreate`.
- Digest-pinned release-binary image, source commit, artifact checksum, base
  runtime image and recipe digest in catalog and resolved locks. This first
  image is **Linux arm64 in the existing local registry**. It is not a published
  multi-platform distribution; fresh clusters need the image provisioned.
- Existing native execution, evidence, restart and teardown infrastructure.
  No new controller or wallet mutation API was introduced.
- A version-selected passive balance adapter. It uses a SQLite read transaction
  and never starts CDK or contacts a mint. The mounted directory permits SQLite
  WAL coordination files; the database connection uses `mode=ro` and
  `query_only=ON`. Missing/incompatible/busy databases fail instead of returning
  a fabricated zero balance.
- Balances are scoped to the selected mint URL and `sat`. Available, reserved,
  pending and pending-spent amounts are distinct wallet-local classifications.
  They are not a mint-side proof-state oracle.
- Unsupported typed mutations/quote/oracle operations are refused before an
  action record is created. Existing Nutshell behavior is preserved.

Source pin: CDK `d3dec24c784e8fec1fd65f853241c7a2261c7abd`.
Image: `proofstorm-registry.localhost:5000/cdk-cli-wallet@sha256:bc4ec6943eb505bb7eb5a6d43ddebf0297fe00f70775378e33ae85c26eb6a5a8`.
Packaging: `docker/wallet/Dockerfile.kube-cdk` and
`docker/wallet/cdk-cli-0.18.0-provenance.json`.
The validated pairing is CDK CLI 0.18.0 against CDK mint 0.18.0 on LND
0.20.0-beta, SQLite storage and unauthenticated BOLT11/sat.

## Deterministic evidence

The CDK gate passed with exit 0 in run `cdk-wallet-checkpoint6-20260904`.
Local retained evidence is under
`dev/wallet-integration-runs/cdk-wallet-checkpoint6-20260904/` (ignored by Git).
The checked-in runner is `crates/proofstorm-acceptance/src/gates/cdk_wallet.rs`;
run it with `make e2e-cdk-wallet` on an idle local cluster. This local-image gate
is deliberately excluded from the default `make e2e` suite until distribution
is available.

| Observation | Verified result |
| --- | --- |
| Funding | LND `SUCCEEDED`; native and passive balance 5,000 sats |
| First payment | Native `PAID`, 700 sats, zero fees; recipient settled; 4,300 sats remain |
| Restart | Replacement pod, unchanged seed fingerprint, 4,300 sats remain |
| Second payment | Native `PAID`, 300 sats, zero fees; recipient settled; 4,000 sats remain |
| Isolation | Different seed fingerprints; wallet B stayed at zero |
| Process cleanup | No remaining `cdk-cli` process |
| Teardown | `closed.json` records `verified_absent: true` and zero inventory |

The workspace suite passed 208 tests. Strict workspace Clippy, the final
acceptance-crate Clippy check, formatting, schema/coverage/golden contract tests,
Helm lint and 14 agent-usability evaluator tests passed.

The older `cross-implementation-wallet` regression is blocked by a pre-existing
test mismatch: it supplies a `wallet_round_trip` as the conservation treatment,
but the current oracle requires `wallet_pay`. The rejection is also present in
the original `HEAD` code and was not introduced by this wallet change. Its
disposable lab was retired through the controller finalizer; the audit in
`regression-failure-cleanup.json` verifies cluster idleness. This gate has not
passed and needs separate maintenance; the oracle was not weakened to admit an
invalid conservation comparison. The current `quote-composition` regression
passed with exit 0 in run `cdk-wallet-regression2-20260904`, including typed pay
of a native-created quote, single-flight admission, external Lightning payment,
explicit claim, non-disclosure checks and verified teardown. Its first attempt exposed
another stale fixture value: the explicit quote-claim timeout was 60 seconds,
above the API's existing 30-second cap. This build corrects that fixture to 30;
the production limit is unchanged. The failed fixture's finalizer cleanup is
recorded in `quote-fixture-cleanup.json`.

`quote-composition-regression.log` retains the passing regression result. The
release MCP doctor passed, and `handoff-cluster-audit.json` records
`verified_idle: true`, no remaining lab resources and the deployed controller
image. Test/Clippy/Helm logs are retained alongside the operation evidence.

The fixture explicitly sets `input_fee_ppk=0` and uses a direct Lightning
channel. It requires real 5,000-sat funding, native payments of 700 and 300 sats,
native payment receipts with zero fees, independent recipient settlement,
passive balances, separate seed fingerprints/volumes, restart persistence,
absence of remaining CDK child processes and verified teardown. No proofs are
fabricated or inserted into a wallet database.

The operator gate relays BOLT11 requests through private process stdin to keep
them out of command/evidence output. This is fixture plumbing, not the proposed
ecash payload exchange. The agent scenario must use only the authorized
Proofstorm surfaces; small BOLT11 requests can appear there during this phase.

## What the live attempts taught us

1. A read-only volume mount cannot reliably open a WAL database whose sidecar
   files are absent. Keep the SQLite connection read-only while permitting its
   directory-level coordination files. The live-reader fixture checks both
   uncommitted/committed WAL behavior and database contents remaining unchanged.
2. Native LND credentials are under `/home/lnd/.lnd`. `/lnd` is an orchestration
   Job mount, not the live node's data directory. `lookupinvoice --rhash` takes
   hex; native `lncli addinvoice` also emits hex, unlike REST/gRPC JSON byte
   fields. Do not base64-decode the CLI result.
3. `mint-pending` in this pinned CDK release checks pending proofs despite its
   help text. Resume a paid quote using `mint <url> --quote-id <id>`; command
   success alone is insufficient evidence of issuance.
4. The default mint input fee is 100/1000 sat per input proof, rounded for each
   operation. Melt preparation may also perform a swap. A default-fee attempt
   debited 702 sats for a 700-sat payment, correctly failing a zero-fee assertion.
   The first fixture now explicitly selects zero fees. Nonzero-fee accounting
   has not passed a complete deterministic gate and needs a separate case.

Use `--work-dir /wallet/cdk --unit sat --non-interactive` on every native CDK
invocation. `balance` initializes state but can also recover incomplete sagas;
use the typed observation when a passive read matters. Bound long commands
inside the container, retain exit codes, and do not retry ambiguous mutations.
The native timeout wrapper used by the gate is `timeout -k 2 45`.

## Partner fuzzer instructions

The scenario `cdk-wallet-native-smoke` is registered in
`scripts/agent-usability-scenarios.json`. Its prompt has been previewed; no agent
run has been launched at this checkpoint.

First inspect cluster availability and run the smoke scenario alone. From the
repository root, the existing runner command is:

```sh
bash scripts/run-agent-usability-benchmark.sh \
  --scenario cdk-wallet-native-smoke \
  --run-id cdk-wallet-smoke-01 \
  --max-seconds 900 --max-steps 60 --max-equivalent-plans 2
```

Use a fresh run ID. The runner retains its configured default model unless the
operator selects one. This command is supplied for the handoff, not executed by
the build task.

The first agent laboratory should establish an explicit zero-fee baseline,
discover native help and supported controls, fund/pay through real Lightning,
compare passive/native evidence, verify the other wallet stays empty, restart,
pay again and clean up. Reserve the final 20% of the run for evidence and
cleanup. Stop after two equivalent failures without a changed hypothesis.

Report discovery friction separately from wallet defects and harness failures.
For each failure retain the operation IDs, exact pins, relevant sanitized
native exit/output, balance/settlement evidence and teardown receipt. Native
payments do not produce typed `paid_melts` counts: the evaluator deliberately
uses manual settlement/fee review rather than pretending those counts prove
success. Seed/proof material and payment preimages must stay out of outputs.

Only after the baseline passes, propose bounded cases for nonzero input fees,
CLI interruption and recovery, concurrency and alternate mint combinations.
Do not infer recovery safety from a normal restart test or expand directly to
an unbounded fault campaign.

## Next build checkpoints

1. Review the short CDK fuzzer report and fix any blocking discovery/lifecycle
   issues.
2. Build experimental cocod from an exact workspace commit, including daemon
   lifecycle, credentials, storage lease and a safe observation contract.
   Pause again after its deterministic vertical slice.
3. Implement the scoped ecash payload exchange: opaque references, recipient
   authorization, bounded streaming, distinct send/delivery/receive outcomes,
   private retention and uncertain-outcome handling. Large ecash notes must not
   be passed through agent prompts or ordinary journals.
4. Add mixed-wallet tests, then the separately gated fault and compatibility
   matrix. The present checkpoint does not claim cocod or mixed-wallet support.
