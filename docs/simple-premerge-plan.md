# Final simplification pass before merging `simple`

Status: implemented and validated. Grounded in
branch `simple` at `934bb23` on 2026-09-07. The sections below record the agreed
scope; the execution record at the end describes validation.

Product promise: **Start a Bitcoin, Lightning, or Cashu lab, connect your app,
run commands, and see what happened.**

## Scope

Finish four bounded changes: consistent environment selection, automatic run
context for ordinary agent operations, accurate current documentation, and
removal of unused storage/RBAC objects. Keep the existing Rust crates and shared
application layer. No database migrations or historical compatibility paths.

Defer canonical read/plan schema unification, wholesale MCP decomposition,
tool renaming/profile consolidation, new connection protocols, and retirement
of advanced evidence/scenario workflows. These are separate changes, not merge
requirements for this pass.

## 1. Resolve the same environment in CLI and MCP

### Current evidence

- `proofstorm-app/src/main.rs::Args` defaults to a durable database, workspace
  `local-lab`, context `k3d-proofstorm`, namespace `proofstorm-system`, and CLI
  principal `developer`. It constructs Kubernetes configuration with an explicit
  context and subsequently overwrites `Runtime.cluster_source`.
- `proofstorm-mcp/src/main.rs::configured_service` silently creates the legacy
  in-memory service if `PROOFSTORM_DB` is absent. With a database it requires
  workspace/principal/capabilities, replaces grants on startup, and silently
  remains disconnected if `PROOFSTORM_CONTROL_NAMESPACE` is absent.
- Connected MCP uses `kube::Client::try_default()`. `Runtime::new` independently
  reads the current kubeconfig context. Neither pins the CLI's selected context.
- Stdio tests deliberately exercise unconfigured and store-only operation.
  Those modes have legitimate test uses but must become explicit.

### Change

Add a small shared environment module in `proofstorm-app`. Resolve database,
workspace, principal, context, namespace, and runtime mode once. CLI flags and
MCP environment variables are adapters to that type, not separate implementations.

- Shared environment defaults: `.proofstorm/proofstorm.sqlite3`, `local-lab`,
  `k3d-proofstorm`, `proofstorm-system`. Resolve relative database paths against
  the process working directory and report the absolute path.
- Precedence: explicit CLI flag, environment variable, shared default. Add
  `PROOFSTORM_CONTEXT`; retain existing database/workspace/namespace variables.
- Keep actor identity explicit: CLI retains `developer`; MCP requires
  `PROOFSTORM_PRINCIPAL`. Never silently run an agent as the human developer.
- Connected operation is the MCP default. Explicit `PROOFSTORM_MODE=offline`
  selects durable store-only operation; `PROOFSTORM_MODE=memory` selects the
  limited ephemeral discovery/test service. Update tests to request those modes.
- Build the Kubernetes client and lifecycle source identity from the same
  resolved context. Do not mutate the user's current kubeconfig context.
- Missing context, invalid mode, missing principal, and unusable storage return
  actionable startup errors. A connection error never falls back to memory or
  to a different cluster. Offline/memory discovery must not advertise executable
  runtime operations that lack a runtime.
- Emit resolved non-secret configuration to stderr, preserving MCP stdout for
  JSON-RPC. Include mode, database, workspace, principal, context and namespace.
- Preserve explicit permission provisioning: CLI `init`/`make serve` and an
  explicitly supplied operator capability list may set grants. If MCP has no
  capability list, use stored grants; do not silently grant a role. Report an
  unconfigured identity with a concrete setup instruction.

Update the three OpenCode profiles, acceptance client/gate startup, doctor, and
stdio fixtures together. Test environment resolution without global environment
mutation by passing captured values into the resolver.

### Acceptance

CLI and MCP resolve the same storage/cluster given the same environment;
principal identities remain distinct. Changing the ambient kubeconfig context
does not redirect default Proofstorm operations. Explicit overrides work.
Offline/memory tests need no cluster. Missing configuration fails visibly;
existing capability filtering and operator-defined grant behavior still work.

