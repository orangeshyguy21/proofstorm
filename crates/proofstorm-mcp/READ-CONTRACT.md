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
| Find cells in the environment | `environment_read`: compact scan, name/owner/phase/component filters, header search, section/field selection, cursor |
| Find actors and tracking intervals | `session_list`: exact ID or overlaps, actor/run/phase/time filters, text search, scans, fields, cursor |
| Admit creation or an edit | `cell_up`: acceptance, identity and generation receipt |
| Find current version and readiness counts | `cell_inspect`: compact summary by default |
| Read a status subsection | `cell_inspect.fields`: RFC 6901 pointers into the detailed view |
| Find topology/configuration | `cell_search`: section, exact ID, literal/regex query, scan, fields, cursor |
| Find failing components | `cell_component_status_list`: component, ready, literal/regex query, scan, fields, cursor |
| Find Kubernetes resources | `cell_inventory_list`: kind, namespace, literal/regex query, fields, cursor (advanced toolset) |
| Find recorded execution/payment results | `activity_search`: component, phase, exit code, kind, actor, run, session, time, text, fields, cursor |
| Retrieve receipt details | `operation_read`: digest-bound JSON pointer or Unicode text slice |

`cell_sync` refreshes recorded activity; `activity_search` searches the stored
results without executing or synchronizing commands. `environment_read` scans
only cells present in the selected cluster and tracked in this workspace. Its
selectors are shared with HTTP; requests without selectors preserve the existing
full GUI and CLI view.

Environment scans use the cluster listing and stored identity metadata. They
skip session/history decoding, resource rendering, endpoint expansion, and
per-cell runtime/prober requests. Typed component filters consult the desired
configuration. Header search runs before field selection; it does not search
configuration or receipts recursively. Runtime freshness remains explicit.
Messages are omitted from compact headers; select `/runtime/message` when needed.

Detailed environment sections require `instance_id`. Scan for a cell first,
then request `components`, `links`, `resources`, `sessions` or `activity`, or
fields inside those sections. Only dependencies of the selected fields load;
for example, reading a component ID does not render endpoints. Each returned
section cursor belongs to that cell and those selectors. Field reads preserve
section continuations alongside their projected values. An unrequested section
is absent; an unreadable requested section is null with an explicit error.

Session directory filters run in storage before bounded text/regex matching.
A sparse search can return an empty page with an advancing cursor after scanning
200 candidates; continue until the cursor is null. `id` means exact lookup;
`overlaps_with` means interval overlap, with legacy `session_id` accepted as its
alias. The overlap cutoff is fixed throughout pagination. `active` means an
unfinished tracking interval, not proof of a running agent. Reads never refresh
last-activity timestamps.

Directory cursors bind selectors, their boundary, scope and source identity.
Session updates invalidate session cursors. Environment cursors bind matching
cell membership and desired generations; readiness observations may change
without invalidation when matching membership stays the same. Restart without
the cursor after an invalidation. Old plain session cursors must also restart.

## Examples

Catalog discovery uses shared MCP/HTTP selectors: `catalog_list` accepts `query`,
`regex`, `case_insensitive` (default true), `origins`, `scan`, `fields` and its existing
exact implementation/kind/feature/lifecycle/dependency filters. For example,
`{"query":"cdk","kinds":["mint"]}` selects CDK mint images across visible origins.
Filters precede paging; complete wire size and snapshot-bound cursors remain bounded.
A new CDK mint build supplies all three runtime presets with one image; use a selector
from `catalog_entries` to choose a preset without another build. Catalog summaries
expose `shared_image_implementations`; each entry retains its own configuration contract.

`candidate_list` scans at most 500 stored records per call and reports `scanned_count`;
`matched_count` is null because an exact total would require scanning all history.
Continue through empty pages with a next cursor. Build updates invalidate that cursor.
Candidate record/log resources provide digest-bound text pages with `next_uri` so recipe
and retained diagnostic bodies do not need to fit a directory response. See
[Cashu candidate images](../../CANDIDATES.md) for source forms and evidence boundaries.

Find Bitcoin cells owned by an actor with `environment_read`:

```json
{"scan":true,"owner":"developer","implementation":"bitcoin-core","limit":20}
```

After selecting a returned cell ID, retrieve a component field:

```json
{"instance_id":"<returned-cell-id>","fields":["/components/items/0/id"]}
```

Find unfinished sessions with `session_list`:

```json
{"instance_id":"<returned-cell-id>","principal_id":"developer","phase":"active","scan":true}
```

Retrieve a session's run and last activity:

```json
{"id":"<returned-session-id>","fields":["/experiment_id","/last_activity_at_unix"]}
```

For HTTP, `sections` and `fields` accept comma-separated values or an encoded
JSON array string. MCP uses arrays. Pointers containing commas should use the
JSON array form in HTTP.

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
