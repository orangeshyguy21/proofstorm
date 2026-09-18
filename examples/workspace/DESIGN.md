# Workspace task foundation

The workspace owns a persistent directory and a supervisor running as its main
container process. Agent requests use the existing bounded native executor to
send small commands to a Unix socket. The supervisor starts task processes;
they are not descendants of the request process, so native command cleanup does
not terminate them. A read-only copy of the supervisor is installed from the
verified controller image before workspace startup.

## Implemented contract

* Persistent files, optional pinned runtime image and one configurable service port.
* `workspace_file` and `workspace_task` agent tools, using existing execution
  authority and short recorded operations.
* Source snapshots and command digests; exact task-ID retries; no automatic replay.
* Per-task supervisor with the native executor's Linux descendant cleanup.
* Optional deadlines, asynchronous stop, bounded rotating logs and paged reads.
* Workspace restart marks uncertain tasks interrupted; graceful shutdown cancels.
* Recreate deployment strategy and a volume lock prevent concurrent supervisors.

The default remains the existing pinned shell image. Custom runtimes select an
immutable image in the cell lock, and only workspace components admit those
custom images. Other components retain their shipped/candidate restrictions.

## Cases covered

The mining example exercises a continuing loop and an editable shared pause
file. A concurrent recorder exercises finite work and durable results. A fake
HTTP service exercises a long-lived listener and cell-local service discovery.
Process tests exercise duplicate requests, frozen source, exit codes, deadlines,
log rotation, restart without replay, cancellation and descendant cleanup.

## Scoped native control bridge

An optional `control` scope on task start selects target components, a call
budget and a per-command deadline. The controller binds it to the initiating
operation, principal, cell incarnation and revision. The task receives a local
helper; the workspace receives no cluster credentials or control-plane route.

The helper submits an immutable call ID and command to the workspace supervisor.
The controller polls this durable mailbox and creates a deterministic child
native action. A persisted claim precedes creation. A lost creation response is
reconciled by reading that action; a missing action after a claim is explicitly
uncertain and is never recreated. The existing native runner independently
fences process starts and collects descendant-cleanup receipts.

Commands are dispatched one at a time per task. Agent disconnects do not affect
dispatch. Task exit, stop, interruption or transport failure cancels outstanding
commands. A replaced workspace pod or changed cell revision closes the grant;
the controller can still collect receipts into the retained workspace volume.
Retries of task start keep the first owner; child actions cannot issue new grants.
Tasks within one workspace still share a trust boundary. Do not modify `.proofstorm`.

Child actions retain controller records, and results are copied to the workspace
output directory. They are cell-owned, not local database operations. Explicit
captures attach their evidence to a run. Their `action_id` is not an `operation_wait` ID.

## Lifecycle controls and leased network faults

Task grants now name lifecycle targets and exact network pairs separately from
native command targets. The initiating principal must have the corresponding
capabilities; the accepted action carries a digest of this authorized scope.
Raw task-start execution passes the same checks. Typed child actions retain the
original authority, revision and sequence. Sequential lifecycle calls from one
task share that origin; newer user controls on a component take precedence.
Lifecycle effects are intentional state changes and are not rolled back on exit.

A partition child carries a bounded expiry and remains reconcilable after it
succeeds. Running and succeeded leases contribute to the union of active network
partitions. Pending calls never activate policies. Explicit heal, task exit,
missing owner, authority closure and expiry durably mark a lease released before
policy changes. Healing removes only that lease, preserving overlapping task and
ordinary partitions. The controller does not need the workspace to heal. A
controller/API outage delays cleanup; the network data plane has no expiry timer.

All network-policy writers serialize locally, read policy resource versions,
then read the latest action journal. Updates carry those versions; initial
creation refuses to overwrite an existing policy. This order prevents delayed
writes from an old controller from restoring a partition after a newer cleanup
has completed. A partial failure leaves the release marker and retries the union.
Cleanup is recorded only after all policy writes succeed.

Claims record pending cleanup in task state before dispatch. The bridge publishes
the remaining fault count with a timestamp and the observed task phase. Process
cleanup and network cleanup are separate facts; terminal tasks need a matching
terminal observation with no pending faults. Call receipts remain immutable;
later release evidence belongs to the controller action and cleanup observation.

## Explicit evidence captures

`workspace_capture` selects one task and exact output paths for an open run in
the same cell. It captures submitted source, full task inputs, task state and
control mailbox records, plus optional retained logs. New tasks preserve an
input copy separately from their working directory; older tasks must still match
their original source digest. File bodies use base64 with individual hashes and
modes, and the complete capture has a deterministic digest.

The supervisor freezes a bounded transfer file under a capture ID bound to the
full host request digest. Retries reuse it after a lost connection or workspace
restart. The host verifies file hashes, source digest, cell incarnation/revision
and pod identity, then reads current controller records for the captured calls.
Missing claimed child actions remain explicitly unknown. File and controller
observations have separate timestamps; there is no distributed checkpoint claim.

SQLite commits the complete capture to an open run in one transaction. Run
closure shares that write lock, so a late capture cannot modify sealed evidence.
An exact retry returns the first stored capture even if live state has changed.
The transfer copy is released only after the local commit. Limits refuse whole
captures instead of truncating selected bodies. There is no task cancellation or
new run-owned execution action, so continuing tasks do not block finalization.

Exports always include the run's attached captures and their configuration
revisions. Bulk bodies remain in the evidence resource; a new bounded section
supports JSON-pointer reads by capture ID. Empty capture arrays are omitted to
preserve existing bundle digests. Export is independent of the live cluster, but
the existing cell purge still removes local history: download evidence first.

## Next layer

Payment replenishment must reconcile uncertain outcomes before retrying.
Generic process restart cannot provide exactly-once application effects.

Captures are explicit observations. Policies for periodic capture, selecting
completed output sets, and external archive retention can build on this contract
without tying background task lifetime to experiment completion.

See [usage and examples](README.md).