## 2. Make ordinary agent operations independent of experiment setup

### Current evidence

- `proofstorm-app/src/lab.rs::ensure_run` automatically creates a named lab's
  run and tracks a session. `LabHandle::run_id` derives it from the instance ID.
- OpenCode profiles select `native`, whose commands use the advanced handlers,
  not `lab_exec`. Merely switching to `developer` would remove planning,
  candidate-build and native/private capabilities used by the current workflow.
- MCP `ComponentLogsRequest`, `ComponentExecLiveRequest`, and other operation
  inputs require an `experiment_id`. Their handlers pass it to
  `ProofstormMcp::create_operation` and the store's admission path.
- `Store::create_operation_inner` looks up that experiment before automatic
  session tracking. Its retry envelope includes the requested experiment and
  session. A superficial optional schema field does not fix admission or retries.
- Experiments still group action sequences, evidence and quote observations.
  Sessions already track the caller automatically. Private access is separately
  authorized. Deleting these models would exceed this pass.

### Change

Introduce one shared default-run resolver backed by store admission, usable for
both named CLI labs and raw MCP instances. Ordinary native-profile operation
requests may omit `experiment_id` and `session_id`.

- Scope an implicit run to workspace, lab incarnation and principal. Two agents
  must not collide on experiment ownership; reconnecting the same actor resumes
  its run and records its new session. Live edits retain the run, recreation does
  not. Use a deterministic, valid identifier and atomic insert/validation.
- Resolve only after validating the requested operation and its existing
  authority. Creating internal bookkeeping must not require the caller to gain
  `experiment.create`, bypass a private grant, or add a general capability.
- Keep explicitly supplied experiment IDs supported. Invalid, wrong-lab or
  closed explicit experiments retain their errors; do not silently substitute
  a default. A closed implicit run also returns an actionable error, rather than
  silently reopening it or creating an endless succession of runs.
- Apply omission consistently to the ordinary native mutation/observation
  paths: live exec, logs, forensics, component restart, private transfer,
  partition/heal and reachability. Audit shared request structs so other handlers
  using them follow the same resolver instead of accidentally accepting an empty
  experiment they cannot execute. Advanced experiment/evidence/grant-management
  requests keep explicit grouping where identifying that object is the request.
- Normalize optional run context once before admission fingerprints are built.
  Pin the resolved run/session in the recorded operation. Retry lookup must
  reuse the original attribution across reconnects, finished sessions and edits,
  and must not create bookkeeping just to return an existing operation.
- Preserve caller operation IDs and idempotency keys, immutable admission
  revisions, stale-incarnation checks, payment claims and private-payload fences.
  Do not combine retry identities as an unrelated contract change.
- Route CLI run creation through the same resolver. Update inspect, sync and
  close paths that currently assume `LabHandle::run_id()`. Lab close must still
  account for all actors' outstanding work and purge all lab-owned runs/sessions.
- Return actual run/session IDs in receipts for optional evidence workflows.
  Change MCP instructions and tool descriptions so an ordinary agent can
  plan/apply, inspect startup, fetch logs, execute, and close without setup calls.

### Acceptance

Use raw MCP JSON in contract tests; the test client must not inject experiment or
session IDs. Prove logs and exec work without them, including logs on an unready
component. Cover reconnect/retry, two concurrent creators for one default run,
two principals in one lab, explicit closed/wrong-lab runs, denied operation/private
access, edits, closing, and same-name recreation. Check schema required fields
as well as handler behavior. Retain private-transfer/handoff regression coverage.

## 3. Remove the dead objects

### Current evidence and changes

- `proofstorm-store/src/lib.rs` creates `operations`, while active journal SQL
  uses `actions`. References to the old table remain in lifecycle purge code.
  Remove its creation and cleanup references; confirm repository SQL has no
  remaining consumers. Keep the public concept of an operation unchanged.
