# OpenCode failure capture

The controlled OpenCode 2 benchmark profile `controlled-mcp-only-opencode2-v2`
records evidence to distinguish local harness termination, telemetry mismatches,
provider-reported failures and transport failures whose origin is unknown. It
does not assign blame from a socket reset or alter benchmark scoring. Provider
configuration, model inventory, plans and results remain private caller data in
ignored `dev/`; no provider-specific settings are shipped with this code.

## Evidence per attempt

| File | Contents |
| --- | --- |
| `transport.private.jsonl` | Local request IDs; session/model identity; allowlisted request/response headers; request and response body capture; time to headers/first chunk, maximum inter-chunk gap and byte/hash totals; HTTP status; SSE error payloads and provider response IDs; observed retry decisions; abort/cancel events; independent route probes. |
| `transport-capture-status.json` | Capture shutdown/drain state, pending request/probe counts, dropped records and write errors. A missing or unfinished file means coverage is unknown. |
| `transport-capture-config.json` | Capture version, limits and explicitly selected control origins. Also included in the benchmark manifest. |
| `opencode-stop.json` | Local termination reason, whether an interrupt was requested and succeeded, and whether session export succeeded. |
| `opencode-session.private.json` | Durable session export, including on local timeout or output limit when the owned server remains reachable. |
| `opencode-*-error.private.txt` | Interrupt/export/API errors when capture cannot finish. |
| `telemetry-reconciliation.private.json` | Exact MCP/transcript matches, unmatched IDs, same-tool candidates and argument differences such as extra properties or string-versus-number values. Candidates are comparisons, never assumed identities. |
| `failure-diagnostics.json` | Observation categories, local termination, capture status, client error count and telemetry completeness, with references to private evidence. |

The grading receipt includes SHA-256 hashes for the capture files, stop receipt,
session export and reconciliation report that exist at grading time. Regrading
checks those hashes and refuses changed or newly added evidence. A receipt proves
integrity of the files present, not completeness of a missing capture. Old sealed
runs stay unchanged; missing historical capture cannot be reconstructed later.

HTTP header IDs and response/chunk IDs have different meanings. Preserve both
with UTC timestamps and the local request ID when asking a provider to correlate
its logs. Do not relabel a response/chunk ID as an HTTP request trace ID. Only
IDs the provider returns are available; the capture does not inject trace headers
or change retry decisions. A retry's pending request IDs are candidates, not a
guaranteed retry-to-request join. Request object identity gaps are explicit.

## What can be concluded

| Evidence | Supported conclusion | Remaining uncertainty |
| --- | --- | --- |
| HTTP 200 with an SSE `error` or error finish reason | An error arrived in the response stream; retain payload status/code separately from HTTP status. | The provider's gateway versus serving backend needs its correlated logs. |
| HTTP 4xx/5xx and response headers | An HTTP error response was received. | Cause may be request validation, quota, gateway, proxy or backend; the status alone does not locate it. |
| `ECONNRESET`, stream error, or no response | A transport failure occurred or the request stayed unresolved. | Local internet, proxy, remote edge and backend are not distinguishable from this alone. |
| Local `timeout`/`output_limit` plus interrupt/abort | The harness initiated a stop at its own boundary. | Earlier provider errors can coexist; local cancellation is not a provider outage. |
| Exact argument mismatch across OpenCode and MCP | The captured representations differ, explaining incomplete scoring telemetry. | Which adapter performed a coercion/stripping step needs inspection; do not normalize away the evidence. |
| Provider probe fails while independent controls succeed | Supports a problem specific to that destination or route. | Probes use separate connections and may differ from inference routing. |
| Provider and controls fail together | Supports a shared connectivity/resolver problem. | It does not prove the user's internet was the cause. |

Any incomplete capture, ambiguous reset or contradictory observations stay
undetermined. Valid task failures remain valid scores (including zero); provider
interruptions remain unscored. New diagnostics do not repair an old score or
permit selecting the best of repeated attempts.

## Independent connectivity controls

The capture probes observed provider origins. To add independent comparisons,
place up to two credential-free HTTPS origins in the **private provider profile**:

