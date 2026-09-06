# Proofstorm architecture simplification review

> Superseded coordination design: leases are replaced by nonblocking [session tracking](session-tracking-2026-09-06.md); private-transfer permissions are separate. Historical results below describe their recorded version.


> Subsequent decision: run time limits and action-count budgets have been removed entirely. The budget recommendations/results below describe the earlier design, not the current interface. See [removal checkpoint](run-limit-removal-2026-09-06.md).

Reviewed 2026-09-06 at `453414edce2420867faf125214e9230ccb595b6f`. Impact means improvement to developer usability, product scope, and maintenance, not implementation effort or vulnerability severity. This is a static review of current code, Git history, and retained checkpoint reports. No code was changed, cluster accessed, or tests rerun. Historical test results cited below are the reports' results.

The product should fit this sentence:

> **Proofstorm spins up Bitcoin, Lightning, and Cashu test labs so you can connect your app, test failures, and see what happened.**

The core is useful. The biggest source of bloat is how much orchestration the caller must understand, followed by product behavior embedded in the MCP transport. The right simplification is to make lab infrastructure independently useful and keep advanced agent coordination optional. Replacing Kubernetes, deleting the journal, or building a universal wallet SDK would create more work than it removes.

## High impact

### H1 — Make the ordinary lifecycle about a lab; make runs and leases optional to manage

**Evidence.** The MCP surface supports plan/apply alongside create/edit/component/link mutation/validate/publish/materialize. Runtime requests separately carry instance, experiment, lease, operation, and idempotency identities. Finalization has an explicitly prescribed wait → lease release → experiment close → export → lab close sequence. The experiment record itself contains ownership, identity, timestamps and phase, rather than a research definition. See [MCP lifecycle](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:2804), [release contract](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:3686), and [experiment model](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/experiment.rs:14).

**Change — mold the interface; cut mandatory ceremony.** Provide one ordinary create/start path, one inspect path, and one finish/close path. Retain explicit plan review when useful, but make configuration replacement and correction a normal operation rather than requiring callers to invent new plan identities. Resolve versions and retain the immutable lock internally. Call an experiment a run in the product; create a default run automatically for managed actions. Clients should generate retry identities automatically and reuse them on retries; the server still enforces idempotency.

A normal local user should not acquire and release a lease manually. Keep ownership, explicit budgets and revocation where they matter, especially delegated access, but let the normal lifecycle manage them. Automatic management must expose owner, expiry and limits; it must not silently renew finite permission forever. Do not automatically create a new run or budget after exhaustion. Finishing should report completed, interrupted and unresolved operations, preserve evidence, and return a cleanup receipt.

**Keep.** Durable action history, replay protection, immutable deployment identity and explicit advanced delegation. Cross-principal handoff now exists and has a documented deterministic pass; deleting leases wholesale would remove exercised behavior. The [handoff checkpoint](/Users/admin/Sites/corpus/proofstorm/docs/private-ecash-cross-principal-checkpoint-20260906.md:3) does not establish separate-model usability.

**Acceptance.** A fresh local session starts a lab, executes a command, reconnects, reads history and closes the lab without manually supplying experiment or lease IDs. Explicit delegated scopes and exhausted budgets still refuse unauthorized new work. Interrupted finalization can be resumed without repeating financial actions.

### H2 — Make connecting another application a first-class contract

**Evidence.** Public component status currently describes an internal service and named ports. Network policy admits lab peers, subject to exclusions, plus DNS; it does not consume the declared topology links as a strict allowlist. Some interfaces deliberately remain local, such as CLN's Unix RPC and cocod's loopback listener. I found no supported external application connection lifecycle in the reviewed core/MCP/runtime code. See [component status](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/instance.rs:48), [network policy](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/adapter.rs:2848), and [native execution environments](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/backend.rs:3100).

**Change — mold the product around real service endpoints.** Define a small connection descriptor: component and endpoint identity, protocol, address valid for the chosen access context, authentication method, separately delivered credential material, readiness, and connection lifetime. Distinguish application-facing endpoints from administration endpoints. An internal Service DNS name is not a laptop connection string.

Start with one supported local connection mechanism, such as a managed loopback tunnel to an explicitly selected service. Export ready-to-use configuration for a host application without placing credentials in public graph responses or agent transcripts. Do not build an ingress platform, universal RPC proxy, or remote identity federation for this increment. Declare unsupported local-only endpoints honestly; do not silently change their listeners.

External applications must be able to use native protocols without turning every request into a Proofstorm action. That means lab leases coordinate Proofstorm-managed operations; they cannot promise exclusivity over all protocol traffic. Identify attached clients and explain whether their path participates in network faults. A tunnel is not evidence that ordinary lab routing survives a partition.

