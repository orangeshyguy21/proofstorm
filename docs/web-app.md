# Live lab explorer

Proofstorm runs protocol labs and lets you watch agents work in them.

```sh
make serve
# Open http://127.0.0.1:8787
```

After `make setup`, `make serve` rebuilds the web assets and CLI, initializes
the local developer permissions, and starts the server. Keep it running;
Ctrl-C stops it. Use `make serve PORT=8788` for another port, or
`make serve ARGS='--database /absolute/path/state.db --workspace my-lab'`
to select another agent workspace. Global options in `ARGS` apply to both
initialization and serving. Each launch restores the selected identity's
default developer permissions.

No separate `init` is needed for `make serve`. When an agent uses a configured
MCP server, that server registers its own identity and grants at startup too.
An agent using the CLI directly needs `init` if its selected identity has not
already been initialized.

Use the same database, workspace and Kubernetes context for the website and the
agent. The CLI defaults and example MCP configurations share
`.proofstorm/proofstorm.sqlite3` and workspace `local-lab`. An agent with the
`developer` toolset can call `proofstorm_lab_up`, then `proofstorm_lab_exec`.
The website automatically discovers the lab and shows its components,
connections, readiness, desired resources, sessions and recorded activity.

The UI is read-only. Click a component for service endpoints and resource
requests; drag the topology to pan, or use the zoom controls. Only labs present
in the current cluster are listed; deleted labs disappear automatically. Activity and sessions offer
explicit history pagination; the graph follows every component and link page.
Labs with incompatible stored records show "History unavailable" individually,
so older history cannot prevent current labs from loading. Their stored data
is preserved. Database failures and permission failures have distinct messages.
The receipt collector ignores pending records for labs absent from the cluster.
For current labs it skips incompatible pending records and reports degraded
collection while continuing to process readable operations.

## Live updates

`GET /v1/events` is a Server-Sent Events endpoint. It emits an `environment`
event on every connection and whenever the server detects a change:

```text
event: environment
id: 12
data: {"refresh":true}
```

Subscribers fetch `GET /v1/environment` for the current typed snapshot. Events
are invalidation hints, not durable operations or a replayable event log.
`Last-Event-ID` never suppresses the initial refresh: reconnecting always gets
a current snapshot. Browser `EventSource` reconnects automatically. Slow
subscribers coalesce changes instead of building an unbounded queue. A quiet
stream sends comments every two seconds and rechecks authorization. There are
at most eight simultaneous streams; HTTP snapshots remain available separately.

One server task checks the journal and Kubernetes lab resources every two
seconds. SQLite `data_version` detects commits from other processes, while
`total_changes` detects writes through the server's connection. Runtime resource
versions/status detect controller changes independently of journal writes.
The controller also watches managed pods and promptly reconciles their lab on
readiness, restart and probe changes, with its existing periodic check retained
as a recovery fallback. Changes in another workspace in the same SQLite database may cause an extra
refresh, but the snapshots remain workspace-scoped. This is eventual observation,
usually within a few seconds, not a complete record of every intermediate pod
transition. Kubernetes watches can replace server-side checks later without
changing the subscription contract.

Starting `serve` also starts a background receipt collector. Every second it
checks up to 50 pending/running operations across this workspace, with bounded
concurrency, timeouts and automatic retry. It records completed runtime receipts
using the same validation and idempotency path as `sync`, including actions from
other principals and actions whose agent disconnected. It does not submit or
cancel operations, create sessions, or change lab infrastructure. It stops with
the server. CLI/MCP snapshot reads and HTTP GET handlers remain passive.

`GET /v1/observer` reports receipt-collector health. The UI shows degraded
collection, unavailable cluster status and lost stream connections explicitly.
It retains its last snapshot during a connection failure and retries failed
snapshot requests. Collection needs `lab.status`, `experiment.read` and
`artifact.read`; normal developer initialization already grants these.

## Scope

This is a local development server, bound to loopback, using the launching
principal's permissions. It accepts same-origin browser requests and local
processes, rejects foreign origins and Host names, and exposes no mutation API.
It is not a multi-user deployment or remote authentication service.

The view covers labs present in the selected cluster and tracked in the selected database/workspace. It does not
discover labs created using another database. Connections are declared topology,
not live payment flows. CPU/memory/storage are desired demands, not measured
usage. Protocol traffic and attached external clients are not collected yet.
Endpoint metadata is credential-free; use `proofstorm connect` to produce a
local application's private connection configuration.

## Rust development

- `proofstorm-view`: shared serializable view types; no Kubernetes or SQLite.
- `proofstorm-web`: Leptos 0.8.20 CSR, Wasm, SVG and plain CSS.
- `proofstorm-app`: existing Hyper server, embedded static assets, SSE and receipts.

`make web` installs checksum-verified Trunk 0.21.14 into `.tools`, installs the
Rust Wasm target and compiles assets. Rebuild `proofstorm-app` afterward to embed
new assets, or use `make build` for the complete sequence. Native builds and tests
can run without Trunk; a binary built without frontend assets serves the API and
returns a clear setup error at `/`.

For UI development, run `proofstorm serve` on 8787 and `make web-dev` in another
terminal. Trunk serves on 8080 and proxies `/v1/`, including SSE. No Node package
manager is required. Native invoice validation remains enabled by default in
`proofstorm-core`; the browser disables that optional feature, since displaying
lab state does not need cryptographic invoice validation.

Validation commands:

```sh
cargo test -p proofstorm-app -p proofstorm-store -p proofstorm-view -p proofstorm-web
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p proofstorm-web --target wasm32-unknown-unknown -- -D warnings
cargo fmt --check
```

## Verified locally (2026-09-06)

- 19 app lifecycle/API tests, 63 MCP/stdio tests, 15 store tests, two web
  presentation tests and 20 controller tests passed. The published environment
  schema stayed unchanged. Workspace and Wasm Clippy, formatting and Helm lint passed.
- A real MCP client created a Bitcoin Core/CDK BDK mint lab in an isolated
  workspace. An already-open browser discovered it and showed both components
  ready without a reload.
- The MCP client submitted a 35-second native command and disconnected. The
  server collected the successful receipt automatically and the browser showed
  `Succeeded` / `exit 0`, preserving its selected node and 120% zoom.
- A managed pod restart first exposed the controller's former 30–40 second
  stable-lab observation gap. With the pod watch, the same check observed
  readiness false at 1.08 seconds and recovered at 3.75 seconds. The browser
  received the updated controller state through SSE.
- The browser retained its last snapshot and displayed a reconnecting warning
  when the server was stopped. Transport tests verified a fresh invalidation on
  reconnect and termination after permission revocation.

The local controller image is `web-live-20260906`, digest
`sha256:e55ff43d006019499c8307f6c88ead098a9274d3025e3b518f3c9e90345b186f`.
The temporary lab was closed by its creating agent, with verified namespace
cleanup. The live check used a separate `web-check` database/workspace; normal workspace
permissions were not changed. Full payment/fault acceptance campaigns were not
rerun for this presentation and observation change.
