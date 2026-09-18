# Workspace validation — 2026-09-17

The first task/file implementation passed the following checks.

| Check | Result |
| --- | --- |
| `just check` on macOS ARM64 | Formatting, shell checks, strict Rust Clippy, and 822 passing tests across 54 test targets; four fixture tests intentionally ignored |
| Linux ARM64 native contracts | Existing native execution tests and five workspace process-lifecycle tests passed inside the Docker `native-contracts` stage |
| Linux ARM64 Clippy | All `proofstorm-exec` targets, with `contract-tests`, passed with warnings denied |
| Kubernetes rendering contracts | Persistent claim, Recreate deployment, verified supervisor installer, storage readiness, custom image selection, and optional service port passed; snapshots regenerated |
| Agent control contracts | Workspace-only admission, exact control retries, capability revocation, native command bounds, tool discovery and response-size budget passed |

## Live script exercise

An isolated internal Docker network ran the catalog's pinned Bitcoin Core 31.1
image and the pinned BusyBox workspace image with the new supervisor. No host
ports were published. The three scripts in this directory ran concurrently.

* `miner.sh` mined 11 regtest blocks with a 30-second interval and remained
  running after more than 310 seconds, beyond the native command timeout limit.
* Repeating the start request returned the original task and source digest.
* `record-height.sh` produced two timestamped observations in its output file.
* `fake-service.sh` returned the expected delayed HTTP 503 response.
* Stopping the miner produced `phase=cancelled` and `cleanup_verified=true`.
* Restarting the workspace preserved the observation file byte-for-byte.
* Repeating the miner's start after restart returned its cancelled record;
  it did not mine again.
* The test removed its containers, volume, and network afterward.

Live evidence from this session is under
`/tmp/proofstorm-workspace-live.CtHtFm`; the check logs are
`/tmp/proofstorm-workspace-check-final.log`,
`/tmp/proofstorm-workspace-native-tests-final.log`, and
`/tmp/proofstorm-workspace-linux-lint-final.log`.

This establishes real Linux process and script behavior plus the rendered
Kubernetes contracts. It does not establish an end-to-end Kubernetes rollout,
publication of a new controller image, or installation into the user's running
environment. Cross-component control bridging and automatic task evidence
bundling remain outside this first implementation.

## Second slice: scoped native control

The control bridge now passes `just check`: 827 repository tests, zero failures,
and four intentionally ignored fixture tests across 54 targets. Final workspace
Clippy, Linux Clippy with warnings denied, chart lint and the example's shell
lint also passed.

Four controller tests use a simulated Kubernetes API and mailbox transport to
exercise actual reconciliation: authority and revision inheritance, controller
reconnection, a lost claim acknowledgement, missing child actions, cancellation,
transport loss, duplicate owners, removed targets and workspace replacement.
They verify that ambiguous calls are not recreated and cancellation receipts
remain collectable.

Linux ARM64 process tests passed inside the `native-contracts` Docker stage:
five portable workspace file tests, eight native supervisor tests and seven
workspace lifecycle tests. The two added workspace tests run the real manager
and helper. They cover scope and budget rejection, immutable call IDs, durable
claims across restart, original-owner preservation, receipt persistence, and
cleanup of a script blocked waiting for a call. The target command's receipt is
simulated in these helper tests; they do not establish live Kubernetes dispatch.

The new `miner-control.sh` example uses the target Bitcoin component's CLI through
this bridge. Its shell syntax and lint passed; unlike the original direct-RPC
miner above, this example has not yet been exercised against a running cluster.

Second-slice logs are `/tmp/proofstorm-workspace-bridge-check.log`,
`/tmp/proofstorm-workspace-bridge-native.log`,
`/tmp/proofstorm-workspace-bridge-clippy-final.log`,
`/tmp/proofstorm-workspace-bridge-linux-lint.log`, and
`/tmp/proofstorm-workspace-bridge-targeted.log`.

This is source implementation and isolated validation, not deployment to the
existing installation. Lifecycle/fault control and automatic experiment evidence
bundling remain future slices.

## Third slice: lifecycle controls and temporary partitions

Final `just check` passes formatting, shell checks, strict workspace Clippy and
834 repository tests across 54 targets, with zero failures and four intentionally
ignored fixture tests. The catalog coverage records were regenerated to match
the expanded workspace capability notes.

The workspace now admits explicitly scoped component start/stop/restart calls
and leased network partitions with task-owned healing. Capability checks cover
both `workspace_task` and raw native task starts. Tests reject undeclared pairs,
excessive durations, incomplete network authority and workspace self-control.

The controller suite passes all 51 tests. Its simulated Kubernetes API exercises
expiry and missing-owner cleanup without workspace access, cancellation before
activation, recovery after partial policy updates, overlapping partitions, and
preservation of ordinary faults. Policy writes carry resource versions and read
the latest journal so stale snapshots cannot reactivate a released lease. Bridge
tests also verify that a completed partition is cancelled on task exit and that
cleanup observations continue through `stopping` to the terminal task phase.
Lifecycle tests verify sequential calls from one task and precedence for newer
user controls.

