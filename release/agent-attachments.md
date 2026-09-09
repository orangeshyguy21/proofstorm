# OpenCode and Claude Code alpha adapters

The next development bundle adds intentional project attachment for OpenCode
1.x and Claude Code 2.x. No global agent configuration, model, provider, login,
permission mode, or project trust choice is changed.

From the project directory:

```sh
proofstorm open opencode
proofstorm open claude
```

Both commands attach and verify Proofstorm, then run the installed agent in that
directory in the current interactive terminal. `claude-code` is also accepted as
an alias for `claude`. An explicit path works, including paths containing spaces.
Use `proofstorm attach opencode` or `proofstorm attach claude` to connect without
starting an interactive agent. `--dry-run` previews without writes, grants, or
MCP server startup. Development bundles still require `--allow-development`.

In `proofstorm gui`, choose **Connect coding agent**, select the agent and folder,
review the configuration path, then confirm. Codex retains its native launch.
OpenCode and Claude Code attach and display a safely quoted terminal command;
the GUI does not claim to have opened them. Native desktop launch is deferred
until a supported, project-specific launch interface is verified.

| Agent | Project configuration | Launch |
| --- | --- | --- |
| Codex | `.codex/config.toml` | Native app by default; `--cli` for terminal |
| OpenCode 1.x | Existing `opencode.jsonc`, otherwise `opencode.json` | Terminal |
| Claude Code 2.x | `.mcp.json` | Terminal |

Configuration applies through each agent's normal project/directory lookup,
including subdirectories. This is not a filesystem access sandbox. The generated
entry points to this machine's installation: review it before committing project
configuration, especially Claude's normally team-shared `.mcp.json`.

## Safety and compatibility

- Each installation/project/agent combination has its own identity. Reconnecting
  never silently restores revoked grants.
- MCP initialize, tools listing, and a read-only environment call must succeed
  before configuration is written. This alone does not prove agent discovery or
  a model tool call; the result keeps `harness_loaded: false`.
- Only the owned Proofstorm entry is edited. Other settings and JSONC comments
  are preserved. Existing files receive private, content-addressed backups.
- Manual, changed, duplicate, malformed, linked, or ambiguous configurations are
  refused. Two OpenCode project config files must be resolved explicitly.
- Known global/ancestor/local Proofstorm conflicts are inspected read-only.
  OpenCode custom configuration overrides are refused, not unset or overwritten.
  Remote organization settings, plugins, managed policies, and tool permissions
  may still prevent loading; inspect the agent's MCP status after connecting.
- OpenCode v2 has a different MCP schema and is deliberately rejected. Claude's
  standard project MCP approval and trust prompts remain the user's decision.
- Nothing installs or upgrades an agent, changes permission settings, or starts
  a billable model session as part of attachment.

Official references checked September 9, 2026:
[OpenCode MCP configuration](https://opencode.ai/docs/mcp-servers/),
[OpenCode configuration precedence](https://opencode.ai/docs/config/),
[OpenCode v2 MCP changes](https://opencode.ai/v2/docs/mcp-servers), and
[Claude Code MCP scopes and approval](https://code.claude.com/docs/en/mcp).

## Verification

The packaged local gate passed on September 9, 2026 with OpenCode 1.18.30 and
Claude Code 2.1.69. Both actual clients reported Proofstorm connected; an unrelated
folder had no Proofstorm connection. GUI preview/confirmation APIs, backups,
idempotency, conflict preservation, separate actor identities, source-denied
execution, and owned runtime cleanup passed. All 62 backend tests and 27 Python
fixtures passed, with clean backend and WASM lint checks. See
[dated verification](agent-attachment-verification.json).

This verifies client connection, not a model-session tool call or native app
handoff. The new browser selector was built but not manually clicked in this gate.

Unit tests cover lossless edits, conflicts, duplicate JSON keys, JSONC trailing
commas, literal path expansion hazards, backups, version guards, terminal quoting,
and typed GUI agent selection. The opt-in installed gate is:

```sh
python3 scripts/test_installed_setup.py \
  --archive /absolute/path/to/development-bundle.tar.gz \
  --work-dir /private/tmp/a-new-proofstorm-agent-test \
  --start-runtime --test-agents
```

This gate creates a disposable runtime and private agent homes. It checks CLI and
GUI attachment, then runs each actual installed client's `mcp list` in the chosen
and unconnected folders. It does not prompt a model, approve project trust, or
launch native agent windows. Runtime cleanup and dev-environment preservation
use the existing installed-smoke ownership checks. See the dated verification
record for results rather than treating the existence of this test as a pass.
