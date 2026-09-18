# Programmable workspace

The `workspace` component keeps files on a persistent volume and runs managed
tasks independently of agent connections. Native `cell_exec` commands remain
bounded to five minutes; the workspace supervisor owns longer tasks.

Create a cell from [cell.json](cell.json) with `cell_up`, then wait for readiness.
The catalog kind is still `attacker`; the implementation is `workspace`.

## Files and reusable functions

Create scripts and supporting modules locally, then use `workspace_upload` to
copy them under `src/` without putting their contents in MCP arguments:

For this example, save `echo hello` in `examples/workspace/greeting.sh` first.

```json
{
  "name": "workspace-demo",
  "component": "scripts",
  "request_id": "upload-greeting",
  "source_path": "examples/workspace/greeting.sh",
  "path": "src/greeting.sh"
}
```

`source_path` names a regular file on the MCP server host. Relative paths use
the server's working directory, which is the attached project for managed
agents. Uploads support text and binary files up to 16 MiB and preserve whether
the source is executable. Size and SHA-256 are verified before an atomic
replacement; the operation receipt records the destination, size, checksum and
executable flag. File contents do not enter MCP arguments or the activity journal.
Retry an interrupted upload with the same request ID and unchanged file. Changed
bytes, destination or executable permission require a new request ID.

Uploads are staged before their recorded commit. The next upload reclaims
abandoned staging older than one hour; staging is bounded to 16 files and 32 MiB
per workspace. Completed and cancelled uploads release their staging space;
late duplicate calls cannot recreate it. If cancellation reports that cleanup
needs a running workspace, retry `operation_cancel` once the workspace is ready.
A failed staging transfer leaves the destination unchanged. The 16 MiB total
source snapshot limit still applies when starting a task.

Each file or task control call returns an ordinary operation ID. Use
`operation_wait`, verify the native exit code, and read the JSON in `stdout`.
Use a new request ID for every fresh status, log, or file read. Reusing a request
ID returns the original control operation and its original result.

Files can be organized as ordinary shell libraries or modules for a custom
runtime. The default runtime is BusyBox: shell, `wget`, text utilities, and
`httpd`. It does not include Python, Node, curl, or jq. To use another language,
set `config.runtime_image` to a fully qualified image pinned with `@sha256:`.
It must run as UID/GID 1000 and provide `/bin/sh` for native control calls.
Bake dependencies into that image; the workspace retains cell network isolation.
The selected image is captured in the resolved lock and each task's metadata.
Changing a running component's image currently requires a new component; the
existing state migration rules still apply.

`workspace_file` supports atomic UTF-8 writes up to 8192 bytes, 1024-byte paged
reads, directory listing, and removal of regular files. Omit the list path to
discover the workspace root. Use `workspace_upload` for local scripts and binary
data. File paths are relative to `/workspace`, and the file tool refuses symlinks
and supervisor internals.

## Tasks

```json
{
  "name": "workspace-demo",
  "component": "scripts",
  "request_id": "start-greeting",
  "task": {"action": "start", "task_id": "greeting", "argv": ["sh", "greeting.sh"]}
}
```

Start captures `src` by default, then runs the command inside that copy. Use
`source` to select another workspace-relative code directory. Supply either
`argv` or `script`, an optional `env` object, and an optional `timeout_seconds`.
Without a timeout, the task runs until it exits or is stopped.

* `PROOFSTORM_WORKSPACE` points to `/workspace`; use `data/` for shared state.
* `PROOFSTORM_OUTPUT` points to the task's persistent `output/<task_id>` directory.
* Editing the original source does not change a running task's captured files.
* Repeating a task ID with identical input returns the original task, even if it
  has finished or was interrupted. Changed input under that ID is refused.
  Use a new task ID to run again or capture updated source.
* Tasks share a user and volume. Snapshots preserve what was submitted; they are
  not a security boundary against code deliberately modifying supervisor files.

Use these `task` values with `workspace_task`:

