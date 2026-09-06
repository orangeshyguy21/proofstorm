# Read-only environment API

Proofstorm exposes one shared view of labs, their topology, resource demand, sessions, and recorded activity. The CLI, MCP, and HTTP adapters call the same application code. Reading the view does not start jobs or sessions, open tunnels, collect receipts, or change lab state.

## Use it

```bash
# JSON for the selected database/workspace and Kubernetes context.
target/debug/proofstorm environment

# Keep this process running for local applications.
target/debug/proofstorm serve --port 8787
curl http://127.0.0.1:8787/v1/environment
curl http://127.0.0.1:8787/v1/environment/schema
```

MCP exposes `proofstorm_environment_read` in the default `developer` and opt-in `all` profiles. Its arguments match the HTTP query parameters and its `structuredContent` matches CLI/HTTP JSON. The contract is versioned as `proofstorm/environment/v1alpha1`; the [JSON Schema](../schemas/v1alpha1/environment.schema.json) is also available over HTTP.

The existing global CLI flags select `--database`, `--workspace`, `--principal`, `--context`, and `--namespace`. The reader needs `lab.read`, `lab.status`, and `experiment.read` in the selected workspace; it needs no operation or connection capability. HTTP uses the launching process's identity and rechecks its permissions on each data request.

The HTTP server binds only to loopback. It serves GET requests, sends `Cache-Control: no-store`, accepts local Host names and same-origin requests, and rejects foreign Origin headers. It has no CORS or remote authentication surface: trusted local processes may read as its configured principal. It is not a remotely shared or multi-user server. Snapshot connections close after their response; `/v1/events` stays open for SSE. Ctrl-C stops the server. The root path serves the embedded Leptos app. See [the web app guide](web-app.md).

## What the view means

- **Inventory:** labs currently present in the selected cluster and tracked in this database/workspace. Kubernetes inventory is read before stored lab records. Deleted labs, old generations absent from the cluster, and unmaterialized reservations are excluded. A current named lab has a `handle`. Instances known only to another database are outside this inventory. A cluster inventory failure fails the request; it is never interpreted as an empty cluster. The browser retains its last snapshot with a warning while reconnecting.
- **Topology:** stable component and link IDs from the immutable published revision. Links describe configured relationships. They do not establish current reachability, payment flows, or strict network-policy allowlists.
- **Endpoints:** desired Service host, port and transport, plus whether the existing local connection implementation supports that endpoint and its authentication method. Cluster DNS addresses are not laptop URLs. Metadata does not start a tunnel or promise current availability. Credentials, component configuration, Kubernetes annotations, command arguments, and artifact content are excluded.
- **Resources:** desired workload container requests/limits, namespace defaults, replica counts, and both standalone PVCs and StatefulSet claim templates. Quantities retain Kubernetes units. Resource rows cover the current component page plus shared workloads. These are rendered requirements, not measured usage or an aggregate cluster capacity estimate. Stopped workloads, scheduler scale changes, existing PVC retention, and temporary action jobs can make actual resource allocation differ. Repeated shared workloads across pages should be deduplicated by name. Claim-template requests are per replica; init-container resources should not simply be added to steady-state container demand.
- **Sessions:** actor, interval, last recorded activity, and an overlap count across all retained sessions for that instance. Finishing a session never gates work. Unfinished intervals are bounded by the read time for overlap calculation and do not prove that a client is alive. Session timestamps have one-second precision.
- **Activity:** recent managed operations across all runs, with actor, session/run IDs, target component IDs, phase, timestamps, artifact digest, and native exit/timeout/cleanup metadata when recorded. Native execution completion does not prove payment settlement. Raw command arguments and output remain in individual operation records.

Protocol traffic, measured resource usage, and attached external clients are explicitly marked **not collected**. External applications continue using native connections independently of this API.

## Freshness and failures

The response reports its observation start and finish times. Each lab reports when its journal was read, its latest recorded operation activity, and when Kubernetes was fetched. Runtime resource versions and observed/current generations are included. `source_updated_at_unix` comes from Kubernetes status field-management metadata when available; otherwise it is null. Component condition transition timestamps are not polling timestamps.