**Acceptance.** An ordinary application outside the lab connects to a mint and one authenticated Bitcoin or Lightning endpoint using exported configuration, without MCP or Kubernetes credentials. Restart/address changes are handled or explicitly reported. Disconnect and lab deletion invalidate the connection. Read-only visualization responses contain metadata, not credentials. The endpoint's fault behavior is documented and tested.

### H3 — Extract shared application behavior from MCP; define who owns each kind of state

**Evidence.** `proofstorm-mcp/src/lib.rs` is 13,519 lines including tests. More significant than its size: it contains planning, evidence assembly, quote handling, authorization mapping, action admission/submission, Kubernetes access and status synchronization. Operation status reads persist terminal artifacts; experiment closure performs reconciliation. Lease acquisition writes SQLite before mirroring authority into a Kubernetes annotation. These are real application responsibilities, not transport formatting. See [status read](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:5637), [experiment reconciliation](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:3552), and [runtime lease mirror](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:9585).

**Change — DRY the behavior before adding another API.** Move planning, lifecycle, submission and observation into ordinary shared Rust modules with domain errors. MCP should translate requests and responses. A developer interface and the proposed visualization API should reuse these modules rather than invoke MCP handlers or duplicate their business rules. This requires no new service fleet or plugin framework.

Keep clear state ownership: the local store owns authored configuration, permissions and durable history; Kubernetes owns observed workloads and execution status; private custody owns payload bytes and its access/consumption fences. Give synchronization a named, independently callable path with restart recovery and freshness timestamps. Pure observation reads should not trigger jobs, consume lease budgets, or be the only mechanism preserving results. A passive balance measurement may still require bounded execution; expose cached observations separately from requesting a new measurement.

Also fix misleading transactional language: `lab_apply` advertises atomic publication/materialization but performs a store publication followed by runtime materialization. These are resumable idempotent stages, not one cross-system transaction. Show stage and recovery explicitly. See [apply implementation](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:2870).

**Acceptance.** The same domain operation works through two thin adapters with consistent behavior. After a client disconnect and controller restart, completed results become durable without the user discovering the right status-read sequence. A failed apply shows the completed publication stage and resumable runtime stage. Observation-only requests perform no experiment actions.

### H4 — Remove the global readiness gate from operations that have narrower prerequisites

**Evidence.** The controller projects the lab to Ready only when all components are ready or intentionally stopped. MCP's common `apply_action` refuses every action unless that aggregate phase is Ready. The component layer already models operation-specific prerequisites. Protocol observation can also be Unknown while waiting for the rotating prober scheduler. See [aggregate readiness](/Users/admin/Sites/corpus/proofstorm/crates/proofstormd/src/main.rs:2492), [submission gate](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:9847), [prerequisite model](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/backend.rs:80), and [waiting probe](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/adapter.rs:853).

**Change — cut the redundant broad prerequisite.** Use aggregate readiness for display and initial setup completion. Admit an operation according to the target, operation and required dependencies, while retaining authorization and closing/closed checks. Logging, diagnosis, restart and healing should not require unrelated components to be healthy. This is a static control-path finding; I did not reproduce an outage in a running lab.

**Acceptance.** With one component unexpectedly unhealthy, another healthy component remains inspectable and usable; diagnosis and authorized recovery of the failing component remain available. A payment whose required backend is unavailable still receives a precise refusal. Waiting for a probe slot is distinguishable from a failed service.

### H5 — Keep native protocols central; move scenario-specific workflows out of the default product surface

**Evidence.** Proofstorm has six toolset profiles, a native execution path, typed wallet/channel actions, recipe bootstrap/channel helpers, and a recipe fee-matrix runner inside MCP. A successful recent private-transfer agent run used native commands, custody and passive observations without another wallet mutation wrapper. Other reports show planning and tool-contract friction before wallets were even launched. See [toolsets](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:2041), [recipe workflow](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:4242), [successful native flow](/Users/admin/Sites/corpus/proofstorm/docs/private-ecash-kimi25-run04-20260906.md:31), and [planning retry](/Users/admin/Sites/corpus/proofstorm/docs/cdk-wallet-fuzzer-retry-2026-09-05.md:25).

**Change — cut default surface; retain useful implementation.** Make one coherent developer/native surface the default. Keep advanced and conformance workflows explicitly selectable. Move fee matrices and other scenario sequences to reusable scenario fixtures or the acceptance layer, using shared primitives. Preserve useful funding/bootstrap conveniences as named recipes. Do not make all supported wallets implement a uniform mutation API just for symmetry.