```json
{
  "providers": { "your-provider": { "...": "existing private settings" } },
  "diagnostics": {
    "control_origins": ["https://your-independent-control.example"]
  }
}
```

This is a shape example; retain the profile's actual native provider definition
and choose controls known to be reachable from the test machine. No public
control endpoint is contacted by default. The setting is part of the frozen
profile fingerprint and cannot be added halfway through a campaign.

Each sample resolves DNS and attempts TCP, then certificate-verified TLS for
HTTPS, using at most one address per IP family. Literal IPs are marked, since
they do not test DNS resolution. Samples occur at the first request, every
60 seconds while the plugin lives, and on errors/retries, with a 30-second
minimum between samples and no overlapping batches. DNS and each connection
stage have two-second limits. These are connection probes, not inference calls
or model health requests. They do not inspect the actual inference socket,
HTTP proxy, Wi-Fi state or packet path. OpenCode and Node/Bun probes may use
different resolver/connection behavior. No failed IPv6 probe alone is treated as
a general outage.

## Bounds, privacy and experimental comparability

The plugin observes requests after the caller's compatibility plugins. It keeps
at most 1 MiB of each body and schedules at most 64 MiB of JSONL per attempt.
Truncated body hashes describe captured prefixes; response stream hashes cover
all bytes the consumer read, not bytes the provider may have sent afterward.
Request clone reads have a two-second observation deadline. Response capture is
pull-driven and forwards the original bytes; capture failures do not intentionally
replace or reject the provider response. Disk work, probes and request observation
have overhead that remains part of end-to-end timing.

Header values are allowlisted; authentication, cookies and arbitrary custom
headers are omitted. Omitted header **names** are retained. JSON credential keys
and observed HTTP credentials are redacted from bounded JSON bodies. Partial or
non-JSON request bodies retain hashes and lengths but no raw prefix. SSE response
content remains private with known HTTP credential values removed; this is not
a general-purpose secret scrubber. Prompts, generated content, tool arguments,
model IDs and hostnames can still be sensitive. Do not send entire attempt
directories as provider feedback. Extract reviewed error fields, timestamps,
IDs and relevant sanitized excerpts instead.

On local timeout/output limit the CLI client is stopped, the owned server session
is interrupted with `continue=false` (five-second API bound), and its session is
exported (15-second bound) before server shutdown. Probe draining is bounded to
2.5 seconds during plugin cleanup, and unfinished probes remain visible. A hard
kill, lost server, missing session identity, full disk or truncated logs can still
prevent final capture. These failures are not healthy-network evidence.

OpenCode's [plugin HTTP and retry hooks](https://dev.opencode.ai/v2/docs/build/plugins/)
cover HTTP traffic. They do not capture native WebSockets, and attaching HTTP
hooks can keep some routes on HTTP according to its
[provider transport documentation](https://opencode.ai/v2/docs/providers).
The new profile version identifies this experimental difference. Keep existing
campaigns pinned to their original binaries, task contracts and provider profile;
use a new linked campaign to adopt these diagnostics.

## Verification without provider usage

The Node tests use loopback fixtures and cover HTTP-200 streamed errors,
redaction, response byte preservation, reset/abort/cancel distinctions, capture
bounds, disk failure, correlation gaps and independent route probes:

```sh
node --test crates/proofstorm-acceptance/src/benchmark/opencode/capture.test.mjs
```

Rust benchmark unit tests preserve strict telemetry and scoring behavior. The
opt-in installed-CLI tests use isolated temporary homes, a fake loopback model
in the test process and a fake Bash/`jq` MCP server, with no provider account or Docker:

```sh
cargo test -p proofstorm-acceptance benchmark --lib
PROOFSTORM_OPENCODE_TEST_BIN=/absolute/path/to/opencode \
  cargo test -p proofstorm-acceptance benchmark::opencode::contract_tests \
  --lib -- --ignored --test-threads=1
```

The CLI fixtures assert real hook correlation and shutdown flushing, a normal
tool round trip, a forbidden tool attempt, and session interruption/export after
a stalled response. Use a separate build directory while another campaign is
running; never rebuild its pinned binaries in place.
