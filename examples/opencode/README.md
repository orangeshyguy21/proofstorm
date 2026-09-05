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
- `PROOFSTORM_DB` and `PROOFSTORM_WORKSPACE` are relative to the repository
  root. Use a fresh database path per run when runs must not share state.

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