`runtime.state` is `available`, `stale`, `missing`, `unavailable`, or `not_materialized`. Revision/generation mismatches cannot produce a current `ready: true`. After inventory succeeds, a failed or timed-out per-lab runtime read preserves its stored topology and history with a safe error code. A missing runtime object does not itself prove verified teardown; a named handle's closed phase records the completed lifecycle. Inspecting never repairs missing observations or synchronizes results. `proofstorm serve` runs a separate background collector automatically; without the web server, use `proofstorm sync NAME` or `sync NAME --watch`.

Pages are observations taken over an interval, not one atomic snapshot across SQLite and Kubernetes. Permission and database access errors fail the request; runtime observation failures are represented within each lab. If a lab's stored records cannot be decoded by this version, its entry carries `read_error: "stored_record_incompatible"` and retains its ID and any current handle. Its empty sections mean unavailable data, not an empty lab. Other labs remain readable. The reader does not rewrite historical records or reinterpret retired permissions.

## Pagination

All collections use `{ "items": [...], "next_cursor": null | "..." }`. Treat cursors as opaque. Requests accept:

| Parameter | Purpose |
| --- | --- |
| `cursor` | Continue the environment's lab list. |
| `limit` | Requested maximum page size, 1–50; default 20. |
| `instance_id` | Select one instance, which must still be present in the selected cluster. |
| `component_cursor` | Continue that instance's components and corresponding resources. |
| `link_cursor` | Continue that instance's declared links. |
| `session_cursor` | Continue that instance's sessions. |
| `activity_cursor` | Continue that instance's activity. |

Section cursors require `instance_id`; the lab-list cursor cannot be combined with it. For example:

```bash
target/debug/proofstorm environment --instance-id INSTANCE --activity-cursor CURSOR
curl --get http://127.0.0.1:8787/v1/environment \
  --data-urlencode instance_id=INSTANCE --data-urlencode activity_cursor=CURSOR
```

The environment list initially previews up to 20 entries per nested section. For one instance, `limit` applies to its sections. The shared payload is bounded to 24 KiB and may return fewer items than requested. Follow **each section's own** `next_cursor`, as well as the lab-list cursor. Never infer completeness from the number of returned items. Resources follow the component page; links may reference components on another page.

Labs, components, links and sessions use ascending stable IDs. Activity uses descending acceptance time and ID across all runs; `sequence` remains local to its `run_id`. Its cursor must refer to an operation in the selected instance. New records inserted before a cursor require a fresh first-page read; existing rows may change while paging. Merge records by stable ID and refresh from the beginning to observe updates.

## Development checks

Generate the contract with `cargo run -p proofstorm-app --example export_environment_schema`. Application regression tests cover passive reads, workspace permissions, safe output, desired storage, runtime failure/staleness/identity mismatch, historical and advanced instances, cross-run activity, bounded topology pagination, and the loopback HTTP contract. MCP tests cover discovery, response size, the shared view, and permission rechecks. Application, MCP (including all five stdio contracts), and store regressions pass across package runs. Strict workspace Clippy, formatting, Helm lint, generated-schema comparison, and whitespace checks also pass. The default developer profile has 14 tools; the new read tool stays within its existing discovery budget.


## Live verification — September 6, 2026

The idle local cluster was upgraded to the session-tracking controller and current CRDs. The controller image is `proofstorm-registry.localhost:5000/proofstormd:environment-view-20260906`, built and pushed with manifest digest `sha256:36fabeae711ed0b5b6eacd57068c5022d0a4ba82c728b4ccd2926ad0d256e5f9`. Its rollout completed successfully.

An isolated database/workspace started the example Bitcoin Core + CDK BDK mint lab. Two principals concurrently ran native Bitcoin RPC commands; both completed with exit code zero. The environment reported both actors, overlapping sessions, two components, their declared backend link, current runtime status, and both storage claims. CLI and HTTP component/activity results agreed, and the output contained no RPC password. An HTTP read left the counts of sessions, actions, and idempotency receipts unchanged. The temporary lab was closed with verified teardown; a final cluster query found no lab namespaces.

This verifies the read surface and ordinary concurrent session operation. It does not rerun the full live wallet/payment/fault or private-handoff campaign suite. No HTTP server was left running after the check.
