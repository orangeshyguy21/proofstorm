# Lab existence and cleanup

The cluster determines which labs exist. The local database tracks creation,
configuration, and activity while a lab is in use. It does not archive deleted labs.

Cleanup runs after verified teardown, during background reconciliation in the web
server and configured MCP server, and before rejecting an occupied name during
creation. Once the lab resource and namespace are absent, cleanup removes the
lab's handles, activity, grants, consumed plans, retry records, and unreferenced
revisions. Explicitly exported files and independent workspace configuration remain.

A connection failure never means deletion. A remaining namespace reports
`cleanup_unverified`. A creation that has not reached Kubernetes can be retried;
rebuilding its cluster invalidates the old creation intent. Labs from another
configured cluster context are preserved.

Export evidence before closing a lab. After successful cleanup, its name can be
used for any supported topology. Creation is serialized with cleanup across local
processes; this coordination has no lease or expiry timer.

## Agent contract

The normal `lab_plan` → `lab_apply` workflow remains unchanged. A new lab incarnation
gets a fresh `instance_key`; editing a live lab preserves it.

- `lab_status` returns `instance_key`. Pass it as `expected_instance_key` to
  `lab_close`, and to `lab_wait` when verifying closure. That wait can verify
  absence even after the background task has removed local records.
- `lab_finish` takes `expected_instance_id` from the named lab inspection result.
- The advanced `lab_materialize` tool requires the published `plan_id` as well as
  its revision. A consumed, deleted plan cannot be replayed to recreate a lab.
- Update plans bind to the incarnation as well as the desired generation. Re-plan
  after a lab is deleted and recreated.

Restart the web server with `make serve` and reconnect existing MCP sessions after
upgrading to these contracts. Old binaries do not participate in the new lifecycle
coordination. The controller and existing lab workloads do not need rebuilding.

## Alpha storage policy

Proofstorm is unreleased. The store initializes only the current schema; it does
not migrate historical alpha databases. Breaking alpha schema changes use an
explicit local data reset rather than carrying compatibility code.