Keep typed functionality where it provides observable value: passive observations, native invoice projection, safe private delivery, bounded faults, and cross-component coordination. Existing payment reservations prevent duplicate payment; they are not removable merely because native commands exist. Retire wrappers individually after a native/scenario replacement reproduces their results and failure behavior.

**Acceptance.** The basic create/connect/execute/observe/close flow is discoverable in one profile. Existing conformance cases remain runnable outside the default menu. Adding another wallet does not require copying every funding, invoice, payment and recovery wrapper.

## Medium impact

### M1 — Consolidate action registration, request conversion and submission

**Evidence.** Tool profile membership and capability requirements are separate string-based lists. Actions have MCP requests, `OperationKind`, Kubernetes `LabAction`, runtime rendering and repeated submission code. Delegated admission additionally inspects public request JSON pointers in the core model. These copies create coordinated edit points. See [profile list](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:2169), [capability list](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:7587), [balance submission](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:5062), and [delegation matching](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/experiment.rs:69).

**Change — DRY policy data and mechanics.** Use typed operation descriptors for capability, profile exposure and shared scope. Normalize requests once before semantic admission; let delegation match typed fields rather than transport-shaped JSON. Extract the existing repeated record/submit/status transition into a small helper. Keep operation-specific validation and separate wire projections where Kubernetes schema constraints require them. Independent controller validation remains necessary at the trust boundary; reuse pure validators rather than deleting those checks.

**Acceptance.** Adding a supported operation requires one metadata registration; missing renderers or capabilities fail contract tests. Preserve actual MCP schema tests: the [private-transfer failure](/Users/admin/Sites/corpus/proofstorm/docs/private-ecash-agent-checkpoint-2026-09-05.md:17) demonstrated that malformed public contracts consume real agent time. Its schema defect was subsequently repaired, not an outstanding finding here.

### M2 — Make configuration, permissions and limits predictable

**Evidence.** Omitting `PROOFSTORM_DB` silently selects a limited in-memory service. Supplying it requires several additional variables and replaces durable grants on startup. MCP selects the current Kubernetes client context; k3d configuration switches that context. Namespace quotas, container defaults, eight active operations, four active prober labs and node-local placement are spread through code. The lab model can allow 64 components while the namespace permits only 12 PVCs; actual feasibility depends on component resource needs. See [startup](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/main.rs:36), [quotas/defaults](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/render.rs:51), [operation ceiling](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-store/src/lib.rs:28), and [scheduler limits](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/scheduler.rs:3).

**Change — mold one resolved environment configuration.** Make durable local operation the normal setup and memory/demo behavior explicit. Display the database path, runtime context, granted role and effective limits. Separate intentional permission updates from ordinary connection startup. Resolve internal catalog metadata without requiring callers to possess a discovery permission merely to run an already-authorized balance observation; the [first handoff gate](/Users/admin/Sites/corpus/proofstorm/docs/private-ecash-cross-principal-checkpoint-20260906.md:28) failed at exactly that dependency.

Publish effective limits and estimated workload demand during planning, preserving capacity for jobs/probes. Reject known quota impossibilities before materialization. Keep the bounded scheduler initially; make waiting visible and only replace it if measurements justify a simpler mechanism. Retain single-node labs until storage and execution contracts actually support distribution.

**Acceptance.** Restarting a client preserves lab history and does not unexpectedly alter permissions or cluster selection. Unsupported capacity fails with required-versus-available values. Permission errors name the requested operation and missing authority rather than a hidden metadata lookup.

### M3 — Organize implementation around components and lifecycle responsibilities

**Evidence.** Large files mix distinct work: the component adapter is 5,379 lines, operation renderer 5,313, and controller main 4,004, including tests. Backend configuration already has a useful central contract, but implementation-specific metadata/rendering remains scattered across registry, catalog and renderer matches. See [backend contract](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-core/src/backend.rs:238) and [renderer registration](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/adapter.rs:54).

**Change — mold modules, not a framework.** Group component packaging/configuration and protocol adapters into predictable modules. Separate lab, action and build reconciliation; move tests alongside the responsibility they exercise. Keep the existing pure plan compiler and common workload/storage/security helpers. Avoid a new trait hierarchy, dynamic plugin loader, separate controller per component, or generic schema language merely to reduce file length. Move candidate-building code behind an optional integration boundary if it obstructs basic deployment; PR testing itself is relevant developer functionality and should not be discarded.

**Acceptance.** A component's configuration, endpoints and rendering have obvious ownership. A small adapter change no longer requires navigating unrelated authentication, wallet and build workflows. Behavior and generated contracts remain unchanged during the extraction.

### M4 — Separate product onboarding from campaign machinery and historical reports

