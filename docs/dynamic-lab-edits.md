# Edit a running lab

Start a lab, grow it, and keep working without losing its state.

## CLI

Keep the same lab name and pass the complete updated configuration:

```sh
proofstorm up lab.json --name demo --preview
proofstorm up lab.json --name demo
```

An unchanged configuration is a no-op. Changed configuration updates the existing
namespace. The preview lists additions, targeted restarts, removals, retained-data
options, required images, and unsupported changes. `up` waits for convergence;
status and the website show progress while it runs.

Removing a component retains its PVCs and generated credentials. To explicitly
delete data with a removal, use `--delete-data`. To release previously retained
data, use the current lab configuration and `--delete-retained old-node,old-mint`.
These options also work with `--preview`. A retained component ID cannot be reused
until its old data has been explicitly deleted. Closing the lab removes its entire
namespace, including retained data.

## MCP

1. `proofstorm_lab_read({"instance_id":"demo"})` returns the full desired lab.
   Its `version` is the desired configuration generation.
2. Call `proofstorm_lab_plan` with a new `plan_id`, the complete desired components,
   connections and policy, plus:

   ```json
   {"update":{"instance_id":"demo","expected_generation":1}}
   ```

   Preserve component IDs, explicit versions, configuration, and all existing
   connections from the read result. The read result uses the canonical lab schema;
   the planner uses its existing component and connection input schema. Never
   reconstruct a lab from a paginated visualization response.
3. Review `update.changes`. Apply with the existing `proofstorm_lab_apply`, using
   the returned `plan_id`, `plan_digest`, target instance, and an idempotency key.
4. `proofstorm_lab_wait` accepts `expected_generation`. It returns `superseded`
   when a newer edit replaces the target; startup blockers end a ready wait early.

Apply records durable intent before contacting Kubernetes. An interrupted apply
can be retried with the same request. Running MCP clients and `proofstorm serve`
resume accepted intent after restart. An old retry reports its original target
and the current generation; it never reinstalls an old configuration. A stale new
plan returns `lab_update_conflict` and requires a fresh read and plan.

`generation` is desired configuration; `observed_generation` identifies the
configuration processed by the controller. Ready additionally requires observed
resources to satisfy that configuration. The website's runtime observation also
retains Kubernetes resource generations separately. `last_converged_revision`
records the last fully ready revision.

## What changes safely

- Add components and links in the existing namespace. Unchanged component pods,
  volumes, generated credentials, and Service addresses remain stable.
- Compatible configuration changes restart only components whose rollout contract
  changes, including affected dependents. Attached clients may need to reconnect
  to those listed components.
- Removing components prunes recorded controller resources. Resources installed by
  attached applications are not part of that inventory.
- Existing operations keep their admission revision. Unrelated additions do not
  stop them. Restart/removal conflicts return the affected active operation IDs;
  finish or cancel those operations and replan. Logs remain available during edits.
- Missing images and other startup failures leave unchanged components usable.
  Catalog validation is not a node image-pull verification. `make doctor` checks
  the installed catalog's pullability; real startup conditions remain authoritative.

Backend/image replacement, database relinking, and unverified state migrations are
rejected before acceptance. Renaming a component is removal plus addition.
Namespace allowances grow with the lab and retained volumes; actual node capacity
can still prevent scheduling and is reported through startup conditions.

Sessions remain passive activity tracking. HTTP and SSE remain observation APIs;
lab changes use the CLI or MCP.

## Deployment and validation

Upgrade binaries, CRDs, controller, and chart permissions together using the normal
build/deploy targets. Restart running MCP clients to load the new tool contracts.
An old MCP process is not safe to use for editing an updated lab.

`make e2e-dynamic-lab` uses its own database and disposable funded lab. It checks
stable pod/PVC/Secret/Service identities, Lightning channel identity, wallet balance,
a persistent external HTTP connection, targeted restart, retained storage, explicit
purge, unrelated application resources, and an intentionally unavailable image.
Evidence is written to `dev/dynamic-lab-runs/<run-id>/`. It closes only its own lab.
