# OpenCode profiles

Each file here is a complete OpenCode configuration that registers the
Proofstorm MCP server with the same capability set and differs only in what
the agent may do on the host. Pick one with the config environment variable:

```bash
OPENCODE_CONFIG=examples/opencode/proofstorm-only.json opencode .
```

| Profile | Host tools | Use it for |
|---|---|---|
| `proofstorm-only.json` | none; every host tool is denied | evidence-grade runs where all lab and network control must go through Proofstorm MCP; the doctor validates this file |
| `research.json` | read, glob, grep, list, web fetch, web search | experiments whose prompt asks the agent to read the README, the spec, the gates, or upstream sources before acting |
| `contributor.json` | research plus edits under the acceptance crate, `tests/`, `examples/`, and `scenarios/`, and hermetic cargo and read-only git commands | runs that must leave a new acceptance gate or scenario behind |

Rules shared by all three profiles:

- Lab and network control always goes through the MCP server. No profile
  grants `kubectl`, `docker`, `helm`, `make`, or a Proofstorm release build.
  Pull-request candidate images are built through Proofstorm MCP by a durable
  controller-owned Job; the agent never needs a host command.
- "Internet" means two different things. Host web access is a profile choice
  above. Network access from inside lab pods is a lab property and stays
  default-deny except for cluster DNS; both native component execution modes
  run in-cluster and cannot reach the internet under any profile.
- `PROOFSTORM_DB` is relative to the process working directory;
  `PROOFSTORM_WORKSPACE` is a logical identifier. The profiles explicitly select
  `k3d-proofstorm`. Use an absolute database path when launching elsewhere.

Host permissions and the MCP toolset are independent. These profiles default
`PROOFSTORM_TOOLSET` to `native`, a slim experiment surface that uses the real
component CLIs for funding, payments, peers, and channels. Keep `experiment` for
typed-contract comparisons. Native commands run through `component_exec_live`
inside a lab component; the host `bash` permission can remain denied. See the
[validation plan](../../docs/native-first-experiments.md) for evaluation and
cleanup requirements.

OpenCode resolves permission patterns with `*` matching any characters, so
`tests/*` covers every file below `tests/`. Agent-level `permission` blocks
override these globals if you add named agents to a profile.

## Growing an existing lab

Reconnect MCP after upgrading Proofstorm. Use `proofstorm_lab_read` with the
instance ID to get complete configuration and its generation, then plan the full
updated topology with `update.instance_id` and `update.expected_generation`. Apply
the returned digest. Unchanged components keep their state; inspect the restart
and removal lists before applying. Use `expected_generation` in `lab_wait` and
handle `superseded` or startup blockers explicitly. See the
[edit contract](../../docs/dynamic-lab-edits.md).

## Starting work without experiment setup

Plan and apply the lab, then inspect status, logs or execute native commands.
Ordinary native requests can omit `experiment_id` and `session_id`; attribution
is automatic per actor and lab incarnation. Keep the operation ID and idempotency
key when retrying. Explicit experiments are optional grouping for evidence work.
Export any desired evidence before closing the lab: confirmed deletion removes
its local history and releases its name. No leases or run budgets are required.
