# Checkout workflow

Install just (`brew install just` on macOS; see [Linux packages](https://just.systems/man/en/packages.html)).
Run `just` or `just --list` to discover commands. Just is a contributor tool, not
a dependency for people installing Proofstorm.

Normal development no longer requires Python. `scripts/develop.sh` orchestrates
Trunk, Cargo, and checkout registration. The small Rust `proofstorm-xtask` helper
owns ownership checks, persisted build settings, resource snapshots, and atomic
launcher writes. It is built separately under `target/maintainer`; it is not
shipped to installed users. The first development run compiles this helper.

Existing `.proofstorm-dev/owner.json`, `build.json`, state, launchers, and selected
binary paths are retained. Resource snapshots may get a new content-addressed
directory; older snapshots are not deleted or overwritten. Controller source
hashing keeps the existing contract and excludes host/web source.

The development shell uses Bash or Zsh from `SHELL`, with startup files disabled;
otherwise it defaults to Zsh on macOS and Bash on Linux. Normal exit/EOF succeeds
even after an interrupted command; shell launch errors and signals still fail.

Run `just dev` from the Proofstorm checkout. It builds matching CLI/MCP binaries,
web assets, chart/CRD resources, and controller source snapshot, then enters a shell selecting this checkout's
private installation. Docker is not touched by the build. Inside that shell:

```sh
storm setup
storm doctor
storm up examples/developer-cell.json
storm gui
```

Leaving the development shell with `exit` or Ctrl-D is a successful session end,
even after an interrupted or failed command. Command failures still appear in
the shell; build/registration failures before it opens still fail `just dev`.

Commands show an ASCII spinner and status text in an interactive terminal,
starting before installation checks. Setup reports its current stage. Ordinary
results are human-readable; use `storm setup --json`, `storm gui --json`,
or the global `--json` flag on another command for the full machine-readable
result, with no spinner. Redirected output uses plain progress lines on stderr,
not terminal animation. `version --json` and internal checkout registration retain
their machine-readable output. This is the same CLI behavior in release bundles.

GUI startup verifies artifacts once in the launcher and independently once in
the new backend. The verified snapshot is reused only within that startup; it is
not a persistent cache. Subsequent requests still detect changed artifacts and
stale GUI builds. Progress reports file checks, server startup, runtime ownership
and health checks, and browser activation. A new server opens the browser directly;
an existing server first attempts to focus its tab.

The checksum dependency is optimized in debug builds via `.cargo/config.toml`;
application debugging and integrity checks are unchanged. This host-only build
setting does not invalidate the controller snapshot or require rebuilding images.

For agent attachment, change to the application's directory and run
`storm agent open codex`, `storm agent open opencode`, or `storm agent open claude`.
The connection is project-specific and keeps this installation selected even
after leaving the development shell. No global agent configuration is changed.

`just dev-build` rebuilds without entering a shell. `.proofstorm-dev/bin/proofstorm`
is the same command launcher outside that shell. `just setup`, `just doctor`,
and `just gui` are conveniences for that launcher. No release archive, installer,
global PATH mutation, or legacy cell migration is involved.

## Rebuilding

- Web: run `just web-dev` in another terminal, then refresh the managed GUI after
  each build. Assets use the same authenticated backend/origin; there is no
  separate API proxy. Automatic browser reload is not implemented yet.
  `just web` performs a single asset rebuild through the same path.
- Host code: run `just dev-build`; stop/reopen the GUI and reconnect agent
  sessions afterward. Existing cells, installation identity, and grants survive.
- Chart/CRDs: rebuild, then run `storm setup` to apply the new snapshot.
- Controller/runtime-contract changes: run `just dev-build`, then `storm
  setup` (or simply `just deploy`). Setup builds the recorded linux/arm64 source,
  verifies source identity, platform, and client compatibility, publishes only
  to this installation's loopback registry, and deploys by immutable digest.
  The first build can take several minutes. Subsequent changed-source builds
  reuse Docker/Cargo caches; unchanged source reuses its verified image and
  healthy deployment. Host/web source edits do not invalidate controller source.
  Runtime contract changes require this setup before reconnecting agents/GUI.
  Failed build diagnostics stay in private `state/controller-build.log`.

The old `make docker-build`, `docker-push`, `install`, and `cluster-schema`
controller workflow has been removed. `just deploy` now uses the same setup
path as the CLI, including chart/CRD updates and ownership checks. No global
kubeconfig, fixed legacy registry, or manually selected deployment context is used.

Build output, immutable resource snapshots, launchers, and private state live
under ignored `.proofstorm-dev/`. Do not delete it casually: it also identifies
the installation that owns Docker resources. Ordinary builds reuse it. The
old checkout `target/`, web `dist/`, and legacy cluster are not adopted.

An explicit `just dev-build --target-dir /absolute/dedicated/cargo-cache` can select
a different build cache on first registration. Keep it dedicated: replacing
either binary outside `just dev-build` makes registration stale and commands
fail closed until a coherent build is registered. Switching binary paths after
registration is intentionally refused rather than silently retargeting agents.

## Maintainer host tools

`just tools` installs reviewed k3d, kubectl, and Helm executables in `.tools/bin`
for this checkout. It supports macOS Apple Silicon and Linux x86-64. Existing
files must match the pinned executable hashes; conflicting files and symlinks
are refused, not silently adopted or overwritten. Payload and extracted Helm
executable hashes are checked before installation. Exact existing files are reused.

`storm setup` uses the same Rust pin validator but installs into its own private
home. It never adopts `.tools/bin`. `just web-tools` and Rust toolchain setup
remain separate because they install build tools, not runtime helpers.

After deliberately updating `tools/versions.env`, generate candidate pins:

```sh
scratch="$(mktemp -d)"
just tool-pins aarch64-apple-darwin "$scratch/macos.json"
just tool-pins x86_64-unknown-linux-gnu "$scratch/linux.json"
```

This downloads the exact publisher checksum receipts and payloads, verifies them,
and hashes the selected executable without running it. Review both candidates
before replacing `release/bootstrap-tools.json` and
`release/bootstrap-tools-linux-amd64.json`. Output files must be new; the command
never rewrites reviewed pins or installs tools as part of resolution.

For workload image maintenance, use [catalog-image](../docker/README.md#build-or-publish-a-catalog-image).

## Workflow boundaries

Live acceptance now uses owned installations and receipt-checked teardown.
Fixed-cluster setup/image/deletion recipes, raw foreground-server replacement,
static OpenCode profiles, and historical model campaigns have been removed.
Compose stacks, Make dispatch and fixed-container wallet/scenario scripts are
also removed. Ignored `.env`, `.proofstorm-active` and old run output are left
alone; current commands never load them.
Release packaging and installer tests remain separate because they test distribution, not a second
product runtime.

## Moving from Make

The root Makefile has been replaced by `justfile`; use `just dev`, `just check`,
and `just gui`. Arguments are ordinary quoted CLI arguments, not Make assignments:
`just dev-build --target-dir '/absolute/path with spaces'` or `just gui start`.
Named gates use `just e2e slice4` instead of `make e2e-slice4`.
No contributor recipe requires Make or Docker Compose. No runtime is migrated
by this change. Image builds may still need upstream projects' build tools.

## Explicit external runtimes

Normal use selects the installation through its launcher or `--home`. Advanced
external-cluster use must omit `--home` and supply **both** `--context` and
`--kubeconfig` (or `PROOFSTORM_CONTEXT` and `PROOFSTORM_KUBECONFIG`). A context
alone never reads the global kubeconfig. This is not a second alpha setup flow:
the operator is responsible for the external controller and its configuration.
MCP also requires an explicit principal for manual external configuration.
Offline and in-memory MCP test modes remain available and never access a runtime.

## Live verification

The default live test owns its runtime; it never borrows your checkout cluster:

```sh
just e2e smoke          # Also the default for just e2e
just e2e slice4         # Select a larger gate explicitly
just e2e cashu-double-spend  # Spent-proof replay/race against CDK and Nutshell
```

This builds the checkout artifacts, registers them to a fresh installation home,
and runs ordinary `storm setup`. The smoke gate creates one Bitcoin cell through
MCP, reads it using a separate read-only identity, checks that identity cannot
create a cell, confirms the CLI reads the same ready cell, and deletes the cell.
CLI and MCP use the ordinary installation database; workspace/actor grants
isolate test activity. No real model or native agent app is launched.
Other named gates use the same installation-aware runner; their presence in the
list does not mean every gate has passed on every platform. The raw-CRD `slice2`
gate explicitly prefetches catalog images through normal setup.

The proof-spend gate funds 64 regtest sats through Lightning, checks repeated
redemption of one token, then races a second token from two independent wallet
states/processes. It requires one winner, a spent-proof rejection from the
loser, and exact zero-fee balances. The start barrier releases both clients;
it does not prove simultaneous HTTP arrival or exhaustive race coverage.
Balances use the existing passive database observer, not CLI output parsing or
CLI-triggered recovery. Tokens and wallet logs remain in disposable test storage;
the retained receipt contains only amounts, exit codes and rejection flags.
The old quote-flood/SLA experiment is retired; no sustained-load availability
claim is made by this gate.

`just check-cdk-config` is a separate image-only contract check: Docker and jq,
no cluster. It validates generated CDK configs and tests initializer edits,
restart retries and failed-edit recovery without network access in containers.
It may download the pinned public images if they are not cached.

At the end, the runner removes only the recorded runtime containers, network,
and volumes. It compares preexisting Docker resources and configuration before
and after, excluding the shared image cache. Avoid unrelated Docker/configuration
changes while testing: drift is reported as a failure, never silently repaired.
State, logs, and `acceptance.json` stay in the printed private work directory.

If cleanup needs a retry:

```sh
just e2e-cleanup /absolute/path/to/the/printed/run-directory
```

Retirement is permanent for that test home; it is not a cell deletion or a
general-purpose reset of your development installation. Existing installations
without a resource receipt are not adopted for deletion. Ctrl-C waits for the
current bounded setup operation to finish recording resources, then cleans up;
gate workers have a deadline. A forced kill before a resource receipt is written
needs manual inspection of the retained evidence—cleanup must fail closed.

For an unpacked bundle, use the same gates without rebuilding checkout artifacts:

```sh
just e2e-bundle /absolute/path/to/unpacked/bundle onboarding agent-config cli-progress
```

Use `--allow-development` for a development bundle, and `--work-dir` for a new
absolute evidence directory. Neither artifact source selects an existing runtime.

### Targeted checks

```sh
just e2e onboarding agent-config cli-progress
just e2e gui
just e2e installation-isolation
# Only when OpenCode and Claude Code CLIs are already installed:
just e2e agent-clients
```

Onboarding checks helper-only preparation, repeat setup/controller reuse, on-demand
images, CLI/MCP cells, and GUI lifecycle while workloads exist. Run it first in a
fresh test runtime. Attachment checks use private test projects and homes.
`agent-config` needs no agent installations; `agent-clients` adds actual client
MCP discovery without starting a model or approving trust.

The PTY gate tests real setup/GUI progress and human/JSON output. Browser appearance,
folder picking, vendor buttons, and native project handoff require a separate
manual desktop session; the headless GUI gate does not claim to test those.

All these gates now use the owned Rust runner. There is no selected-development-home
Python smoke path. See [check lanes and ownership](CHECKS.md#live-and-manual-checks)
for boundaries and prerequisites.
