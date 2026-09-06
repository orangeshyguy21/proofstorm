# CDK CLI and cocod wallet expansion

Status updated 2026-09-05: CDK phase 1 and its deterministic hardening baseline
passed. Cocod phase 2 also passed its deterministic money/lifecycle checkpoint;
focused agent checkpoints verified restart/unlock and funding/payment behavior,
while overall privacy/reporting gates remain failed. The initial
[cocod fuzzer handoff](cocod-wallet-fuzzer-handoff.md) retains its original scope.
See [cocod execution hardening](cocod-execution-hardening-2026-09-05.md) and the
[CDK hardening results and agent-usability limits](cdk-wallet-hardening-2026-09-05.md).
The funded CDK agent benchmark has not passed all gates together. Private ecash
exchange and mixed-wallet laboratories remain subsequent phases.
[Structured invoice relay](structured-invoice-relay.md) is implemented for native
cocod invoice text and LND invoice JSON; its deterministic money/restart gate
passed. It deliberately exposes small validated Lightning invoices and does not
implement the private bearer-note exchange below.
The focused agent relay execution was valid and its target held; report
accounting still failed review, with an independent correction retained.
Reviewed 2026-09-04 against Proofstorm commit
`e6f918a3b3c2782958c2301ee838e53f4468d5eb`.

## Recommendation

Add `cdk-cli-wallet` and `cocod-wallet` as independent implementations of the
existing `wallet` component kind. Keep native commands as the normal experiment
surface. Reuse the catalog, compiled component plans, controller, persistent
storage, live execution, faults, action journal, evidence, and verified teardown.
Introduce small adapter boundaries where the current implementation assumes
Nutshell. Do not require complete typed-wallet parity before either wallet can
run useful experiments.

Deliver CDK first to validate a second CLI wallet. Bring up cocod immediately
after that deployment checkpoint, before treating the CLI design as universal.
Its persistent daemon is the necessary counterexample for lifecycle and reads.
Hand each wallet to the agent fuzzer after its deterministic vertical slice
passes; keep mixed-wallet and crash laboratories as later, separately gated
rounds.

The user's direction explicitly includes unreleased cocod. This proposal
supersedes the release prerequisite in `dev/ROADMAP.md` B3.5f and Gate H for
experimental cocod onboarding. It does not declare that roadmap's full parity
gate complete. Later native-first guidance in
[native-first-experiments.md](native-first-experiments.md) governs which typed
operations earn implementation.

## What is already extensible

| Layer | Existing extension point | Assessment |
| --- | --- | --- |
| Identity and discovery | `proofstorm-core/src/catalog.rs`: exact version, config schema, image digest, runtime endpoints and controls | Reusable; experimental-only implementations need a catalog correction |
| Configuration and execution | `proofstorm-core/src/backend.rs`: `BackendContractRegistry`, `ComponentPlanContract`, mounts, environment, conditions and admission | Reusable, with new backend registrations and typed config variants |
| Deployment | `proofstorm-kube/src/adapter.rs`: `COMPONENT_RENDERERS` | Reusable; current wallet renderer explicitly requires Nutshell and runs an idle shell |
| Operations | `proofstorm-kube/src/operation.rs`: action rendering and bounded Jobs | Scheduling is reusable; wallet rendering selects `nutshell_wallet_image` and embeds Nutshell commands |
| Quote evidence | `proofstorm-core/src/quote.rs`, durable observations in `proofstorm-store` | Reuse attribution and history; native IDs and meaning need adapter-specific validation |
| Native execution | `proofstormd/src/main.rs`: `reconcile_component_exec_live` | Already operates inside the deployed component; suitable for CLI and daemon clients |
| Acceptance and experiments | `proofstorm-acceptance`, `scripts/agent-usability-scenarios.json`, existing benchmark runner | Reuse gates and evidence pipeline; add wallet dimensions and bounded scenario briefs |

