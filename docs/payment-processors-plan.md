# First-class payment processors and canvas support

Status: implementation plan; no processor implementation or live acceptance has been completed by this task.

## Outcome and delivery order

Make external CDK payment processors reproducible, independently controllable lab components that appear on the existing GUI canvas with their actual dependencies, supported payment methods, units, and observed state.

Deliver complete backend-to-browser slices:

1. Shared processor contracts and LDK Server, including canvas support.
2. Bark and its Ark infrastructure, including canvas support.
3. Spark federation and open-ssp, after an exact-version CDK compatibility gate, including canvas support.
4. Mixed-backend and fault scenarios, resource accounting, and documentation consolidation.

Each slice stops at an agent-fuzzing checkpoint after deterministic acceptance. Review and resolve its blocking findings before expanding the next processor or payment-method surface; the checkpoint sequence and evidence requirements are below.

LDK Server comes first because it has an upstream Cashu regtest suite and is also a Lightning dependency of open-ssp. Spark is an actionable experimental candidate; it is no longer deferred for lack of a local SSP. The first three slices remain experimental until their own acceptance gates pass. Passing one payment method must not advertise another as verified.

The GUI stays an observer in this increment. Labs are authored through the existing CLI/MCP paths and appear automatically. A visual component palette, drag-to-connect authoring, and browser mutation endpoints are separate work.

## Current evidence and integration points

- Existing CDK 0.18 support covers external LND/CLN and embedded LDK/BDK. The embedded-LDK acceptance proves offer creation, not a complete external-processor payment cycle. See `crates/proofstorm-acceptance/src/gates/cdk_ldk.rs` and `cdk_cln.rs`.
- `crates/proofstorm-core/src/model.rs` has payment links and BOLT11/BOLT12/on-chain methods, but no processor/service/operator component kinds and no Ark-native method.
- `crates/proofstorm-core/src/catalog.rs`, `backend.rs`, and `publication.rs` own exact support tuples, configuration, dependency contracts, and publication validation. Extend these mechanisms rather than adding a second component registry.
- `crates/proofstorm-kube/src/adapter.rs` compiles and renders components, credentials, state, probes, and dependency conditions. The controller owns lifecycle and observation.
- The existing GUI is Leptos/Wasm/SVG. `crates/proofstorm-web/src/model.rs` positions nodes in four fixed columns by kind. Deeper processor dependencies will need a different layout.
- `ComponentView` lacks payment capability metadata; `LinkView` omits the authored binding. Both are in `crates/proofstorm-view/src/lib.rs`, projected by `crates/proofstorm-app/src/environment.rs`.
- The telemetry draft uses `BalanceAmount { label, sat }`. That cannot faithfully represent the LDK processor's msat balances. Review the complete sampler/presentation path before adding readers.
- GUI shell, inspector, themes, and telemetry are being changed under [the GUI plan](gui-plan.md). Implement against that work's settled module boundaries; do not duplicate its sampler, panels, or SSE subscription.

Upstream evidence reviewed for this plan:

