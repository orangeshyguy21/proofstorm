# Dynamic lab edits

Status: implemented; funded live preservation gate passed, 2026-09-06. See [usage and supported changes](dynamic-lab-edits.md). Validation uses disposable labs; the existing thunder-dome topology is unchanged.

Product promise: **Start a lab, grow it, and keep working without losing its state.**

A lab has a stable identity and an evolving desired configuration. Published
revisions remain immutable records of configuration. Editing a lab changes its
desired revision; it does not create another lab or namespace.

## Original foundation and gaps

- `proofstorm-store/src/lib.rs::materialize` derives infrastructure identity from
  the revision and rejects another revision for an existing instance.
- `proofstorm-store/src/labs.rs::reserve_lab` rejects changed configuration on an
  open named lab. The native and developer interfaces both need the same update path.
- `proofstorm-core/src/publication.rs` already computes per-component rollout
  digests and tests that unrelated changes leave them unchanged. Reuse this.
- `proofstormd/src/main.rs::apply` already patches resources in place. However,
  its fixed-name inventory ConfigMap is immutable, and it has no general removal
  reconciliation. Updating the store alone would therefore be insufficient.
- `proofstorm-store/src/lib.rs::operation_context` resolves the instance's current
  revision. Admitted operations need their own immutable configuration context.
- Existing HTTP environment reads and SSE can expose update progress. The web
  application remains an observer; this work does not require a web editor.

## User and agent contract

Keep the existing plan/apply workflow. Extend it to target an existing lab; do not
introduce another experiment, session, reservation, or approval workflow.

1. Read the lab's current desired configuration and generation.
2. Submit the complete desired configuration through the shared planner. A targeted
   edit must preserve omitted-by-pagination data; clients must obtain the full
   authoring configuration rather than rebuild it from a partial environment page.
3. Receive a bounded change summary: added components, unchanged components,
   restarted components and affected dependents, connections changed, removals,
   storage effects, image requirements, and any unsupported changes. Page detail.
4. Apply the exact plan against its expected base generation and revision.
5. Observe reconciliation through the existing status/wait tools and HTTP/SSE.

Extend `proofstorm_lab_plan` with an optional existing instance target and base
generation. Keep `proofstorm_lab_apply` as the apply operation. Plans bind their
target, base generation, resolved image locks, and change effects in their digest.
Extend named CLI `up` and developer `lab_up` to use the same service: changed
configuration updates an open lab; identical configuration is an idempotent no-op.
Expose a CLI preview using that same planner. Tool descriptions explicitly explain
that additive edits preserve existing state and list changes that require restart.

Do not add writes to the visualization HTTP API. It reports the same update
receipts and conditions that MCP and the CLI consume.

## Implementation sequence

### 1. Stable identity and revision history

Preserve existing `instance_key`, resource name, namespace, component IDs, Service
names, credentials, and PVC identities. Existing labs keep their stored keys even
though those keys were originally derived from a revision. No migration renames
infrastructure. For newly created labs, derive identity from the lab incarnation,
independently of configuration. Closing and explicitly recreating a lab still
creates a new incarnation.

Track a monotonic desired generation, desired revision/lock, observed generation,
and last fully converged revision. Record each accepted update with its base,
target, actor, request identity, and result using the existing durable journal
patterns. Observed generation means the controller has processed the request;
convergence additionally requires its desired effects to be satisfied.

Use one store transaction to compare the base generation, append the update
receipt, and advance desired configuration. A concurrent stale plan fails before
changing desired state. Exact retries return the original receipt, even after
later edits; never replay an older desired revision into Kubernetes.

Kubernetes application is resumable reconciliation, not an atomic transaction
with SQLite. Persist intent before applying; reconcile the latest accepted
generation with optimistic Kubernetes version checks. Recover interrupted
applications after process restart. Closing a lab wins over subsequent edits.

### 2. State-preserving expansion — first shipping milestone

Initially accept additions and connections whose computed effects do not mutate
existing component workloads or storage. Validate the complete resulting graph,
dependency compatibility, resource demand, and image requirements before accepting
an update. Reject unsupported changes with a precise reason and no partial apply.
Image checks report actual verification coverage; they must not claim runtime
pullability solely because an image appears in the catalog.

Reconcile additions into the existing namespace. Preserve unchanged pod templates,
PVCs, generated credentials, lifecycle state, Service addresses, and endpoint
identity. Component rollout digests, not the lab revision alone, govern restarts.
Shared helpers such as protocol probes and network policies may need updates;
include their effects and dependency impact in the plan.

Replace the immutable live inventory with an updateable controller-owned inventory
using resource versions, keeping previous inventory until reconciliation succeeds.
Retain immutable revision records in the store; do not duplicate full history into
an unbounded Kubernetes status field. Adoption must handle existing immutable
inventory ConfigMaps explicitly, replacing only that derived metadata object.

Reconcile from actual owned resources after restarts. An addition failing to pull
its image leaves existing components usable. No automatic teardown or rollback of
the whole lab. Readiness shows which components remain ready and why the new one
is blocked, using the startup diagnostics already implemented.

