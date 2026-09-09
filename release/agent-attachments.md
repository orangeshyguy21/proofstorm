# OpenCode and Claude Code alpha adapters

The next development bundle adds intentional project attachment for OpenCode
1.x and Claude Code 2.x. No global agent configuration, model, provider, login,
permission mode, or project trust choice is changed.

From the project directory:

```sh
proofstorm open opencode
proofstorm open claude
```

Both commands attach and verify Proofstorm, then start the agent in the current
interactive terminal. Add `--gui` to open the installed native app on macOS.
`claude-code` is also accepted as
an alias for `claude`. An explicit path works, including paths containing spaces.
Use `proofstorm attach opencode` or `proofstorm attach claude` to connect without
starting an interactive agent. `--dry-run` previews without writes, grants, or
MCP server startup. Development bundles still require `--allow-development`.

In `proofstorm gui`, choose **Launch Agent**, then click a vendor button. Only
supported native apps found in `/Applications` or `~/Applications` are shown
(Codex can also be discovered through its bundled executable on PATH). The folder
where the GUI was launched is selected. Click the folder field beneath the buttons
to open the native macOS directory picker. Picking or cancelling never attaches
an agent; cancellation keeps the previous folder.
Opening the dialog is read-only; clicking a vendor button attaches and opens.
There is no automatic installation or terminal fallback from the browser.
The no-labs screen reuses the same full-width button rows, folder selection,
busy state and replacement confirmation as the dialog. It does not maintain
a second attachment flow.

The MCP entry is named **proofstorm** in every client. An old alias such as `pst`,
or a manually edited connection, gets an explicit **Replace and open** choice.
This backs up the original config and replaces only that one project connection.
Consent is invalidated if the file, project, agent, or proposed connection changes.
Multiple duplicates and inherited/global connections still require manual review.

| Agent | Project configuration | Launch |
| --- | --- | --- |
| Codex | `.codex/config.toml` | Terminal by default; `--gui` for native app |
| OpenCode 1.x | Existing `opencode.jsonc`, otherwise `opencode.json` | Terminal by default; `--gui` for native OpenCode 1.18.30+ (1.x), with limitation below |
| Claude Code 2.x | `.mcp.json` | Terminal by default; `--gui` for native Claude 1.40609.1+ (1.x) |

GUI launch buttons always request the native app, independently of the CLI default.
OpenCode 1.18.30's new layout ignores project links: the app opens, but the folder
must be selected inside OpenCode. The default terminal launch avoids this upstream
bug ([#35225](https://github.com/anomalyco/opencode/issues/35225)).

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
- Manual or changed connections require explicit replacement consent. Duplicate,
  malformed, linked, or ambiguous configurations are refused. Two OpenCode
  project config files must be resolved explicitly.
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

Native project links: [Claude Desktop Code links](https://support.claude.com/en/articles/14729294-open-claude-desktop-with-a-link)
and [OpenCode desktop protocol handler](https://github.com/anomalyco/opencode/blob/dev/packages/desktop/src/main/index.ts).
OpenCode's `open-project?directory=` parser was also inspected in installed 1.18.30.
Only an encoded absolute folder is passed—no prompt, model choice or auto-submit.
Claude always asks the user to confirm the folder. A successful OS handoff does
not prove that an agent has loaded MCP: approve prompts and check its MCP status.

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