Linux ARM64 native contracts pass five portable file tests, eight native
supervisor tests and nine workspace lifecycle tests. The added tests run the real
manager/helper, accept typed receipts without a native exit code, record cleanup
responsibility before dispatch and retain cleanup observations after task exit.
Linux Clippy passes with warnings denied. Helm lint and shell syntax/lint for
`outage-and-restart.sh` also pass.

This validation uses simulated Kubernetes responses and real Linux processes.
The new outage example has not been exercised against a running Kubernetes
cluster, and policy-update receipts do not establish live traffic behavior.
No controller image was published or installed into the existing environment.
Automatic experiment evidence bundling remains the next slice.

Third-slice logs are `/tmp/proofstorm-workspace-faults-check-final.log`,
`/tmp/proofstorm-workspace-faults-controller-final.log`,
`/tmp/proofstorm-workspace-faults-native.log`, and
`/tmp/proofstorm-workspace-faults-linux-lint.log`.

## Fourth slice: workspace task evidence

Final `just check` passes formatting, shell checks, strict workspace Clippy and
842 repository tests across 54 targets, with zero failures and four intentionally
ignored fixture tests. The full check used a fresh temporary build cache after
directory reads in the existing host cache stalled. Tool discovery includes all
47 tools within the unchanged 128 KiB envelope budget.

`workspace_capture` attaches an immutable snapshot to an open run without stopping
the task. The evidence export includes submitted inputs, selected file bodies,
local control records and later controller observations. The capture's request
digest binds retries to the same run, component and file selection. New tasks
preserve submitted source separately from the execution directory.

Linux ARM64 contracts pass seven portable file tests, eight native supervisor
tests and ten workspace lifecycle tests. The real manager/helper test captures a
running task, excludes generated working-directory files from submitted inputs,
preserves binary bytes, restarts the workspace, retries the frozen transfer
byte-for-byte and releases it. Strict Linux Clippy also passes.

Application and export checks cover a run closing during download, pod
replacement, corrupted bytes, missing or oversized selected files, changed
request identity, optional rotated logs, and exact retries without a live
workspace. Controller record checks preserve an original partition receipt while
capturing later cleanup and mark missing child actions explicitly unknown.
The export test reads a capture through the bounded evidence section, purges the
cell and verifies the downloaded JSON's retained binary content and digests.

The Kubernetes API and bulk transfer are simulated in application tests; the
supervisor and capture helper run as real Linux processes. The live validation
below separately exercises the Kubernetes WebSocket transfer. The existing
teardown contract is unchanged: download evidence before removing the cell,
which purges local run history and its captures.

Fourth-slice logs are `/tmp/proofstorm-workspace-evidence-check-final.log`,
`/tmp/proofstorm-workspace-evidence-native-final.log`, and
`/tmp/proofstorm-workspace-evidence-linux-lint.log`.

## Live dev Kubernetes validation

The workspace flows passed on the existing two-node dev cluster
`k3d-proofstorm-4a151314`, running Kubernetes `v1.34.9+k3s1`. The registered dev
client and deployed controller were already compatible; this exercise did not
require a rebuild or rollout. The deployed controller image was
`proofstorm-registry.localhost:5000/proofstormd@sha256:f3b952c43eef3b3fc37e32c3616747226a62fd08a2948cec0480f7b6e59d5704`.

The disposable cell `workspace-live-0917-4cpuy0` contained Bitcoin Core 31.1 and
the default BusyBox workspace. A fresh MCP client exercised all three workspace
tools through the registered dev binary and the real Kubernetes API.

* The controlled miner produced three regtest blocks using 30-second intervals,
  completing in 71 seconds. Repeating its completed task returned the original
  record without mining again. A height recorder and the delayed HTTP 503 service
  ran concurrently; stopping them verified process cleanup.
* A 50,817-byte capture crossed the real Kubernetes exec/WebSocket transport
  while its task stayed running. It retained binary bytes, a 32 KiB file, logs,
  original submitted source and selected outputs. Generated working-directory
  files did not enter the submitted-source capture.
* Changed capture requests and missing selected files were refused. Exact
  retries returned the original capture after output changes, MCP reconnection
  and run closure. Finishing the evidence run left the task running.
* A transfer staged before import survived workspace restart byte-for-byte.
  Import retained the earlier running-task snapshot while current task status
  remained cancelled. No task replay occurred, and successful import removed
  the frozen transfer copy.
* Real Bitcoin RPC traffic succeeded before a partition, failed during it, and
  succeeded after its owning task exited with code 23. A separate 15-second lease
  expired and restored traffic while its task remained running.