```json
{"action":"status","task_id":"greeting"}
{"action":"logs","task_id":"greeting","stream":"stdout"}
{"action":"list"}
{"action":"stop","task_id":"greeting"}
```

Logs return the latest 1024 bytes; each stream retains two rotating 256 KiB
segments. Log reads explicitly expose raw output. Status is metadata only.
List pages contain up to eight tasks, with `next_after` for continuation.
Stop is asynchronous: wait for a terminal task phase and `cleanup_verified`
for process cleanup. Network faults have a separate cleanup observation below.
Cancelling the short control operation does not cancel its workspace task.

Tasks are cell-owned, so a continuing miner does not hold an experiment run open.
Control requests and their responses enter the ordinary activity journal.
Background output and result files remain on the workspace volume; explicitly
capture needed results into a run, then download its evidence before removing
the cell. Captures are included automatically in that run's export.

## Calling another component's native CLI

Add an explicit control scope to a task:

```json
{"action":"start","task_id":"controlled-miner","argv":["sh","miner-control.sh"],"env":{"MINING_ADDRESS":"<regtest-address>","BLOCKS":"10"},"control":{"components":["chain"],"max_calls":10,"max_timeout_seconds":15}}
```

Inside the script, call the installed helper. It runs the command inside the
target component, where its CLI, local credentials and sockets are available:

```sh
"$PROOFSTORM_CONTROL" workspace call '{"call_id":"height-1","component":"chain","command":{"argv":["bitcoin-cli","-regtest","-rpcconnect=127.0.0.1","-rpcport=18443","-rpcuser=proofstorm","-rpcpassword=proofstorm-regtest-only","getblockcount"],"timeout_seconds":10,"output":{"mode":"public"}}}'
```

The helper waits for a JSON receipt and exits nonzero if the command fails,
is cancelled, or its outcome is uncertain. It uses the same command and output
contract as `cell_exec`; raw output remains private unless explicitly requested.
Keep the same `call_id` and identical input when collecting an uncertain result.
A new ID can repeat application effects. Receipts are also readable through
`workspace_file` at `output/<task_id>/control/<call_id>.json`.

Native scopes allow 1–16 component IDs, 1–4096 calls (default 256) and up to 300 seconds
per command (default 30). Calls are dispatched serially per task; independent
tasks can work concurrently. Each encoded call is limited to 8192 bytes. The
helper waits for the command deadline plus 60 seconds; if a queued call takes
longer, retry its exact ID to collect the result.

The grant retains the initiating principal and cell revision. It lasts until
the task ends or is stopped; changing the caller's permissions does not revoke
an already accepted task. A revision change or workspace replacement closes
control and stops the task. Stop requests and crashes cancel outstanding native
calls through the controller. Cancellation is asynchronous and cannot undo
effects that already occurred. A controller outage delays dispatch/cancellation;
each started command still has its own enforced deadline.

Call receipts are retained as controller actions and workspace
files; their `action_id` is not an `operation_wait` ID, and they are not yet
part of a run until explicitly captured with `workspace_capture`. All scripts in one workspace
share its user and control state; use separate workspace components for different
trust levels.

## Lifecycle and temporary network faults

Scripts can start, stop or restart selected components and temporarily partition
explicit pairs. Upload `outage-and-restart.sh`, then use `workspace_task` with:

```json
{
  "name": "workspace-demo",
  "component": "scripts",
  "request_id": "start-outage",
  "task": {
    "action": "start",
    "task_id": "outage-demo",
    "argv": ["sh", "outage-and-restart.sh"],
    "control": {
      "lifecycle": ["chain"],
      "network": [{"from_component": "chain", "to_component": "scripts"}],
      "max_fault_seconds": 60,
      "max_timeout_seconds": 120,
      "max_calls": 3
    }
  }
}
```

The script requests a 30-second partition, waits five seconds,
restarts Bitcoin, then releases its partition. Set `env` to
`{"CRASH_AFTER_PARTITION":"1"}` and use a new task/request ID to exercise a
failure immediately after the partition. The controller owns cleanup; a shell
trap or a live workspace process is not required.