- `proofstorm-kube/src/render.rs` renders an empty workload Role and RoleBinding.
  `proofstormd/src/main.rs` applies them. Remove those fields, imports and apply
  calls, and update the rendering golden fixture. Remove the controller's
  Role/RoleBinding permissions from the Helm chart after the consumer audit.
  Keep the workload service account, disabled token mounting, and actual
  controller ClusterRole/ClusterRoleBinding.

Acceptance: fresh schema has no `operations` table; lifecycle tests still purge
all lab-owned records with foreign-key integrity. Render/controller tests show
unchanged workload isolation without empty RBAC objects. Existing empty objects
can disappear with their lab namespace; do not add upgrade cleanup machinery.

## 4. Publish one accurate current workflow

Correct README statements that configuration changes require close/recreate and
that result lookup survives teardown. Current behavior is live editing and
purging lab-owned activity after verified deletion; export evidence first.

Update startup/mode documentation, OpenCode examples, MCP server instructions,
agent demo prompts and session/lifecycle guides to match the contracts above.
Keep `make setup` for cluster provisioning and `make serve` for building,
initializing and serving. Preserve historical reports as clearly marked history;
current instructions must not send agents through obsolete experiment ceremony.

Acceptance: a reader following the documented ordinary path sees the same cluster
in MCP and the website, can diagnose a startup blocker immediately, and knows
exactly what survives live edits and what disappears on deletion.

## Delivery and validation

Implement environment resolution first, automatic run admission second, dead
objects third, then finish docs against the resulting interface.

Run focused resolver, store/admission and MCP stdio/schema tests while working.
At the end run `make test` and `make lint`, including Helm lint and rendering
fixtures. Build CLI, MCP and controller artifacts. Reuse the existing lifecycle
and dynamic-lab gates rather than building a new testing framework.

Live validation uses a unique disposable lab and isolated test database in the
explicit `k3d-proofstorm` context. Plan/apply through MCP with no experiment setup;
observe the same lab over HTTP; read startup logs; execute a native command;
reconnect and retry without duplicate execution; apply a small additive edit;
close, verify purge and reuse the name. Include a deliberately unavailable
component image to prove diagnosis remains available. Verify fresh lab inventory
contains no empty workload Role/RoleBinding. Record any failed or unrun check.

Do not erase current user labs or reset the main database merely to validate this
pass. Tests use fresh state. There is no new automatic data-reset behavior or
migration system. Build/deploy matching controller and chart changes together;
restart MCP clients when rolling out the changed contracts.

Done means the ordinary agent workflow requires lab configuration and command
requests, not knowledge of environment fallbacks or experiment bookkeeping.

## Execution record — 2026-09-07

Implemented shared environment resolution, explicit MCP modes, automatic native
run context (including wallet balance and partition/heal), actor/incarnation
scoping, pure observation of unmaterialized intent, and the storage/RBAC removals.
Named CLI operations now use the same run resolver and no longer require
experiment-creation authority. Current README and OpenCode instructions match.

- Full workspace tests: 319 passed, one existing ignored test. Final scoped
  application tests: 37 passed, including the added regression for inspecting
  reserved, unmaterialized labs. Strict workspace Clippy, formatting and Helm
  lint passed.
- Live validation: all seven checks passed. Native commands and logs ran without
  `experiment.create`; retries retained original attribution and results; live
  edits preserved pods, wallet state and run identity; an unavailable image
  produced an immediate blocker; cleanup purged lab-owned data; same-name
  recreation created a fresh run. The existing lab kept its resource identity.
- The rebuilt controller and matching Helm permissions were deployed to
  `k3d-proofstorm`. No empty workload Role/RoleBinding appeared in the test lab.
- Live evidence: `/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-premerge-62e2ww0p/result.json`.
  Synthetic missing-image input used the same private-catalog fixture technique
  as the existing dynamic-lab gate; no actual candidate source build was claimed.
- User database and labs were not reset. No migrations or compatibility shim were
  added. Restart MCP/OpenCode sessions to load the updated release binary.