### 3. Configuration changes and reconnection

Classify changes using rendered resource effects, dependency contracts, and state
compatibility. Distinguish changes without a restart, changes requiring targeted
restarts, and unsupported/destructive changes. Include affected dependents rather
than looking only at the edited component.

Restart only affected workloads; preserve compatible storage and credentials.
Do not rotate mint keys or recreate wallet state as a side effect of an unrelated
edit. Keep endpoint contracts stable where possible; report address, credential,
or connection changes explicitly for applications attached to lab infrastructure.
Clients may need to reconnect after a listed restart; do not promise uninterrupted
connections to components being changed.

A matching state-contract identifier is necessary but not sufficient evidence that
every software upgrade or downgrade is safe. Initially reject backend replacement,
unverified data migrations, incompatible state changes, and immutable Kubernetes
field changes. Add explicit backend support as it is validated.

### 4. Removal and explicit reset

Add owned-resource pruning only after expansion and updates are reliable. Validate
that removed components have no remaining desired references. Prune using actual
ownership, component incarnation, and recorded inventory; never delete arbitrary
resources placed in the namespace by attached applications.

Removing a component retains its data by default and reports retained storage in
inventory and resource demand. Data deletion/reset requires an explicit option
bound to the reviewed plan digest. A rename is remove-plus-add, not a silent data
migration. Prevent accidental reuse of retained data with an incompatible backend;
require an explicit restore or discard decision when reusing an ID.

Teardown must include retained resources and partially completed edits. Reapplying
an older configuration is another validated edit, not a guarantee that software
data migrations or external effects can be reversed.

## Operations, concurrency, and observation

An operation records its admission revision/lock and relevant component rollout
and state identities atomically with admission. Dispatch and evidence use that
snapshot, never whichever revision happens to be current later. Legacy records
retain their original revision during migration.

Additions do not stop operations on unchanged components. For an edit that would
restart or remove a component with active work, reject the conflicting edit before
acceptance with the affected operation IDs; the actor can finish or cancel that
work and replan. Coordinate operation admission with accepted updates so a new
operation cannot race into a component already scheduled for disruption. Recovery
commands remain available when their actual prerequisites hold. Sessions stay
passive; session overlap is never an edit lock.

Expose desired versus observed generation, last converged revision, update progress,
per-component readiness, and bounded blockers. Suggested update outcomes are
`reconciling`, `blocked`, `converged`, and `superseded`; reuse existing receipt and
condition types where possible. A superseded wait returns explicitly rather than
waiting for a revision the controller is no longer pursuing.

HTTP and SSE distinguish desired additions from observed resources, keep unchanged
components visibly ready, and report partial failure accurately. Readiness must
not turn true using status from an older generation. MCP contracts explain accepted
versus converged and return actionable conflict/recovery codes.

## Validation and release

- Unit/contract tests: deterministic diffs; stable rollout digests; migration keeps
  existing identities; stale-plan rejection; idempotent replay after later updates;
  atomic operation snapshots; unsupported edits make no desired-state changes.
- Controller tests: unchanged pod templates; preserved generated secrets and PVCs;
  immutable-inventory adoption; crash recovery between journal/apply/status stages;
  failed addition; concurrent edits; edit/close races; superseded waits.
- API tests: MCP, HTTP, and SSE agree on revisions, blockers, partial readiness,
  completion, and bounded paginated change summaries. Regenerate schemas and CRDs.
- Live acceptance in a disposable funded lab: add Lightning nodes and a mint;
  prove existing pod/PVC UIDs, wallet keys, balances, channel identities, and Service
  endpoints survive. Existing components must remain usable during a deliberately
  failed image pull. An external client remains connected to an unchanged Service.
- Later gates: targeted restart, active-operation conflict, dependency relink,
  storage retention, explicit deletion, and complete teardown after partial edits.

Ship the first milestone through the existing CLI/MCP and live viewer, then expand
supported edits. Upgrade store migrations, controller/CRDs, CLI/MCP, and observer
together before applying an update to an existing lab. Validate compatibility on a
disposable lab first; use `thunder-dome` only after the preservation gate passes.

The first milestone is complete when an agent can add a node to a funded lab with
one plan/apply cycle, observe its real startup state, and keep using every unchanged
component without losing data or rebuilding the lab.

## Implemented validation evidence

The disposable gate `dev/dynamic-lab-runs/1788745813-82786` passed. It funded a
1,000 sat wallet and opened a Lightning channel, added a node and mint, restarted
only the edited node, retained removed storage, and explicitly purged that storage.
Existing pod/PVC/Secret/Service identities, wallet balance, channel identity, and a
persistent external HTTP connection survived. A deliberately missing image then
returned an attributed blocker with generation 6 observed, while all five original
components remained ready and wallet operations continued. The test lab was closed
and its namespace removed. `thunder-dome` retained its nine ready components.

Additional regression coverage checks stale generation conflicts across independent
SQLite connections, no-op/idempotent retries, original operation revisions, scoped
active-operation conflicts, closing admission, restart recovery of accepted intent,
and refusal to treat old status as completion of a data-only edit.