* The unchanged `outage-and-restart.sh` example completed its partition, Bitcoin
  restart and heal. Block height remained three and RPC traffic was restored.
  Captures preserved the initial call receipt unchanged while recording the
  later controller cleanup and `task_ended` release reason.
* Three exported bundles contained eight captures. All bundle, capture and file
  hashes were independently verified, including exact binary bytes. Offline
  exports and an exact capture retry succeeded without a live-runtime client.
* The disposable cell and its namespace were verified absent after teardown.
  Downloaded evidence remained verifiable. The existing `demo` and `pr-1131`
  cells retained their identities, revisions and generations, and both remained
  ready.

The test harness, MCP transcript, receipts and downloaded evidence are retained
under `/tmp/proofstorm-workspace-kube.4CpuY0`; `summary.json` records the passed
checks and bundle digests. This validates the default workspace
runtime on this dev cluster; controller/node failure and custom runtime images
were not exercised in this live run. No product-code fix was needed.

## Local file uploads and text-write limits

Strict workspace Clippy and all 850 repository tests across 54 targets pass,
with four intentionally ignored fixtures. Formatting and diff checks pass.
MCP discovery includes 48 tools within the existing 128 KiB budget.

`workspace_upload` accepts a local source path and workspace destination, with no
inline file body. Application tests cover binary bytes, executable permission,
metadata-only operation records, exact and changed retries, interrupted staging,
pod replacement, cancelled operations, revoked permissions, unsafe destinations,
wrong components, missing files, FIFOs and the 16 MiB source boundary.

The Linux ARM64 suite passes nine portable/staging tests, eight native supervisor
tests and thirteen workspace integration tests. Uploads exercise empty files,
binary files larger than the inline limit, an exact 16 MiB file, truncated and
corrupted transfers, an oversized request, staging capacity/expiry, destination
symlinks and an uploaded executable launched as a managed task. Staging leaves
the destination unchanged; commit verifies the checksum and replaces it atomically.

The inline-write regression accepts all 8192 raw UTF-8 bytes even when JSON
escaping would exceed the native command budget. Tests include quotes,
backslashes, newlines, NUL and multibyte text, reject 8193 bytes, and preserve
the original encoding of previously accepted requests for exact retries.

These initial upload checks used simulated Kubernetes APIs in application tests
and real Linux helper/supervisor processes. Subsequent live validation and fixes
are recorded below.

Logs: `/tmp/proofstorm-workspace-upload-check-rust-final.log`,
`/tmp/proofstorm-workspace-upload-linux-final.log` and
`/tmp/proofstorm-workspace-upload-linux-clippy-final.log`.


## Upload fault regressions — September 18, 2026

The first live upload run exposed stranded staging after cancellation and late
duplicates, simultaneous exact-retry admission conflicts, and cancellation
incorrectly recorded as a missing-runtime failure. The fixes pass strict host
and Linux Clippy, 852 repository tests across 54 targets (four intentionally
ignored fixtures), and 33 Linux helper/supervisor/workspace tests. Formatting
and diff checks pass.

The fixed checkout was built and deployed normally to the dev Kubernetes cluster.
Using a disposable workspace and independent stdio MCP connections:

* Eight simultaneous identical first uploads returned one operation without
  conflicts. The database regression additionally exercises eight connections
  over eight fresh admission rounds.
* A duplicate helper was paused until the first upload committed. Resuming it
  returned the original success and left no staged payload.
* Two 16 MiB uploads were cancelled while staging was blocked. Both reported
  `cancelled` with `action_cancelled` and `submitted: false`, preserved their
  absent destinations, and released all staged payloads. A fresh upload then
  succeeded immediately, without expiry or manual cleanup.
* Executable upload and managed task execution, exact 16 MiB binary hashes,
  empty files, unsafe source/path refusals, unchanged and changed retries, and
  all five 8192-byte escaping boundaries passed again.
* MCP disconnection and workspace restart during staging left destinations
  unchanged; same-request retries recovered with matching hashes.
* The helper upgrade preserved workspace files and cleanly stopped the old task.
* The test cell and namespace were removed. Existing `pr-1131` and `demo` cell
  identities, revisions and generations were unchanged; both cells and all 17
  associated component/prober pods were ready.

The tested controller image is
`sha256:b2f782dc8b1cbd69cde29a1813cb7e12c766bbe1ff09c6ca24df5ced05d87dc6`.
The dev cluster remains on this build. Cancellation cleanup needs a running
workspace; an unavailable workspace produces an explicit cleanup retry message.

Reproductions, transcripts, receipts and cleanup evidence are retained in
`/tmp/proofstorm-upload-fix-live-0918`. Host check log:
`/tmp/proofstorm-upload-fix-check-rust-final.log`; Linux check logs:
`/tmp/proofstorm-upload-fix-linux.log` and
`/tmp/proofstorm-upload-fix-linux-clippy.log`.