**Evidence.** The README has 613 lines with only a quick-start and legacy-harness section heading, interleaving many historical slices and checkpoints. Campaign tooling spans shell, Python, Rust acceptance gates and several preparation scripts. `make test` runs Rust tests but not the Python or Linux supervisor contracts; `make e2e` runs a selected gate list. These are different checks with different prerequisites. See [README](/Users/admin/Sites/corpus/proofstorm/README.md:9), [Makefile checks](/Users/admin/Sites/corpus/proofstorm/Makefile:73), and [gate selection](/Users/admin/Sites/corpus/proofstorm/Makefile:33).

**Change — cut onboarding history; DRY repeated harness transport only.** Keep the README to the product promise, one working example, connection instructions, inspection, cleanup and limits. Put capability support and compatibility in a current reference; retain checkpoint reports as historical evidence. Share repeated MCP session/wait/cleanup primitives in the benchmark harness, but leave financial assertions independently expressed. Document named verification tiers and one discoverable way to run each tier with explicit skips/prerequisites. Do not rewrite all harnesses into one language for aesthetics.

**Acceptance.** A developer can complete the normal path from one page. A contributor can identify the checks their change requires. A successful quick test command cannot be mistaken for all live wallet and execution gates passing.

## Low impact

### L1 — Remove stale storage and duplicate public names carefully

The store still creates an `operations` table, but the reviewed current Rust code reads/writes journal operations through `actions`; repository search found no SQL consumers of the former. Also, `proofstorm_action_status` delegates directly to `proofstorm_operation_status`. See [old table](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-store/src/lib.rs:306) and [alias](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-mcp/src/lib.rs:6132).

Stop advertising redundant names in the default profile and choose one public term. Check historical database contents and downstream consumers before a migration stops creating or removes the legacy table. Preserve historical evidence; do not infer that every existing database is empty. Acceptance is old database compatibility plus one unambiguous public status operation.

### L2 — Remove security objects that confer no authority

Every lab renders an empty Role and a RoleBinding to its workload service account. Keep the service account and disabled token mounting, but the empty grant objects appear to add inventory and teardown work without permission. See [security spine](/Users/admin/Sites/corpus/proofstorm/crates/proofstorm-kube/src/render.rs:84).

Confirm no consumers, then omit the empty Role/RoleBinding and update inventory/cleanup expectations. Preserve restricted workloads and network isolation. Acceptance is unchanged workload behavior and no additional Kubernetes permissions. This is a small cleanup, not a reason to redesign RBAC.

### L3 — Give the legacy Compose harness a retirement boundary

The old harness is already separated, and its documentation explicitly retains it until attack scenarios have Kubernetes equivalents. See [legacy rationale](/Users/admin/Sites/corpus/proofstorm/docs/compose-harness.md:3). Keep it out of the normal product path; inventory the unique regression scenarios, port or archive them deliberately, then remove the duplicate lifecycle entrypoint when its remaining purpose disappears. Replacing Kubernetes with Compose would be a separate product decision, not an architectural cleanup established by this review.

## What should survive the simplification

- Immutable component images and resolved configuration: these make a lab reproducible.
- Durable operation identity, bounded execution, cancellation and verified teardown: these let callers recover from disconnects.
- Explicit unknown outcomes and independent payment observations: command success does not prove settlement.
- Private payload custody, one-consumer/replay fences and scoped handoff: these prevent large or spendable payloads from becoming public tool output.
- Real component adapters and portable observations: these let developers use the actual software being tested.

The [reliable execution report](/Users/admin/Sites/corpus/proofstorm/docs/reliable-native-execution.md:3) ties supervision and output handling to real fuzz findings: missing durable cleanup/exit evidence, a leaked payment preimage and an unenforced cleanup boundary. These safeguards have substantially stronger evidence than the original rationale for manually managed lab leases.

## Recommended order and product acceptance

1. Simplify the local start/inspect/finish lifecycle and expose resolved configuration. Correct the broad readiness gate. Keep existing wire contracts as compatibility paths during the transition.
2. Extract shared lifecycle/observation modules while adding a narrow read-only environment snapshot. Preserve current state owners and resumable synchronization; do not introduce a new database or event bus.
3. Prove connection export with one external application, a mint and an authenticated node endpoint. Feed endpoint metadata and recorded activity into visualization. Application traffic may remain unobserved until collectors exist.
4. Prune default tools and move scenario orchestration out of MCP. Consolidate registration/submission, then remove verified dead structures.

The decisive acceptance test is a developer who can **start a lab, connect an ordinary app, reproduce a failure, inspect the evidence, and close the lab** without learning Kubernetes, manually managing leases, or using an AI agent. An agent should be able to use that same product through MCP. Further wallet, payment-processor and advanced delegation work should be prioritized by whether it enables that path, rather than by increasing the number of supported tools.
