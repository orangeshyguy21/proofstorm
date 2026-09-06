# Developer lifecycle: first high-impact implementation

> Superseded coordination design: leases are replaced by nonblocking [session tracking](session-tracking-2026-09-06.md); private-transfer permissions are separate. Historical results below describe their recorded version.


> Subsequent decision: run time limits and action-count budgets have been removed entirely. The budget recommendations/results below describe the earlier design, not the current interface. See [removal checkpoint](run-limit-removal-2026-09-06.md).

This increment implements the first usable path from the [architecture review](architecture-simplification-review-2026-09-06.md): **start a named lab, run native commands, connect an application, inspect results, and close the lab**. See the [quick start](../README.md#developer-quick-start) and [example lab](../examples/developer-lab.json).

## Delivered

| Review item | Implementation |
| --- | --- |
| H1: simplify lifecycle | `proofstorm init/up/status/exec/sync/result/down`; durable named labs; an automatically managed run and finite root lease; stable command retry IDs; visible owner, expiry, used and remaining action budget; resumable finish with verified cleanup. Closing and reopening a name creates a new immutable instance generation. |
| H2: application connections | `proofstorm connect` exports a loopback URL and separate owner-only configuration for mint HTTP or authenticated Bitcoin Core RPC. Native HTTP clients need no MCP or Kubernetes credentials. New connections re-resolve ready pods; lab closure stops tunnels and removes their configuration files on normal exit. |
| H3: shared behavior | New `proofstorm-app` crate owns Kubernetes lifecycle/submission, receipt collection, named labs, and local connections. Both developer CLI and MCP call this library. `status`/`lab_inspect` only observe; `sync`/`lab_sync` explicitly collect results. `sync --watch` provides an independently running collector while its process remains alive. |
| H4: operation readiness | Removed the MCP/runtime blanket requirement that the entire lab be Ready. Component-specific controller admission still applies. New work is refused during lab deletion, Closing, or CleanupBlocked. |
| H5: smaller default surface | Default MCP `developer` profile exposes 12 tools when fully granted: catalog discovery, named lab lifecycle, component inspection, native execution and operation observation/cancellation. Explicit `all`, `native`, `experiment`, `design`, `runtime`, and `evidence` profiles remain available. Existing regression gates explicitly select `all`. |

The local store continues to own authored configuration, permissions and durable history. Kubernetes continues to own observed workloads and live execution. Private payload custody is unchanged. `lab_handles` adds only the durable name/generation, owner, initial limits and shutdown latch; it is not another copy of workload readiness.

No new control server, event bus, universal protocol proxy, or application request ledger was introduced. External requests use the real component's protocol; they neither consume managed action budgets nor automatically become action records. The connection permission (`lab.connect`) is separate from action leases.

## Verified behavior

An isolated local lab used Bitcoin Core 30.0 and CDK BDK 0.18.0 with the existing local controller. The developer CLI was tested against that controller; this verification did not deploy a replacement controller.

- Repeating startup resumed the same instance after a Kubernetes metadata conflict. The shared runtime now retries only bounded metadata conflicts when attaching/releasing a lease; it does not replay native commands or reset budgets.
- A managed `getblockchaininfo` command returned a durable receipt with exit code 0 and `cleanup_verified: true`.
- An ordinary host HTTP client read the mint's `/v1/info` through the mint tunnel.
- The same client authenticated to Bitcoin RPC using the private configuration file and read `getblockchaininfo`, reporting `chain: regtest`.
- `down` returned Closed with `verified_absent: true`. Both connection processes exited and both private configuration files disappeared. A subsequent cluster query found no lab namespaces.
- Seven mocked Kubernetes lifecycle tests cover process reconnection, idempotent command replay, finite budgets, interruption/resume, closure without provisioning, refusal to claim cleanup with an orphan namespace, shutdown admission, metadata conflicts, configuration permissions and capability revocation.

The CLI also checks native exit code, timeout, signal and cleanup receipt: an action reaching Succeeded means execution collection finished and does not itself establish that the native command exited successfully or that a payment settled.

Regression verification passed across package runs: **263 Rust tests** (53 core, 85 Kubernetes, 56 store/transfer/controller, seven new application lifecycle tests, and 62 MCP tests including five real stdio contracts). The first full run exposed a stale generated CRD and discovery-size regressions; the CRDs were regenerated and the affected packages rerun successfully. Strict workspace Clippy, formatting, Helm lint, and whitespace checks also passed.

MCP discovery measures **12 tools / 38,921 bytes** for the default developer profile. Repeated lifecycle response schemas are advertised once through inspect/operation-status contracts. The opt-in `all` union now includes the five named-lab tools and remains larger (87 tools / 276,503 bytes); existing focused profiles retain their prior scope.

## Explicit boundaries and remaining work

This is the first vertical implementation of the high-impact items, not the completion of every extraction proposed in the review.

- Planning, scenario orchestration and full evidence assembly still have code in MCP. They remain compatibility paths; extraction/migration can proceed behind the shared application boundary without blocking ordinary lab use.
- There is no always-on synchronization service. Start `sync --watch` when ongoing durable collection is needed. Missing or interrupted outcomes remain explicit; inspection itself does not repair them.
- Configuration replacement currently means close, then up with the same name. In-place reconciliation of edits and retaining multiple named historical generations in a lab-list UI remain future work. Prior operations remain addressable by request ID in the durable store.
- A namespace remaining without its runtime object is reported as unverified cleanup; the application does not delete arbitrary orphan infrastructure.
- Connections currently support mint HTTP and Bitcoin Core RPC. Lightning TLS/macaroon export, private Unix sockets, remotely hosted applications and attached-client discovery in an environment API remain future work.
- A Kubernetes tunnel bypasses lab NetworkPolicy faults. Successful tunneled requests do not establish connectivity along ordinary lab routes. Component restarts can break existing TCP sessions; applications must reconnect. A forced kill can leave a private config file behind; normal termination removes it.
- The visualization API, protocol traffic collectors and simulated flow/event scheduling are not introduced here. Workload state and managed action receipts can form that API's initial observation surface; external protocol traffic may remain unobserved until collectors exist.
- Advanced delegation remains available and retains its existing authority fences. The developer profile does not expose private transfer/handoff or scenario tools by default.

Run the hermetic workspace suite with `make test` (connection tests need permission to bind local loopback sockets); use `make lint` for formatting, strict Clippy and Helm lint. Live payment/fault/campaign gates are separate checks and were not all rerun for this increment.
