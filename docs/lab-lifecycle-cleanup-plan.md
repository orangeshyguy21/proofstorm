# Lab lifecycle and cleanup

Status: implemented and validated locally. See [usage and agent contracts](lab-lifecycle.md).

Validation: 28 application/HTTP tests, 59 MCP tests, 5 stdio tests, 48 core tests,
3 cleanup tests, 4 update tests, and 10 store acceptance tests passed. Strict scoped
Clippy and formatting passed. A disposable live MCP/HTTP exercise verified normal
close, complete record purge, changed-topology name reuse, refusal of stale apply
and close requests, and external-deletion recovery. Its final database and HTTP
inventory were empty. Evidence: `/var/folders/w7/t4mxkw6n48vd2dwyvdgrn73w0000gn/T/proofstorm-lifecycle-17k2tx5r/result.json`.

Implementation uses a single crash-releasing SQLite sidecar guard per database,
not a new agent lease system. Cluster identity is bound to context/control namespace
and the kube-system UID. Live edits retain their keys; replacement labs receive
fresh keys. Advanced materialization and close contracts now require their plan or
incarnation identity. The sections below record the implementation design.

Product promise: **If a lab is gone, its name is available and its local data is gone.**
Kubernetes owns deployed lab existence. SQLite holds creation intent, the desired
configuration of live labs, and their activity. It must not keep deleted labs alive.

## Confirmed problem

- The current cluster has no ProofstormLab resources, but SQLite still contains
  the old `local-lab/thunder-dome` instance.
- `proofstorm-app/src/environment.rs` filters against current cluster inventory,
  so the website correctly excludes it.
- `proofstorm-store/src/lib.rs::materialize` checks the retained instance before
  the MCP path checks Kubernetes. A different configuration produces a conflict.
- Named CLI labs have a second reservation path in
  `proofstorm-store/src/labs.rs::reserve_lab`.
- Close paths intentionally preserve journal rows. There is no purge operation.
- The existing background observer skips activity belonging to absent labs. It
  does not reconcile their lifecycle or release their names.

## 1. One shared lifecycle service and explicit identity

Put creation, absence reconciliation, and final cleanup in proofstorm-app.
CLI and MCP must call the same service; their store-first creation paths must not
bypass it. Keep HTTP GET and SSE passive.

Bind runtime tracking to the actual cluster identity, workspace, and lab
incarnation. Use a stable cluster identifier such as the kube-system namespace
UID, not just a kubeconfig context name or API address. Record the ProofstormLab
UID once created. An edit keeps the incarnation; recreation gets a new one.
The human-readable name can be reused immediately after verified cleanup.

Keep a small internal creation state: accepted creation intent versus a lab that
has existed in Kubernetes. Persist the creation token before applying and record
the returned UID. This closes the crash window between Kubernetes creation and
the database commit: recovery can recognize the resource by its creation token.
Do not introduce user-visible leases, expiration periods, or another setup step.

Serialize creation and cleanup for the same cluster/workspace/name across CLI,
MCP, and web processes. Use a process-safe lifecycle guard with automatic release
on process death, plus transactional version checks; an in-process mutex alone
is insufficient. All local creation entry points must participate.

Protect updates, operations, close requests, and cleanup with incarnation checks.
Use Kubernetes UID/resourceVersion preconditions for mutations. Scope execution
identities to the incarnation so an old action cannot be attached to a replacement
lab merely because the name and desired generation match.

## 2. Reconcile before deciding a name is occupied

For a creation request or a conflicting stored name:

1. Acquire the lifecycle guard and read the current cluster identity.
2. Read the exact lab resource and tracked namespace, not just cached inventory.
3. If the matching lab exists, preserve it and use the existing retry/edit rules.
4. If creation is in progress, resolve the accepted intent or report its actual
   failure. A pod that is Pending, blocked, or pulling an image is still a live lab.
5. If a previously materialized lab is absent and its namespace is absent, purge
   the old incarnation and continue the new creation in the same call.
6. If resources remain, report cleanup pending with the namespace/resource that
   prevents completion. Do not silently forget resources or treat them as ready.
7. If any required read fails, preserve state and report cluster unavailable.

Never let a periodic list snapshot alone authorize deletion: re-read under the
guard before the database transaction. Distinguish an unstarted/interrupted
creation from deletion of a lab that previously existed. Recover accepted creation
only in its original cluster; a rebuilt cluster invalidates that intent rather
than silently deploying an old requested lab into the new cluster.

Use the same rules for raw MCP instance IDs and named developer labs. There should
be no requirement to invent a different name, close a nonexistent lab, create an
experiment, or manually edit SQLite to proceed.

## 3. Purge the complete lab-owned record set

Implement one atomic, incarnation-checked store purge. Inventory all references,
including JSON references and legacy tables, before writing the deletion code.
Delete in foreign-key-safe order:

- Lab handles, instance records, creation state, close state, update state,
  retained-component records, and lab-specific update plans.
- Experiments, sessions, actions, operation revision links, observations,
  payment claims, private access grants, receipts, and internal artifacts.
- Lab-specific retry/idempotency responses and consumed creation plans, so an
  old receipt cannot return a deleted lab or reserve its name.
- Lab-owned revision snapshots and other control-namespace resources after
  ownership/UID checks and verified runtime teardown.

Delete unreferenced generated revisions and associated internal blobs. Preserve
workspace/principal configuration, the catalog, reusable unconsumed authoring
drafts, shared revisions still referenced elsewhere, and explicitly exported files.
These are independent objects, not a hidden archive of deleted labs.

Creation plans/retries must be bound to cluster and creation identity. Once the
associated plan is consumed and purged, an old apply must return plan-not-found
and require a fresh plan; it must not recreate the old lab. Audit the lower-level
materialize entry point too: it must not bypass this rule by replaying a globally
retained revision. This does not add a user step to the usual plan/apply workflow.

Cleanup is idempotent and recoverable. Database deletion is atomic; any owned
external artifact deletion is a separate retryable step with explicit ownership.
Do not retain a permanent tombstone or journal merely to reserve an old name.

## 4. Run cleanup at the right points

- **Explicit close:** once the lab and namespace are verified absent, complete
  cleanup before returning successful teardown. Return a final receipt from the
  completed operation rather than looking up an instance that has been purged.
- **Startup and periodic observation:** run bounded reconciliation in the existing
  background loop, including headless MCP usage. Do not require the website to be
  open. Reuse one shared implementation even when multiple processes run it.
- **Creation:** reconcile the requested name synchronously before rejecting it.
  This is the immediate correctness path even if no background pass has run.
- **Cluster rebuild:** after successfully identifying the replacement cluster,
  reconcile records bound to the replaced cluster. Do not confuse selecting a
  different, still-existing cluster with rebuilding this one or purge its records.

Keep the current one-second observer cadence as a scheduling target, with bounded
pages and request timeouts. Do not promise that Kubernetes teardown completes in
one second. No retention timer or age-based inference of deletion.

## 5. Make CLI, MCP, and GUI agree

Define shared machine-readable outcomes for lab-not-found, creation-in-progress,
cleanup-pending, cluster-unavailable, and stale-incarnation. Include concrete
recovery guidance. A database record alone is never evidence that a lab is running.

Keep the website focused on the current cluster. Surface pending cleanup or failed
reconciliation through observer health; do not silently display success while
cleanup repeatedly fails. Database changes should invalidate the existing SSE
snapshot. When a selected lab disappears, clear its detail view normally.

Preserve close/wait usability: a wait already tracking an incarnation can return
verified absence after purge. An unrelated lookup for an unknown name returns
not-found. Repeating close for a verified absent target is harmless and must never
close a newly created incarnation. Update schemas, MCP descriptions, and docs,
including the requirement to export desired evidence before closing a lab.

## 6. Alpha reset policy

Proofstorm is unreleased. Do not migrate historical databases or retain legacy
schema compatibility. Reset disposable local state when a breaking alpha schema
change requires it. Initialize the current schema directly and test against it.
The existing local data was explicitly authorized for deletion for this change.

## Validation and delivery

Implement in this order: identity and purge transaction; shared lifecycle and
creation integration; close/background reconciliation; transport contracts and
fresh-state initialization; live validation.

Required automated cases:

1. Create, close, verify all lab-owned rows disappear, then reuse the same name
   with a different topology through both CLI and MCP.
2. External deletion and cluster rebuild: GUI and agent agree; a new apply can
   reclaim the name without waiting for background cleanup.
3. Timeouts, 403s, API failures, partial reads, and namespace termination retain
   records and produce accurate diagnostics.
4. Two creators, creation versus cleanup, close versus recreation, and two cleanup
   workers cannot erase live state or admit work to the wrong incarnation.
5. Crashes before/after Kubernetes creation and before/after store commits recover
   without permanent reservations or resurrecting deleted labs.
6. Old apply, edit, operation, and close retries cannot affect a same-name new lab.
7. Shared configuration and exports survive; lab-specific history, grants, and
   retry records do not. Assert foreign-key integrity after purge.
8. Fresh databases initialize the complete current schema. Switching cluster
   contexts does not erase another cluster's labs.

Run scoped store/app/MCP/HTTP contract tests, formatting, and strict Clippy. Then
use a disposable live lab to demonstrate create → delete → disappearance → same-
name recreation and external-deletion recovery. Test cluster-identity replacement
with mocked runtime identities; do not destroy the user's cluster to exercise it.

Done means: a confirmed-deleted lab leaves no lab-owned local records or reserved
name, and an agent can immediately create its replacement without a workaround.