The helper accepts these typed calls:

```json
{"call_id":"stop","operation":{"kind":"component_stop","component":"chain"}}
{"call_id":"start","operation":{"kind":"component_start","component":"chain"}}
{"call_id":"restart","operation":{"kind":"component_restart","component":"chain"}}
{"call_id":"outage","operation":{"kind":"network_partition","from_component":"chain","to_component":"scripts","duration_seconds":30}}
{"call_id":"heal","operation":{"kind":"network_heal","partition_call_id":"outage"}}
```

Pass each JSON object to `"$PROOFSTORM_CONTROL" workspace call`. Call IDs, budgets,
serial dispatch and exact-retry rules also apply to these operations. Typed calls
wait up to 360 seconds in the helper; controller operations and lifecycle
convergence use the scope's `max_timeout_seconds` after dispatch. A timeout cannot
roll back an already applied start, stop or restart. Lifecycle changes persist
after task exit. A newer user
lifecycle action on that component supersedes later calls from the older task.

Starting a lifecycle-enabled task requires `component.control` in addition to
workspace execution authority. Network control requires both `network.partition`
and `network.heal`. These permissions are checked even when task start comes
through raw `cell_exec`. Each scope admits up to 16 entries, all in the same cell.
A task cannot lifecycle-control its own workspace component.

Network grants name exact, bidirectional pairs. A task can heal only its own
partition, using the partition's call ID. Multiple tasks may hold the same pair;
releasing one preserves the others. The fault ceiling defaults to 60 seconds and
can be raised to 3600. Each partition must specify a positive duration within
that ceiling. Its expiry clock begins when the controller accepts dispatch,
not when the script receives the receipt. Exit, stop, interruption, loss of the
owner or expiry also trigger release. Cleanup retries after partial policy
writes and controller restart. Expiry is controller-enforced: a controller or
Kubernetes API outage can delay healing beyond the requested duration.

For a network-enabled task, status contains a `control_cleanup` observation:

```json
{"pending_faults":0,"observed_at_unix":1234567890,"task_phase":"cancelled"}
```

Wait for zero pending faults **and** an observed `task_phase` matching the task's
terminal phase. An older observation from a running task does not establish
cleanup after exit. Process `cleanup_verified` is independent of this observation.
Network cleanup means the controller has applied policies without this task's
faults; it does not assert that another task's partition has been removed or that
network traffic has been probed. Initial call receipts are immutable snapshots;
the controller action retains subsequent release evidence. `workspace_capture`
preserves both the initial mailbox records and later controller observations.
Delay and packet-loss injection are outside this slice.

## Capturing task evidence

A task can keep running while an experiment captures a fixed observation of it.
Create a run with `run_start` (or use an existing open run in the same cell):

```json
{"name":"workspace-demo","run_id":"mining-observation","request_id":"open-observation"}
```

Call `workspace_capture` after the recorder has produced its output:

```json
{
  "name": "workspace-demo",
  "component": "scripts",
  "run_id": "mining-observation",
  "request_id": "capture-recorder-1",
  "selection": {
    "task_id": "recorder",
    "output_paths": ["heights.jsonl"],
    "include_logs": false
  }
}
```

`output_paths` names exact regular files under `output/<task_id>`. Omit it to
capture inputs and control records without output files. Directories and wildcards
are not expanded. Logs are optional; enabling them includes all retained stdout
and stderr segments, not the entire historical stream.

The capture includes submitted source, command and environment, task status,
selected binary or text output, local control records, and a later observation
of the corresponding controller actions. Each file carries its byte length,
executable bits, SHA-256 and standard base64 content. A missing claimed child
action is recorded as missing with an unknown outcome. Controller cleanup does
not rewrite the original call receipt. Raw inputs, selected outputs and logs are
shared with readers of the run; choose files accordingly.

