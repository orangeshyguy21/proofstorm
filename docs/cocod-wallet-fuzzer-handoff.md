# cocod deterministic checkpoint and fuzzer handoff

Status: **deterministic checkpoint passed; focused agent restart and funding
targets held, with overall privacy/reporting gates still failed**.
This document records the initial handoff. See the
[execution hardening and current agent results](cocod-execution-hardening-2026-09-05.md)
for subsequent runs, fixes and scoped follow-ups.
This is phase 2 of the [wallet expansion](wallet-expansion-architecture.md).

## Exact experimental build

Select `cocod-wallet` version `0.0.17-dev.44e5101c`, configuration
`cocod-wallet/0.0.17/v1`, with empty authorable config. This is an unreleased
source build, not a published cocod 0.0.17 release. There is no preferred or
minimum supported version; plans must select the exact version. Existing
supported implementations retain their preferred-version behavior.

- Repository: `https://github.com/cashubtc/coco`, package `packages/cocod`.
- Commit: `44e5101cbea370132af6e68f88e01b47e39431c4`.
- Wallet image: `proofstorm-registry.localhost:5000/cocod-wallet@sha256:88dc907f64530788280b0ba603b1bd7f361c58281171e74ca25b0676fadfcdc7`.
- Controller used for the corrected deterministic run:
  `sha256:e9d3238b7ba216bef7afea1623f8d63fdc722545c050d06383c6b6072cc127a7`.
- Build recipe: `docker/wallet/Dockerfile.kube-cocod`; full archive, lock,
  recipe, Bun build image and Python runtime checksums are recorded in
  `docker/wallet/cocod-44e5101c-provenance.json` and carried in resolved locks.

The image is provisioned in the local registry for Linux arm64. Source and
`bun.lock` are unchanged. Frozen installation disables lifecycle scripts:
the unrelated `better-sqlite3` workspace adapter otherwise fails installation
in the Bun builder. Only the actual core, SQL storage and Bun SQLite dependency
packages are built using upstream scripts. The wallet runs the upstream entrypoint.
This does not claim bit-for-bit reproducibility across independent rebuilds.

To provision a fresh local registry, download the exact `artifact_url` from the
provenance file into a temporary build context as `source.tar.gz`, then build
with `docker build --platform linux/arm64 -f docker/wallet/Dockerfile.kube-cocod
-t localhost:5111/cocod-wallet:0.0.17-dev.44e5101c <context>` and push it.
The Dockerfile verifies archive and lock hashes. Verify the resulting immutable
image identity; never silently substitute a rebuilt digest into a published lock.

## Runtime contract

The existing component infrastructure runs a foreground daemon under UID 1000,
with its own persistent `/wallet` volume and `Recreate` deployment. There is no
cocod Service or external HTTP bridge. The listener is `127.0.0.1:62626`.
Exec clients inherit `COCOD_URL` and cannot silently autostart another daemon;
the foreground daemon alone has that client override removed.

`/health` measures daemon health. `/v1/status` reports initialization, seed
access and session state separately and requires the native administrative
credential at `/wallet/.cocod/credentials/current/client` (mode 0600). Use this
file privately inside the component. Initialization returns recovery material;
keep its response and every seed, credential, proof and preimage out of ordinary
outputs and reports. Use native help, the pinned API documentation and catalog
invocation hints. Python and Bun are present; do not assume curl or jq.

The tested policy is **protected initialization and explicit session unlock**.
At this pin, the public initialization API accepts a passphrase but offers no
mint selector, and its default mint is external. Initialize with a component-local
private passphrase so the session remains stopped. While stopped, edit only
`mintUrl` in native `/wallet/.cocod/config.json` to the lab mint URL; restart the
component to reload it, then start the native session with the passphrase read
privately from its file. This is native configuration, not proof/seed database
editing. Never let default initialization start a session against an external mint.
The acceptance fixture generates `/wallet/session.passphrase` at mode 0600;
this is unattended lab setup, not a production credential-management claim.

A protected session is locked/stopped after daemon replacement, while daemon
health remains good. Native `session stop` also leaves the daemon healthy.
Do not equate client exit, session stop and daemon termination. Native SQLite
ownership prevents a second daemon using the same state directory, even on a
different port.

Use existing `component_exec_live`, operation receipts, cancellation, restart,
evidence and teardown tools. Wallet-specific mutation/quote/oracle wrappers are
not advertised. Unsupported typed initialization is refused before execution.
Native mutations should use default private output, preferably direct argv.
Inspect actual exit, timeout and cleanup fields; operation completion alone
does not establish a payment outcome. Use `json_fields` for suitable native JSON,
including independent LND settlement evidence. Do not use grep/redaction as a
privacy boundary or synthesize a payment status from an unknown output format.

## Passive observations

`wallet_balance` uses a version-specific SQLite read transaction, `mode=ro`
and `query_only=ON`. Its mounted directory permits WAL coordination; wallet
records are not changed, no network is contacted and no second wallet SDK runs.
Unknown schema, missing/unregistered mint, busy database and invalid amounts
fail closed. The adapter checks this source pin's migration boundary.

The observation is scoped to the exact mint URL and `sat`:

| Field | Wallet-local meaning |
| --- | --- |
| `balance_sat` | Ready proofs without an operation reservation |
| `reserved_sat` | Ready proofs assigned to an operation |
| `total_ready_sat` | Spendable plus reserved ready proofs |
| `inflight_sat` | Proofs in the native inflight state |

