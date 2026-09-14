# Efficient MCP reads

Agent reads should support a short discovery → search → selected read workflow.
Pagination bounds a response; filtering and projection keep irrelevant data out
of the agent's context in the first place.

For growing collections, provide stable IDs, a compact scan, exact filters,
literal/regex search where useful, selected fields, and bounded continuation.
Filter before paging. Bind cursors to the query and relevant source identity;
reject stale cursors instead of silently skipping matches. A live status page
identifies its observation but does not promise a frozen runtime snapshot.

Measure the complete `CallToolResult`: structured content, escaped text copy and
envelope. An accepted mutation returns a small receipt independent of collection
size. If a selected value is too large, expose the omission or an actionable
smaller read; never return an empty page with a non-advancing cursor.

## Current cell workflow

| Need | Tool and selection |
| --- | --- |
| Admit creation or an edit | `cell_up`: acceptance, identity and generation receipt |
| Find current version and readiness counts | `cell_inspect`: compact summary by default |
| Read a status subsection | `cell_inspect.fields`: RFC 6901 pointers into the detailed view |
| Find topology/configuration | `cell_search`: section, exact ID, literal/regex query, scan, fields, cursor |
| Find failing components | `cell_component_status_list`: component, ready, literal/regex query, scan, fields, cursor |
| Find Kubernetes resources | `cell_inventory_list`: kind, namespace, literal/regex query, fields, cursor (advanced toolset) |
| Find recorded execution/payment results | `activity_search`: component, phase, exit code, kind, actor, run, session, time, text, fields, cursor |
| Retrieve receipt details | `operation_read`: digest-bound JSON pointer or Unicode text slice |

`cell_sync` refreshes recorded activity; `activity_search` searches the stored
results without executing or synchronizing commands. `environment_read` remains
a workspace overview with cell/section pagination, and `session_list` remains a
paged session directory. They are not general text-search interfaces. New large
collection interfaces should follow this contract instead of adding another
unfiltered dump; these remaining directories are candidates for the next audit.

## Examples

Scan components that are not ready:

```json
{"instance_id":"alpha-retries","ready":false,"scan":true,"limit":20}
```

Find image-pull failures and retrieve only useful details:

```json
{"instance_id":"alpha-retries","query":"image_pull","fields":["/id","/conditions"],"limit":10}
```

Find one component's configured value:

```json
{"instance_id":"alpha-retries","id":"chain","fields":["/config/txindex"]}
```

Get a specific runtime section through `cell_inspect`:

```json
{"name":"alpha-retries","fields":["/runtime/blockers"]}
```

Missing JSON pointers return `null`. Projections are keyed by the requested
pointer; normal unprojected status/inventory entries keep their existing shape.
Keep filters, scan mode and selected fields unchanged when continuing a cursor.

## Editing and retrying

Read `desired_generation` and `instance_key` from `cell_inspect`. Pass them as
`expected_generation` and `expected_instance_key` to `cell_up` when editing.
Omit both for creation. A mismatched generation rejects a new edit before any
runtime request; the store also checks the generation atomically at admission.

Keep the full request unchanged for an exact retry. A previously accepted edit
reuses its saved plan and receipt. If newer work has since been accepted,
`accepted_generation` identifies the retried edit while `desired_generation`
identifies the current configuration. Replaying does not roll back newer work.
Unfenced calls retain the convenience behavior of applying against current state.

`cell.incarnation_generation` is the name-handle counter, formerly ambiguously
serialized as `generation`. It is independent of the desired configuration
version and can reset after verified teardown. Use `instance_key` to identify
an incarnation. Existing saved handles with the old field remain readable.

Acceptance does not mean readiness. Inspect after admission; if
`runtime_reconciliation_failed` is true, retry the same request to reconcile.
If `activity_ready` is false, retry it to restore the default run/session.

## Regression checks

Test growing history before a live edit, full text/structured wire size, exact
retries after newer edits, stale generations and replacement keys, filtered
cursor invalidation, byte-driven page shrinking without missing IDs, escaped
JSON, and scans/projections that recover from an oversized individual entry.