Capture neither stops the task nor waits for it to finish. It observes files
sequentially, then reads controller records; the timestamps describe those
separate observations. It is not an atomic filesystem checkpoint or proof that
application effects completed. A selected file that changes during its read is
refused. Retry while its writer is idle, or have the script publish a finished
output file atomically. New tasks keep submitted code separately from their
working directory, so generated caches do not alter captured inputs. Older
tasks can be captured only if their execution copy still matches its start digest.

The receipt returns a `capture_id`, digest and counts. The file bodies stay out
of the tool response. Keep the **entire request unchanged** on retry: the frozen
transfer and completed local record preserve the first observation. Use a new
request ID for a later observation, including a capture after task stop or fault
cleanup. Inputs and selected file bytes are copied into local run storage before
success is reported. A failed or interrupted transfer does not attach partial
evidence, and a run that closes during capture refuses the late attachment.

After receiving the capture receipt, call `run_finish`, then `evidence_export`:

```json
{"run_id":"mining-observation","request_id":"finish-observation"}
```

```json
{"run_id":"mining-observation"}
```

The export's `workspace_capture_count` counts attached snapshots. Its resource
contains their complete bodies in `content.workspace_captures`, regardless of
the optional ordinary artifact-body selection. For a small inspection, call
`evidence_section_read` using the returned capture ID:

```json
{"run_id":"mining-observation","section":"workspace_capture","capture_id":"<capture_id>","pointer":"/content/snapshot/task"}
```

Download the complete evidence resource before `cell_down`. Cell teardown
purges local run history, including captures. The downloaded JSON includes the
file bodies and digests and can be checked offline. While the run remains in
local storage, repeated exports require no live workspace and have the same digest.

Capture requires `component.exec_live`, `artifact.read` and `experiment.read`.
Limits are 64 selected output paths, 8192 bytes of selection JSON, 4096 files,
24 MiB total file content and 40 MiB encoded evidence per capture. Oversized,
missing or linked files fail the capture instead of being silently omitted.
The workspace retains at most 32 interrupted transfer copies within 128 MiB;
successful imports attempt to remove their transfer copy after durable storage.
Submitted source now has two copies, so account for both when sizing the volume.

## Concrete flows

Upload the included scripts under `src/` using `workspace_file`.

**Mine a block every 30 seconds.** Obtain a regtest address from the Bitcoin
component using its normal native CLI, then start:

```json
{"action":"start","task_id":"miner","argv":["sh","miner.sh"],"env":{"MINING_ADDRESS":"<regtest-address>"}}
```

The first block is mined immediately. Each subsequent iteration waits 30 seconds
after the preceding RPC completes. Write `data/mining-paused` to pause mining;
remove that file to resume. RPC failure ends the task visibly. Restart with a new
task ID after inspecting the error.

**Record observations while mining.** Run `record-height.sh` with a different
task ID. It writes observations into `output/<task_id>/heights.jsonl`. Configure
`SAMPLES` and `INTERVAL_SECONDS` through `env`.

**Simulate a slow failing service.** Run `fake-service.sh`; the example cell
declares port 8080. Other components can call
`http://scripts:8080/cgi-bin/respond`. Each request delays and returns HTTP 503.
This is a cell-local service, without an external host port.

## Failure behavior and limits

Workspace restart preserves files and terminal records. Active tasks with no
terminal receipt become `interrupted`; they are never automatically replayed.
Reconcile application state before retrying any payment or other side effect.
Graceful shutdown requests cancellation and verifies descendant cleanup where
possible. An interrupted task does not claim its effects were rolled back.

The first implementation allows 16 concurrent tasks, 128 retained task records,
and source snapshots up to 16 MiB / 512 entries / 32 directory levels. Source
symlinks and special files are refused. The workspace has a 1 GiB memory limit
and two CPU limit. Storage defaults to 1 GiB (`storage_size` also accepts 2, 5,
or 10 GiB). Task-created output consumes that volume and is the script's
responsibility to bound. Removing the cell deletes its workspace state.

Scripts can call cell services directly or use scoped native, lifecycle and
temporary network controls. Explicit task captures now join experiment evidence
exports. Richer default runtimes and capture policies can build on this foundation.