No generic pending amount or mint-side proof-state conclusion is fabricated.
The native `/balance` endpoint is passive but merges ready categories into its
total. The SQLite projection retains the distinction that endpoint omits and
works while a protected session is stopped. At settled checkpoint boundaries,
native totals and portable balances must agree with independent payment evidence.

## Deterministic gate and evidence

Run `make e2e-cocod-wallet` on an idle local cluster after provisioning the
pinned image and deploying this controller. It is excluded from default `make
e2e` until the image is distributed. The checked-in gate is
`crates/proofstorm-acceptance/src/gates/cocod_wallet.rs`.

The gate uses Bitcoin Core 30.0, two LND 0.20.0-beta nodes, CDK mint 0.18.0
with explicit `input_fee_ppk=0`, and two cocod wallets on separate volumes.
It tests uninitialized health and authentication, protected initialization,
session lifecycle, exclusive ownership and client-only behavior, 5,000-sat
issuance, 700- and 300-sat payments with independent recipient settlement,
5,000/4,300/4,000-sat balances, identity persistence through restart, the second
wallet remaining empty, evidence export and verified teardown.

The operator fixture relays BOLT11 requests through private process memory/stdin.
This is not an ecash token relay or evidence of the proposed agent payload exchange.
Small BOLT11 requests can be explicitly extracted through the authorized native
surface in an agent run; ecash notes remain outside this checkpoint.

Run `cocod-checkpoint-01-20260905` stopped before funding because the new catalog
control was misspelled `wallet.balance` rather than `wallet_balance`. The corrected
registration uses the existing control. That run completed normal teardown and
retains its failure evidence. Do not count it as a money-flow test.

Corrected run **`cocod-checkpoint-02-20260905` passed**. Evidence is retained under
`dev/wallet-integration-runs/cocod-checkpoint-02-20260905/` (ignored by Git).

| Observation | Result |
| --- | --- |
| Funding | Independent LND `SUCCEEDED` for 5,000 sats; native issuance total and passive balance both 5,000 |
| First payment | Recipient settled 700 sats; native/passive balance 4,300, reserved/inflight zero |
| Restart | Replacement pod, one running owner, protected session locked; passive balance 4,300 before unlock; seed fingerprint unchanged |
| Second payment | Recipient settled 300 sats after unlock; native/passive balance 4,000 |
| Isolation | Distinct seed hashes; second wallet stayed at zero |
| Session stop/start | Daemon remained healthy; passive balance available while stopped; 4,000 after session restart |
| Native execution | All 27 executions have complete, untruncated streams and verified cleanup; two exit-1 refusals were deliberate |
| Teardown | Normal close receipt verified absence; independent cluster audit verified idle with no blockers |

No wallet records or seeds were fabricated. The observed debits equal invoice
amounts in this zero-input-fee direct-channel case; no general fee behavior is
inferred. All 215 workspace Rust tests, strict workspace Clippy, formatting,
generated schema/coverage/golden checks, Helm lint and 21 Python harness tests
passed. The source is an uncommitted workspace build based on
`cc73dee8395ad3edd1e609a6ca59be8d0ef71254`; retained diffs and digests identify it.

## Partner fuzzer brief

Begin with one bounded **cocod lifecycle/usability** run: 600 seconds, 50 model
steps, two equivalent attempts. Keep the existing hard cap, cleanup admission,
absolute deadlines and reporting reserve. Prepare a verified exact-version plan
from the tested topology before the benchmark; disclose assisted planning.
Use the existing runner and default configured model. No run was launched by
the build task.

The first question is whether the agent can distinguish daemon health, wallet
initialization and usable session state through the authorized native surface.
Exercise private protected setup with the lab mint, safe help/authenticated status,
empty passive balance, session stop/start and component restart with explicit
unlock. Retain only identity hashes and safe status fields. Require terminal
owned-execution receipts, evidence export, agent-observed closed receipt and a
final report inside the cap. No funding, ecash exchange, daemon-kill or controller
fault is needed in this first usability checkpoint.

Review that result before a separate funded smoke using the deterministic
5,000 → 4,300 → 4,000-sat baseline and independent LND recipient evidence.
Supply tested setup constraints but let the agent discover native commands;
do not paste the acceptance script into the benchmark and label it discovery.
Distinguish execution validity, wallet behavior, evidence sufficiency and teardown.
Reproduce concrete defects deterministically, fix the owning layer, and repeat
only the affected checkpoint. Stop after two equivalent failures without a new
hypothesis. Report incomplete work rather than expanding the time cap.

## Scope still open

This pin remains experimental. Agent usability claims are scoped to the reviewed
results in the execution-hardening report.
Nonzero fees, interrupted issuance/melts, crash recovery, concurrency, state
migration/restore, unprotected autostart policy, Nutshell-mint compatibility,
other architectures and agent-operated upstream candidate builds need separate
checkpoints. Cocod's bundled NPC plugin attempts external networking; the lab's
network policy blocks external egress and NPC behavior is outside this test.

The next infrastructure phase is private ecash delivery by opaque reference,
followed by mixed-wallet interoperability labs. Neither transport deduplication
nor a native command's success establishes redemption or exactly-once payment.
