# OpenCode profiles

These profiles add the Proofstorm MCP server to your personal OpenCode
configuration. From the repository root, launch:

```bash
OPENCODE_CONFIG=examples/opencode/proofstorm-only.json opencode .
```

`proofstorm-only.json`, `research.json`, and `contributor.json` now have the same
settings. Existing launch commands still work; the filenames no longer select
different host restrictions. The doctor uses `proofstorm-only.json` to check MCP
discovery, not host permissions.

OpenCode automatically merges `~/.config/opencode/opencode.json` with the selected
profile. Your providers, models, and named subagents remain personal. These
profiles enable the Task tool and leave shell, editing, file access (including
external directories), and web permissions to your settings and OpenCode defaults.
They do not grant every tool unconditional access. Agent-specific permissions can
still restrict individual agents.

To use a different model for delegated work, define a named agent with
`mode: "subagent"` and `model: "provider/model"` in your personal config, then ask
the main agent to delegate to it. No shell wrapper is needed. Provider endpoints
and model choices do not belong in these shared profiles. Restart OpenCode after
changing configuration.

See OpenCode's [configuration precedence](https://opencode.ai/docs/config/#precedence-order)
and [subagent configuration](https://opencode.ai/docs/agents/#json). To inspect
the merged settings locally, run:

```bash
OPENCODE_CONFIG=examples/opencode/research.json opencode debug config
```

The output can contain private provider settings; inspect it locally rather than
posting the full output.

Rules shared by all three profiles:

- Use MCP for lab operations so Proofstorm can track them. These profiles do not
  enforce MCP-only execution. Acceptance runs that require it must state that
  requirement in their prompt and verify the recorded tool calls.
  Pull-request candidate images can be built through Proofstorm MCP by a durable
  controller-owned Job.
- "Internet" means two different things. Host web access follows your OpenCode
  settings. Network access from inside lab pods is a lab property and stays
  default-deny except for cluster DNS; both native component execution modes
  run in-cluster and cannot reach the internet under any profile.
- `PROOFSTORM_DB` is relative to the process working directory;
  `PROOFSTORM_WORKSPACE` is a logical identifier. The profiles explicitly select
  `k3d-proofstorm`. Use an absolute database path when launching elsewhere.

Host permissions and the MCP toolset are independent. These profiles default
`PROOFSTORM_TOOLSET` to `native`, a slim experiment surface that uses the real
component CLIs for funding, payments, peers, and channels. Keep `experiment` for
typed-contract comparisons. Native commands run through `component_exec_live`
inside a lab component; the host `bash` permission can remain denied.

## Growing an existing lab

Reconnect MCP after upgrading Proofstorm. Use `proofstorm_lab_read` with the
instance ID to get complete configuration and its generation, then plan the full
updated topology with `update.instance_id` and `update.expected_generation`. Apply
the returned digest. Unchanged components keep their state; inspect the restart
and removal lists before applying. Use `expected_generation` in `lab_wait` and
handle `superseded` or startup blockers explicitly.

## Starting work without experiment setup

Plan and apply the lab, then inspect status, logs or execute native commands.
Ordinary native requests can omit `experiment_id` and `session_id`; attribution
is automatic per actor and lab incarnation. Keep the operation ID and idempotency
key when retrying. Explicit experiments are optional grouping for evidence work.
Export any desired evidence before closing the lab: confirmed deletion removes
its local history and releases its name. No leases or run budgets are required.