This is a compile-time adapter architecture, not a drop-in dynamic plugin
system. Adding a wallet currently requires Rust registrations, a renderer,
an image, and tests. That is acceptable for these two additions; a dynamic
loader or another controller is unnecessary.

Legacy Compose already builds CDK CLI 0.18.0 in
`docker/wallet/Dockerfile.cdk` and drives it through `scripts/lib/wallet.sh`.
That is useful packaging experience, not Kubernetes support. Likewise,
`cross_implementation_wallet.rs` currently tests two mint implementations with
Nutshell wallets on both sides; it does not prove cross-wallet interoperability.

## Upstream facts that affect the design

CDK's [0.18.0 CLI entrypoint](https://github.com/cashubtc/cdk/blob/v0.18.0/crates/cdk-cli/src/main.rs)
supports an explicit work directory, SQLite, and noninteractive invocation.
It stores `cdk-cli.sqlite` and `seed` in that directory. It calls
`recover_incomplete_sagas()` before command dispatch, including balance.
Consequently a native balance command must not be presented as a guaranteed
passive observation. A copied database alone does not prevent recovery from
affecting the mint through network calls.

At this exact release, `mint-pending` calls `check_all_pending_proofs()` despite
its CLI help describing paid-quote claims. Resume an existing paid quote with
`mint <url> --quote-id <id>` and independently verify issuance and balance.
See the pinned [pending-mints implementation](https://github.com/cashubtc/cdk/blob/d3dec24c784e8fec1fd65f853241c7a2261c7abd/crates/cdk-cli/src/sub_commands/pending_mints.rs).

The cocod source reviewed is Coco commit
[`44e5101cbea370132af6e68f88e01b47e39431c4`](https://github.com/cashubtc/coco/commit/44e5101cbea370132af6e68f88e01b47e39431c4).
Its [package manifest](https://github.com/cashubtc/coco/blob/44e5101cbea370132af6e68f88e01b47e39431c4/packages/cocod/package.json)
is private, reports `0.0.17`, and consumes workspace Coco packages through Bun.
It is now the tested experimental Proofstorm pin `0.0.17-dev.44e5101c`, built
with unchanged source and lock. It is not a published 0.0.17 release.

Its [README](https://github.com/cashubtc/coco/blob/44e5101cbea370132af6e68f88e01b47e39431c4/packages/cocod/README.md)
and [API reference](https://github.com/cashubtc/coco/blob/44e5101cbea370132af6e68f88e01b47e39431c4/packages/cocod/docs/API.md)
describe foreground daemon execution, authenticated HTTP on
`127.0.0.1:62626`, and separate process, wallet, and session lifecycle.
Explicit client endpoints disable automatic daemon startup. The state directory
contains the wallet database, credentials and a SQLite process-ownership lease.
Initialization returns recovery material. The phase 2 gate verified these
contracts in the pinned container using protected initialization and explicit
session unlock. The public initialization API cannot choose a mint: private
protected setup, native configuration while stopped, restart and explicit unlock
are required to select the lab mint. See the handoff for the exact workflow.

## Architecture decisions

### 1. Preserve separate component, native, and portable-operation contracts

Each wallet owns its packaging, configuration, data layout, native discovery
guidance and workload readiness. The infrastructure consumes the existing
compiled component plan. Share PVC/security/metadata helpers where useful,
while leaving launch and readiness implementation-specific.

Add a small internal `ProtocolActionAdapter` registry when introducing the first
portable operation for another wallet. Resolve it from the locked wallet
implementation and action-adapter version. Start with balance observation;
extract the existing Nutshell implementation without changing its behavior.
Make `protocol_action_adapter_version` absent for a native-only adapter instead
of automatically advertising a protocol-action implementation.

The adapter owns operation availability, prerequisites, invocation strategy,
observation semantics and decoding. The controller retains authorization,
deadlines, replay fences, journaling and resource ownership. Unsupported typed
actions must be refused before execution even if the underlying native wallet
can perform an equivalent action. Runtime controls describe installed drivers;
upstream features describe the wallet. Do not conflate them.

No wallet-specific MCP names or general SDK facade are needed. A CLI or HTTP
invocation can differ internally without changing the lifecycle/evidence API.
Do not recreate cocod as a Proofstorm-owned cashu-ts wallet.

### 2. Make experimental source identity real

Register cocod as prerelease/experimental with an exact commit-derived version
and immutable OCI digest. Build from the complete Coco workspace with its lock,
not an assumed published npm package. Pin the Bun runtime and build image;
record repository, full commit, package path, dependency-lock digest, build
recipe digest, runtime identity, transformations and resulting image digest.

Two current contracts need explicit changes:

* `implementation_support()` requires exactly one `Preferred` entry and
  `CatalogImplementationSupport.preferred_version` is mandatory. Allow zero
  preferred versions only for experimental-only implementations, represent no
  default explicitly, and require an exact version in plans for those entries.
  Existing supported implementations retain their one-default rule. Review
  `minimum_supported`/`supported_versions` projections so experimental availability
  is not presented as tested compatibility.
* `CatalogEntry.source` and resolved-lock `source` are `CandidateSource`, which
  requires a PR URL and candidate ID. Add optional immutable build provenance
  for ordinary commit builds, carried into the lock and its digest. Preserve
  existing PR receipts and historical lock identities. Never fabricate a PR
  or hide a Git commit behind an untraceable `source_digest`.

Initial images can use a checked-in, operator-run build recipe. The current PR
builder only builds variations of installed adapters and has an implementation
allowlist; it cannot onboard cocod by itself. Agent-operated cocod PR builds
are a subsequent addition, not a prerequisite for the first experimental pin.
Never substitute a different image when the selected source fails to build/run.

### 3. Model the actual wallet process

For CDK, use a persistent CLI workspace with an explicit directory such as
`/wallet/cdk`, SQLite, sat units, and noninteractive calls. Native `--help`
must work under the restricted workload UID. Use independent state and seeds
for every component; preserve them through restart. Adapt the existing Docker
recipe to exact source/build provenance and immutable runtime images.

For cocod, run the daemon in the foreground as the workload process. Use
`HOME=/wallet` for private state and set `COCOD_URL=http://127.0.0.1:62626`
for clients. Keep the initial listener on loopback; live component execution
already supplies the correct process/network context. An external Service or
new remote API bridge is unnecessary for onboarding.

Distinguish daemon health from wallet initialization and active session status.
An uninitialized but controllable wallet must not make initial lab readiness
impossible. Money operations require initialized/usable wallet state. Health
probes must not initialize a wallet, start a session or restart the daemon.
Pin and test the chosen unprotected/protected session policy; do not infer it
from `/health` alone. Capture initialization output privately because it includes
recovery material. Discover credentials through cocod's own private client file.

Use single-owner rollout behavior, such as a one-replica Deployment with
`Recreate`, and test termination and restart with the same PVC. A replacement
process must not race an old owner. Do not launch an additional daemon in an
operation Job or in an offline forensics context. Database migrations are part
of the source upgrade gate, not an implicit safe rollback promise.

### 4. Make observations passive and explicit

Add a portable balance observation for each wallet, scoped to wallet, mint and
unit. Define spendable, pending/reserved and total separately; omit unavailable
categories rather than substituting zero. Attach observation time and locked
adapter identity through the evidence envelope. Unknown output/schema is an
observation failure, never an empty balance.

CDK needs a version-specific read-only database projection or verified upstream
passive export. Do not use ordinary `cdk-cli balance` as the portable reader.
For cocod, prefer a verified passive query to the running daemon, with private
credential handling. If its query synchronizes state, treat it as a mutation
and use a passive alternative for observations. Running a second wallet SDK
against its database would bypass the implementation being tested.

Phase 2 finding: cocod's native balance query is passive but merges spendable
and reserved ready proofs. The implemented SQLite read transaction retains
those categories plus inflight proofs and remains available with the wallet
session stopped. It does not load a second SDK or infer missing pending amounts.

Do not generalize Nutshell's `cp -R` snapshot recipe to actively written
databases. Use a transactionally consistent read/backup with declared semantics;
for forensic snapshots, prevent network access and automatic recovery. Compare
the observation to independent native-state evidence, especially under faults.

Keep native quote/operation IDs distinct. Current store validation restricts
quote IDs to component-style slugs; audit each new wallet's IDs before enabling
quote tools and introduce bounded opaque identifiers if required. Quote history
remains attributed observations, not a second wallet state machine. Missing
quote correlation means that typed quote workflow is unavailable.

### 5. Keep coordination small and earned

Start mint selection/trust through native commands and explicit mint URLs.
Typed wallet-to-mint trust/default links remain a later parity item, justified
when planning/admission needs them. They must describe intent and validate
selection, not silently overwrite wallet-native trust state on every reconcile.

Typed invoice/pay parity is not needed for the first fuzzer handoff. When added,
dispatch payer and recipient through their own adapters, preserve the existing
payment reservation/replay guarantees, and never assume both use one image or
database schema. Serialize ordinary adapter-driven mutations per wallet; test
concurrency deliberately in dedicated experiments. A submission idempotency key
does not prove exactly-once native payment execution after an ambiguous timeout.

The agreed direction for agent-operated ecash transfers is a lab-local payload
exchange: agents direct delivery between wallet endpoints using opaque handles,
while the actual notes stay outside model context. Large tokens make this useful
for context and output budgets as well as private handling. The current Nutshell
invoice flow is not a generic token relay. Direct native interoperability tests
remain possible before the exchange exists; the exchange is the planned default
for reusable agent transfer scenarios.

### 6. Exchange ecash payloads by reference

The agent controls source, destination and delivery; Proofstorm moves the bytes.
One agent can operate both endpoints. Separate agents can use the same mechanism
later when their existing principal/lease permissions authorize the relevant
endpoints; possession of a handle alone must not grant access.

The proposed flow is:

1. Execute the source wallet's native send operation and capture its token
   directly into private runtime storage, before ordinary stdout/artifact
   collection. Return a small payload reference with source identity, media
   type, byte length and content digest. Record separately any wallet-reported
   amount/mint metadata and its provenance; the transport does not validate value.
2. Bind delivery to a destination wallet and authorize it within the same lab.
   Stream the unchanged payload into a private destination inbox. Keep transfer
   storage separate from both wallets' state volumes. The agent sees a delivery
   receipt, not token text or base64 chunks.
3. Have the destination's native receive operation consume the delivered payload
   using its supported input path. Prefer files, stdin or a streamed local API
   body; where a CLI only accepts an argument, handle that internally with a
   tested size limit and no token-bearing command text in the action journal.
4. Record the native receive result and independently observe wallet state.
   Transport completion means bytes arrived; it does not mean the mint accepted
   the proofs or the recipient acquired spendable balance.

This is a shared transport boundary with small wallet-specific extraction/input
bindings. It neither implements Cashu send/receive nor replaces the native
wallets with an SDK harness. Initial scope is same-lab, directed payload delivery;
cross-lab federation and a general peer-discovery network are unnecessary.
The exact tool names and physical relay mechanism remain implementation choices.

The contract must include:

* **Bounded streaming:** per-payload, per-lab and concurrent-transfer limits,
  checksums, backpressure and deadlines independent of the small receipt limit.
  No silent truncation. Reserve capacity before a value-changing producer runs;
  preserve an explicit unresolved outcome if capture fails after native send.
* **Scoped access:** owner, source, destination, lab and lease attribution;
  server-side authorization for each operation; private paths generated by the
  runtime rather than caller-selected host paths. References expose no payload
  through ordinary status or evidence reads.
* **Separate journals:** native send, byte delivery and native receive have
  distinct identities/results. Deduplicate retries of the same delivery request;
  never automatically repeat native send to regenerate a missing payload.
  After an ambiguous receive timeout, record an unknown outcome and reconcile
  recipient/mint evidence before deciding whether another native attempt is valid.
* **Explicit retention:** retain pending payloads for a bounded, advertised
  recovery interval; support release and verified lab cleanup. Expiring a
  payload reference does not expire the Cashu notes or reclaim their value.
  Evidence retains metadata and outcomes, not spendable notes.
* **Honest fault semantics:** label whether delivery uses an infrastructure
  relay or the wallets' lab network path. An infrastructure relay must not count
  as evidence of wallet-to-wallet reachability. A scenario testing peer transport
  under partition must select a path governed by that fault. Mint redemption
  still uses the receiving wallet's native network context.

The normal delivery path targets one recipient. Intentional duplicate delivery
or delivery of the same notes to competing recipients is a separately explicit
adversarial treatment with its own receipts, not an accidental transport retry.
Transport deduplication is never evidence that ecash cannot be double-spent.

## Rollout and checkpoints

Each row is an independently reviewable change or small PR series. A checkpoint
records source/image/config identities, changed claims, checks, live evidence,
known failures and the decision to continue. No automatic progression from
"pod running" to interoperability or recovery claims.

| Phase | Deliverable | Exit checkpoint | Agent fuzzer involvement |
| --- | --- | --- | --- |
| 0 — contract freeze | Confirm source pins, initial BOLT11/sat scope, passive-read strategy, experimental catalog/provenance changes and bounded execution plan | Written operation/support matrix; build recipes reviewed; unresolved upstream semantics named | Prepare briefs only |
| 1 — CDK vertical slice | Exact image, backend/config/renderer registration, native help/init/fund/spend, passive balance adapter | Deterministic real-regtest lab passes funding, payment, independent balance check, two-wallet isolation, restart and verified close; existing Nutshell regressions pass | First short CDK discovery/smoke session after exit |
| 2 — cocod vertical slice | Pinned workspace build, experimental catalog entry, foreground daemon, private initialization, session controls and passive balance | Same money/persistence/cleanup checks plus authenticated query, no autostart, single-owner restart and health/session distinction | First short cocod discovery/smoke session after exit |
| 3 — interoperability | Capability-filtered wallet/mint matrix; ecash payload exchange by reference | Large-payload delivery, interruption/retry, access isolation and cleanup pass; native recipient acceptance and actual fees are independently verified for claimed combinations | Bounded mixed-wallet scenario labs using payload handles |
| 4 — recovery and concurrency | Supervised native process control, independent pending/reservation observations, restart/interruption scenarios | Real interrupted operation observed, no replay assumption, recovery/spendability independently checked; inconclusive attempts retained | Crash, partition, concurrency and reconnect rounds |
| 5 — support checkpoint | Generated compatibility report, documentation, pinned recipes and regression promotion | Every advertised combination traces to passing evidence; known failures and omissions visible; cleanup verified | Held-out briefs; source upgrades trigger selected reruns |

CDK phase 1 and cocod phase 2 deliberately share only the observation boundary
and component infrastructure. If phase 1's abstraction requires cocod to run
like an idle CLI wallet, revise it at phase 2 before adding more operations.

The initial matrix is Nutshell, CDK CLI and cocod against CDK and Nutshell mints,
using BOLT11/sat and a known-working LND funding route. Exercise six wallet/mint
cells before claiming that matrix. Token interoperability is a separate axis:
test the three distinct wallet pairs in both directions on a common mint.
Cross-mint Lightning payments are another axis; vary payer/recipient direction
without taking the full product of storage, backend, protocol and fault options.
Add CLN, BOLT12, on-chain, auth, restore and spending conditions only as explicit
capability-filtered tranches. Unsupported and untested are different outcomes.

## Fuzzer handoff contract

The first handoff occurs after one real wallet slice works deterministically,
not after complete typed parity and not while the agent must repair packaging.
The first brief should ask the agent to discover the exact wallet, materialize
it, use native help, fund/spend, compare a portable observation, restart, export
evidence and close. Let the agent discover commands; do not give it a complete
solution script and call that a usability test.

Provide the fuzzer with:

* Exact Proofstorm and wallet revisions/digests, model/profile, scenario variant,
  supported controls, known limitations and a tested setup/funding boundary.
* One question per run, observable success/refusal criteria, named independent
  evidence and allowed fault targets. Separate CDK client death, cocod client
  death, cocod daemon death and wallet-session stop.
* A fresh workspace and lab, one benchmark session at a time, a fixed action/
  step/time budget, and a cleanup reserve. Concurrent traffic inside that one
  lab is allowed only when it is the bounded treatment.
* A checkpoint report separating faithful execution, target behavior, evidence
  sufficiency and teardown. Retain expected refusals, upstream bugs, harness
  failures and inconclusive timing attempts as distinct results.

Suggested starting budgets are 15 minutes/60 steps for a single-wallet smoke
and 30 minutes/100 steps for a later fault round, with 20% reserved for evidence
and cleanup. These are proposed limits, not measurements or active automations.
Review every 10 minutes or 10,000 new tokens; stop after two equivalent failures
without a changed hypothesis. Record coordinator and fuzzer usage separately,
including cached input, uncached input, output/reasoning and peak context.

The current runner bounds time, steps and repeated equivalent plans, exposes
absolute cleanup/hard deadlines, clamps observation waits and enforces cleanup
admission. Optional token ceilings trigger cleanup at observed model-step
boundaries. Shared native supervision now retains terminal exit/signal,
stream and cleanup receipts; the small reliability checkpoint passed. See
[reliable native execution](reliable-native-execution.md). This supersedes the
earlier timeout limitation in the recovery report, but does not establish every
restart/descendant fault case. Preserve partial evidence, expose unknown outcomes,
and never retry a possibly accepted payment just because its client timed out.

After each run, reproduce a concrete defect deterministically where possible,
fix the smallest owning layer, and rerun the affected brief. Add a typed helper
only when the transcript establishes recurring coordination or observation
value. Do not add an MCP wrapper for each mistaken native flag.

## Implementation map and validation

* `crates/proofstorm-core`: catalog/default-selection/provenance, backend
  registrations and config, optional action-adapter identity, coverage metadata;
  quote-ID changes only when needed for supported observations.
* `crates/proofstorm-kube`: wallet renderers, restricted pod state and startup,
  internal protocol-action dispatch and passive observation implementations.
* `crates/proofstormd`: reuse lifecycle and execution; improve supervision and
  partial-result handling before adversarial process experiments.
* `crates/proofstorm-mcp` / `proofstorm-store`: capability/admission and portable
  receipts; preserve existing replay/evidence contracts. No new wallet-specific
  tool families. Add private transfer coordination only for phase 3's need.
* `docker/wallet`, `tools`, `scripts`: immutable build recipes/runtime pins and
  bounded benchmark changes. `crates/proofstorm-acceptance`: deterministic
  wallet gates. `coverage` / `schemas`: regenerate affected contracts and reports.

Meaningful checks include unsupported-operation refusal, experimental explicit
selection, source/image lock identity, consistent passive reads during writes,
no recovery side effects from observations, state isolation, daemon single-owner
restart, and preservation of Nutshell behavior. Real regtest gates are required
for funding, settlement, fees and recovery claims; mocks and FakeWallet are not
substitutes. Check negative cases and absence after teardown alongside successes.

Rollback an adapter release by withdrawing new selection/capability claims and
using the previous pinned implementation for fresh labs. Preserve failed-run
evidence. Never downgrade an already migrated wallet database in place without
a separately tested restore/migration contract.

The original architecture review was read-only. Subsequent CDK and cocod build
checkpoints are linked above; their retained evidence defines the tested scope.
The six-cell wallet/mint matrix and mixed-wallet ecash exchange are not complete.