| Candidate | Evidence | Remaining local proof |
| --- | --- | --- |
| LDK Server processor | [Processor source and regtest](https://github.com/cashubtc/cdk-payment-processors/tree/147c82e7882016ffd976d308db4a361b3a719deb/crates/ldk-server): real nodes, channels, Cashu flows; msat | Exact Proofstorm images, configuration, wallet unit compatibility, independent restart/fault recovery |
| Bark processor | [Processor source and regtest](https://github.com/cashubtc/cdk-payment-processors/tree/147c82e7882016ffd976d308db4a361b3a719deb/crates/bark): BOLT11, on-chain boarding/offboarding, outgoing arkoor, restart and reorg tests; sat | Package the pinned Ark server, Esplora, PostgreSQL and CLN dependencies; reproduce each advertised method |
| Spark processor + open-ssp | [CDK processor](https://github.com/cashubtc/cdk-payment-processors/tree/147c82e7882016ffd976d308db4a361b3a719deb/crates/spark) supports a custom SSP. [Open-ssp fixtures](https://github.com/benthecarman/open-ssp/blob/7e1bfe90c70c2240585b60a5cc925b67e7c01c62/e2e/upstream/README.md) describe live local Lightning acceptance. | CDK's spark-wallet 0.23.0 and the required open-ssp/operator/SDK revisions have not been exercised together here |

These are inspected source contracts, not claims that their suites were run locally. At implementation time capture the complete compatible revision set, dependency locks, build recipes, and image digests. In particular, the reviewed Spark CI image build references SDK 0.22.0 while its Cargo dependencies reference 0.23.0; resolve that discrepancy before reusing the fixture.

## Component and dependency model

Proposed component identities:

| Role | Catalog implementations to add | State ownership |
| --- | --- | --- |
| Lightning node | `ldk-server` using existing Lightning kind | Node identity, channel/wallet state, authenticated RPC credentials |
| Payment processor | `cdk-processor-ldk-server`, `cdk-processor-bark`, `cdk-processor-spark` under a new PaymentProcessor kind | Per-implementation quote/reconciliation state and embedded wallet state where applicable |
| Payment service | Bark server and `open-ssp` under a new PaymentService kind | Ark server state or SSP identity, liquidity wallet and durable settlements |
| Settlement operator | `spark-operator` under a new SettlementOperator kind | Individual operator identity, keyshares and database binding |
| Chain indexer | A pinned Esplora-compatible implementation under a new ChainIndexer kind | Index state and selected Bitcoin chain dependency |

Use a separate component for each independently restartable service. Spark operators are individual components with explicit federation membership. A GUI federation group is a presentation convenience, not a replacement for operator identities or fault targets. Embedded Bark/Spark wallets remain owned by their processor; do not fabricate additional runtime nodes for them.

Preserve `payment_backend` for mint → processor bindings with exact method/unit tuples. Preserve existing chain, database and Lightning peer links. Add a small typed service-dependency binding for processor → LDK RPC, processor → Bark server, processor/SSP → Spark operators, processor → SSP, and wallet/service → Esplora. Define finite roles, supported protocols, network agreement and cardinality; do not use unvalidated URLs or `network_path` as dependency configuration. Freeze exact wire names in the first milestone before renderer and GUI implementation.

Validate selected operator identities and threshold, duplicate identities, network mismatches, missing dependencies, and supported provider versions before materialization. Compile endpoints and private credential references from topology. Bootstrap operations such as mining, deposit claims, leaf funding and channel opening remain explicit recipes/acceptance steps; readiness probes must not fund a wallet.

For CDK, extend the existing mint contract with an explicit external gRPC mode and reject conflicting embedded/external selections. Confirm the exact 0.18 binary's method-registration and multi-processor behavior before promising combinations. If the binary only supports one relevant gRPC endpoint, reject unsupported combinations rather than introducing an implicit multiplexer. Compare advertised `GetSettings` capabilities with the declared binding without upgrading catalog support automatically.

Regenerate affected lab/CRD/catalog/environment schemas and contract fixtures. Preserve existing lab serialization and defaults. New optional view fields must decode when absent. Document incompatibility for older binaries encountering newly introduced enum variants; do not silently reinterpret them.

## Milestone 1 — LDK Server, processor contract, and visible lab

Backend work:

- Pin and package the node and processor independently. Reuse workload, storage, network policy, native execution and lifecycle facilities.
- Bind LDK Server to Bitcoin Core; expose P2P and authenticated RPC through separate endpoint contracts. Provision stable API credentials and TLS material privately.
- Render CDK's database-backed gRPC configuration using the exact binary's native configuration validation. Keep processor state separate from mint and node state.
- Introduce amount/unit handling for the processor's msat contract, including mint limits, quote checks, wallet selection, telemetry and formatting. Preserve existing sat configurations and avoid truncating sub-sat amounts.
- Add bounded protocol probes and dependency reasons. Separate process liveness, backend connectivity and ability to settle a funded payment.

Acceptance:

1. An ordinary CLI-authored lab and an MCP-authored equivalent resolve the same component contracts and materialize successfully.
2. A real regtest payer funds the mint, a CDK wallet mints and melts, and independent wallet/node observations establish amounts, fees and settlement.
3. A held/pending payment survives processor restart and event-stream reconnection; final state is recovered once, without duplicate payment or mint accounting. A terminal failure releases/compensates correctly.
4. BOLT12 send/receive capabilities are enabled only after separate successful settlement scenarios; offer creation alone is insufficient. Exercise amount/unit boundaries independently of whole-sat Lightning settlements.
5. The already-open browser discovers the complete topology, shows labeled bindings and readiness changes, retains selection/viewport, and shows the affected dependency after a processor or node outage.
6. Lab close removes its workloads and follows the existing declared storage-retention policy. Retained claims remain accounted for.

## GUI work shipped with every milestone

### Shared read model and canvas

- Add credential-free binding metadata to `LinkView` and selected, versioned capability metadata to `ComponentView`. Distinguish configured support, experimental lifecycle and observed readiness. Derive these from the resolved catalog/lock; the browser must not infer support from implementation names.
- Replace four fixed kind columns with stable dependency-depth placement. Separate dependency edges from peer/membership edges; handle cycles deterministically and keep disconnected nodes visible. Use stable component IDs, collision-free rows, measured graph bounds and fit-to-lab.
- Render processor, payment-service, operator and indexer tiles with distinct labels/icons within existing theme tokens. Every tile shows name, implementation/version and text readiness status. A payment processor is visible even when it has no readable balance.
- Label edges by role, with payment labels such as `BOLT11 · msat`. Parallel method bindings need separate selectable paths or a combined label that preserves every underlying link ID. Direction means consumer → dependency; it is not a live payment animation.
- Selecting a node highlights its immediate dependencies and dependents. Selecting a link opens its method/unit or service role and endpoints. Show unavailable dependencies from actual conditions; a declared line alone never means a healthy connection.
- Preserve selection, pan, zoom and expanded groups on SSE refresh/reconnect. Only structural changes recompute topology. Dynamic lab edits place new nodes deterministically without resetting the viewport; removal clears a vanished selection safely.

### Inspector, measurements, and resources

- Show enabled payment methods/units, linked mint/backend/federation/SSP, endpoint access context, exact version, lifecycle status, conditions and observation age. Show credential availability or authentication type, never credential contents.
- Use the existing sampler/cache/SSE path for bounded passive observations. Add explicit amount plus unit representation with compatibility for current sat data. Distinguish unsupported, unavailable, stale and zero values; keep integer precision through serialization and display.
- Attribute a balance to its actual owner. An LDK processor's backing-node balance is a reference to the node, not a second asset total. Likewise, SSP liquidity and processor-wallet funds are separate holdings. Do not sum these into an apparent lab-wide spendable balance.
- Show processor-owned wallet balance and pending amounts only where a verified passive source exists. A startup log or process health result is not a balance or settlement observation. Counts of quotes/events also require a real source; otherwise omit them.
- Include each service/operator in System → lab → component → container accounting exactly once. Collapsing a federation must not duplicate resources or conceal an unhealthy member.
- Reuse Activity/Sessions names and endpoint connection metadata. Link relevant existing operations to their components without collecting or exposing private payment payloads.

### Browser acceptance

Use real populated LDK, Bark and Spark labs as each becomes available. Verify dark/light themes, keyboard selection/focus, readable non-color status cues, edge labels, inspector navigation, pan/zoom/fit and small-screen overlays. Include parallel links, multiple mints sharing infrastructure, pagination beyond one component/link page, a three-operator group, long names, missing telemetry and disconnected nodes. Exercise live restart/fault/reconnect and add/remove updates without reloading. Compare canvas identity/edges with the resolved lab and runtime observations, and inspect HTTP/SSE responses for private-data leakage.

## Milestone 2 — Bark and Ark infrastructure

Package the Ark server version matching the selected Bark wallet/harness, its PostgreSQL storage, an Esplora indexer, and CLN dependencies. Reuse existing Bitcoin, PostgreSQL and CLN components. Support processor mnemonic/state persistence and independently controllable server/indexer outages.

Deliver BOLT11/sat and on-chain/sat with real mint/melt verification, then outgoing `arkoor` as an explicit custom-method increment. `PaymentMethod` currently has no custom method: add a constrained representation and versioned support contract before advertising arkoor, and verify the selected Cashu wallet can actually use it. Do not infer incoming arkoor support from outgoing support.

Acceptance includes confirmed deposit → Ark boarding → correct net mint credit; offboard melt and independent on-chain receipt; boarding/offboarding fees and limits; pre-confirmation/reorg boundaries; restart during pending settlement; and zero-fee outgoing arkoor accounting when enabled. Match upstream acceptance coverage before making stronger claims about already-credited deposits after deep reorgs.

Canvas acceptance shows mint → Bark processor, processor → Ark server/indexer, and server → database/Lightning/chain dependencies as separate identifiable services. Distinguish processor-owned funds from server liquidity and make a failed indexer/server dependency diagnosable from the inspector.

## Milestone 3 — Spark federation and open-ssp

Start with a bounded compatibility gate before integrating the full runtime: run the exact CDK Spark processor against pinned local operators and open-ssp, with live LDK mode. Configure the custom SSP URL, identity and GraphQL endpoint, fund both Spark and channel liquidity, then complete a real CDK BOLT11 mint/melt. Record any necessary upstream patches and pins explicitly. A Breez-only success does not pass this gate.

After compatibility passes, add operator identity/keyshare/database contracts, federation membership/threshold, open-ssp private admin and LDK credentials, SSP durable state, and explicit funding recipes. Keep each operator, SSP and processor independently restartable and faultable. The acceptance fixture may use two SSPs and two LDK nodes to match upstream's cross-SSP payment path.

Acceptance includes successful BOLT11 mint/melt, independently reconciled Spark and Lightning balances, liquidity exhaustion, SSP restart between Spark/Lightning transitions, failed sends, duplicate retries, and an unavailable operator. Separate threshold configuration from experimentally established outage tolerance; do not promise a quorum outcome until measured.

Preserve documented open-ssp boundaries: standard same-SSP internal invoices may be rejected; fee estimation is limited; BOLT12 uses non-atomic payout/refund semantics. The current CDK Spark processor only advertises BOLT11, so open-ssp's BOLT12 support does not enable CDK Spark BOLT12. Do not advertise incomplete cooperative exits or static-deposit SSP APIs as supported Cashu methods.

Canvas acceptance includes an expandable federation group showing each operator's identity and readiness, independent SSP and processor nodes, explicit SSP → LDK dependency, and accurate group resource counts. Missing payment compatibility appears as an experimental limitation, not a healthy-payment claim.

## Milestone 4 — Combined regression and release

- Add mixed labs with shared infrastructure, exact supported method/unit combinations and invalid combinations rejected at planning. Keep processor outage diagnosis/recovery available when unrelated components are unhealthy.
- Exercise network partition/heal using the actual service paths. Check that tunnels, Unix sockets or co-located paths do not accidentally bypass the fault being claimed.
- Test persistent restart state and authored configuration drift using existing update/lifecycle rules. Do not reuse incompatible retained state silently after changing a processor, network or identity.
- Run focused core/publication/render/schema tests; app/view contract tests; native presentation tests and Wasm compilation/lint; then the real processor and browser acceptance gates. Include existing LND/CLN, embedded LDK/BDK and cross-principal handoff regressions where shared units/lifecycle/private-output paths change.
- Publish example lab files, exact capability matrices, resource needs, bootstrap recipes and live evidence references. Distinguish config validation, quote creation and completed settlement in the report.

## Agent-fuzzing checkpoints

Agent fuzzing is exploratory use of the supported product surface, separate from deterministic acceptance. A scripted happy path does not pass this checkpoint. Use the existing `scripts/run-agent-usability-benchmark.sh`, scenario registry and evaluator where applicable; extend their contracts for these scenarios instead of creating another general campaign runner. Scenario names below are proposed, not registered or runnable yet. This document schedules checkpoints; it does not launch campaigns.

| Checkpoint | Entry evidence | Agent exploration and exit evidence |
| --- | --- | --- |
| F1 — LDK processor, after milestone 1 | Exact images and contracts; passing real mint/melt and restart gates; working canvas | Discover and author a lab through public CLI/MCP documentation; select valid msat bindings; fund, mint, melt, diagnose a processor outage, recover and close. Report unsupported combinations accurately. Verify canvas components, method/unit labels and dependency diagnosis against the authored lab. Resolve blocking discovery, unit/accounting, recovery or GUI findings before expanding. |
| F2 — Bark, after milestone 2 | Passing gates for the methods actually enabled, including boarding/offboarding and fee observations | Explore BOLT11 and on-chain flows, confirmation boundaries, fees and processor/server/indexer failures. Exercise outgoing arkoor only after its own deterministic gate. Independently reconcile wallet, Ark and chain observations; verify the GUI distinguishes processor funds from server liquidity. |
| F3 — Spark/open-ssp, after milestone 3 | Passing exact-CDK compatibility gate and live BOLT11 mint/melt; pinned federation/SSP/LDK fixtures | Discover operator/SSP dependencies, complete cross-SSP payments, diagnose liquidity exhaustion, an unavailable operator and SSP restart. Check same-SSP refusal and unsupported methods without inventing support or blindly retrying a pending payment. Verify expanded federation membership, member readiness and resource counts on the canvas. |
| F4 — Mixed lab, before milestone 4 release | F1–F3 findings triaged, supported matrix frozen and relevant deterministic regressions passing | Author supported combinations with shared infrastructure; explore invalid bindings, concurrent independent payments, topology edits and bounded faults. Check unrelated healthy components remain usable and failed dependencies remain diagnosable. Reconcile settlement and cleanup independently; verify GUI identity, selection and viewport survive live changes. |

### Campaign structure

1. Prepare a handoff checkpoint containing the source/controller/adapter/frontend pins, supported tuples, passing deterministic evidence, known limitations, permitted surfaces, available cluster capacity and a cleanup recipe. Give the agent a task and discoverable documentation, not hidden implementation knowledge. Record any assisted topology or prefunding explicitly.
2. Start with a bounded discovery-and-payment smoke run on fresh disposable regtest state. If it passes, run a separate bounded fault/recovery exploration. Fix discovery/lifecycle blockers before increasing concurrency or combinations. Give each run explicit time, step and token/cost budgets, with cleanup time reserved; retain the configured model unless deliberately selected otherwise.
3. Include a browser exploration session at every checkpoint. It may share the live lab with the protocol session using only supported read-only GUI controls. If the protocol runner has no browser tool, run the browser session separately and report its evidence separately; MCP success is not GUI acceptance. Ask the browser agent to find a named component, trace a method/unit binding, diagnose an injected dependency failure and verify recovery without reloading.
4. Use authorized lab operations and native protocols. Direct host Kubernetes/database access by the exploratory agent cannot substitute for a missing product capability. Record harness intervention separately from agent success. Keep spendable payloads, credentials and preimages in the existing private paths; use sanitized identifiers or independent verification results in reports.
5. Stop after repeated equivalent failures without a changed hypothesis, budget exhaustion, or an ambiguous mutation that cannot be resolved by observation. Do not replay an uncertain payment to manufacture completion. Switch to owned-operation diagnosis/cancellation and verified cleanup. Preserve failures and any separately performed operator cleanup in the result.

### Findings and advancement gate

Retain the prompt/scenario version, exact runtime and frontend pins, model configuration, assistance, sanitized transcript, operation IDs, independent amount/fee/settlement observations, browser screenshots and relevant HTTP/SSE evidence, elapsed usage and final cleanup receipt. Distinguish product defects, upstream defects, agent mistakes, discovery friction, GUI misrepresentation and harness failures. A successful command or agent summary is not settlement evidence.

Every actionable finding gets a reproducer, owner, severity and disposition. Block advancement for duplicate or incorrect settlement, unit/fee errors, private-data exposure, unrecoverable lifecycle/cleanup failures, unsupported capability claims, materially false GUI state, or discovery defects that prevent the required flow. An upstream blocker keeps the affected capability experimental/unavailable; it does not justify weakening acceptance. Record lesser limitations explicitly.

Turn reproducible defects into focused deterministic regressions, fix them, rerun the affected checks, then repeat the affected agent scenario on the corrected pins. Archive the checkpoint report and explicit pass/blocked result before progressing. Unaffected implementation work can continue while findings are resolved, but a slice is not complete until both its protocol-agent and browser checkpoints pass. No unbounded automatic campaigns are part of this plan.

## Completion criteria and dependencies

A processor slice is complete only when an agent or CLI user can create its lab, complete the supported payment flow, reproduce and recover a bounded failure, inspect that topology and outcome in the GUI, and close the lab with verified cleanup. Deterministic acceptance, GUI verification and the corresponding agent-fuzzing checkpoint are all part of each slice's exit gate.

Coordinate shared view types, amount units, graph placement and telemetry with the ongoing GUI work. Reuse the current application/environment API; this plan does not require a new API service. Cross-principal wallet handoff remains a separate capability, although any changed wallet-unit/private-output path must retain its existing acceptance behavior.

Unresolved issues to settle during the first relevant gate are exact upstream compatibility pins, CDK multi-gRPC-backend constraints, msat support throughout the chosen wallet path, passive balance interfaces, Ark custom-method compatibility, and realistic resource requirements for the full Spark/Bark fixtures. These are verification tasks with explicit outputs, not reasons to advertise untested support.
