# Recovering from MCP waits and input errors

## Inline cell authoring

For a new inline specification, `cell_plan` and `cell_up` accept top-level
`components` and `links` alongside `name` and `request_id`. Supply both arrays,
including `links: []` when there are no links. The server uses the outer name and
current cell API version. Component fields remain strict: copy implementation and
configuration versions from the catalog, and supply `id`, `kind`, `control` and
`config`. Component fields are advertised directly in the array-item schema so
clients with limited nested-schema rendering can show them.

Backend links use the existing flat kind-specific fields. An optional top-level
`policy` applies only to this inline form; omission uses the same safe default as
an ordinary cell document. Explicit null arrays or policy are rejected.

The existing `cell` document, encoded JSON and file forms, `patch`, and bound
`plan` remain available. Choose exactly one form; do not mix inline fields with
those alternatives. Every form uses the same validation, authorization and edit
fences. Changing input while reusing a request ID still conflicts. For edits,
copy `desired_generation` and `instance_key` from `cell_inspect` into
`expected_generation` and `expected_instance_key`.

`cell_remove`, `cell_wait` and `operation_wait` return
`requested_timeout_seconds` and `effective_timeout_seconds`. These are wait
bounds, not measured elapsed time. `cell_remove` accepts any positive wait
request but uses at most 30 seconds per call. Continue with its `next_tool`,
the same cell name and `expected_instance_key` until `complete` is true.
`cell_wait` and `operation_wait` accept 1–120 seconds and use the requested bound.
A backend that fails to answer before either wait deadline returns the same
limit metadata and next tool in the error data.

A wait timing out does not cancel the operation. The outer `timed_out` describes
the wait; `native_result.timed_out` describes the command itself. Startup blockers
and superseded cell generations direct callers to `cell_inspect` rather than
another wait.

`operation_read` accepts `limit` from 1 through 4000 characters or array items,
defaulting to 1000. Follow `next_offset` for more data. A missing artifact path on
a recorded pending/running operation returns `operation_read_output_pending`,
its `recorded_phase`, and an exact `operation_wait` call in `next_tool` and
`next_arguments`. The read itself does not poll or change state. After waiting,
use the refreshed operation digest for the next read. The requested artifact
path cannot be validated until an artifact exists; waiting does not expose
private output. Missing paths on terminal operations or existing artifacts keep
their ordinary pointer diagnostics, and stale digests still reject the read.

Start `operation_wait` with at most 10 IDs per batch. This is guidance, not a
new count limit: the complete response must fit the existing 32 KiB budget.
`operation_wait_response_too_large` reports `operation_count`,
`maximum_response_bytes`, and `suggested_batch_size`. Split the IDs using that
size, halving again if necessary. When even one compact receipt is too large,
the error directs `operation_read` for selected recorded fields instead of
repeating the same wait. Native exit, cleanup and application settlement still
need independent checks.

## Native output

Compact operation receipts preserve `native_result.output`, including available
stream byte counts and exact `operation_read` arguments for recorded public or
projected output. These hints survive omission of large artifact bodies. Follow
the returned digest and pagination to read output without executing the command
again. Missing byte counts mean unavailable information, not zero bytes.

Private stdout and stderr cannot be revealed through `operation_read`. Retained
byte counts record what the runner captured at execution time; they do not
promise those files still exist. To recover application state, use an appropriate
read-only native query with public or projected output. Do not repeat a payment
or another state-changing command merely to expose its output. Command exit,
process cleanup and application settlement remain separate facts.

## Input corrections

Wait bounds, operation and cell read limits/offsets, activity search limits,
native output fields and missing read pointers use a common `data.issues` shape:

```json
{
  "code": "operation_read_limit",
  "issues": [{
    "path": "/limit",
    "code": "out_of_range",
    "expected": {"minimum": 1, "maximum": 4000},
    "example": 1000
  }]
}
```

Paths identify request fields. Examples are replacement field values, not complete
requests. Examples describe the field's schema; identifiers and other application
values must still refer to real resources. Existing domain error codes remain stable.
Errors appear in both structured content and text for clients that only render text.

Missing pointers in `operation_read`, `cell_read` and `catalog_config_schema_read`
include an existing parent, up to 16 child pointers, omission flags, the current
document digest and the next read tool. Suggestions contain no sibling values and
are generated only after the resource's access checks. They normally use the
nearest existing parent; `parent_shortened: true` indicates a shorter ancestor was
selected to keep the response bounded. Child pointers use at most 256 encoded JSON
bytes each; the parent uses at most 512. `pointers_omitted` also covers children
excluded by those bounds. Follow the returned paths using the same resource and
document selection. For cell reads, copy `document_digest` into `expected_digest`;
`cell_digest` from search applies only to configuration, not the plan or image lock.
Catalog reads return `config_schema_digest` for identification but do not accept
an `expected_digest` argument.

Cell read limits, offsets and scalar scan errors include field corrections.
Offsets count Unicode characters in strings and items in arrays or object scans;
an offset equal to the length is valid and returns an empty final page. A scalar
cannot be scanned: set `scan: false` to read its value.

When request parsing fails, every tool returns `tool_input_invalid`, `executed: false`
and a bounded issue list. Missing fields, wrong types, unknown fields and invalid enum
values use the tool's original generated schema. A unique close spelling match is
returned as `did_you_mean`; suggestions never rename fields or retry commands
automatically. Tagged unions report the selected variant's requirements. Ambiguous
forms are left unresolved rather than reporting requirements from arbitrary branches.
Cell diagnostics also understand canonical links and JSON-encoded cell documents;
paths inside an encoded cell refer to the decoded document. File references are not
opened to generate parsing diagnostics.

At most 16 issues and 6 KiB of encoded issue data are retained. `details_may_be_omitted`
is true for truncated, unsupported or unresolved diagnostics; an empty list is not
a claim that the input is valid. Examples are omitted when no simple field example
or schema-provided example is available. Parsing diagnostics expose schema facts and
field paths, not submitted values. Authorization failures and existing application
errors retain their original classification and details.

Diagnostics run only after the original parser rejects the request. Tool schemas,
request admission, output permissions and scoring rules remain the authority.
